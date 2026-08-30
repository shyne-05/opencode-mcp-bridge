use crate::config::ProcessConfig;
use std::{
    collections::HashMap,
    path::Path,
    process::Stdio,
    sync::{Arc, Mutex as StdMutex},
    time::Duration,
};
use tokio::{
    io::AsyncReadExt,
    process::{Child, Command},
    task::JoinHandle,
};

const OUTPUT_DRAIN_GRACE: Duration = Duration::from_millis(100);

#[derive(Debug)]
pub struct ProcessOutput {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    pub timed_out: bool,
}

impl ProcessOutput {
    pub fn is_success(&self) -> bool {
        self.code == Some(0) && !self.timed_out
    }

    pub fn render(&self) -> String {
        let stdout_suffix = if self.stdout_truncated {
            "\n[stdout truncated or stream left open]"
        } else {
            ""
        };
        let stderr_suffix = if self.stderr_truncated {
            "\n[stderr truncated or stream left open]"
        } else {
            ""
        };
        let timeout = if self.timed_out {
            "\ntimed_out:true"
        } else {
            ""
        };
        format!(
            "exit:{:?}{timeout}\nSTDOUT:\n{}{}\nSTDERR:\n{}{}",
            self.code, self.stdout, stdout_suffix, self.stderr, stderr_suffix
        )
    }
}

#[derive(Default)]
struct Capture {
    bytes: Vec<u8>,
    truncated: bool,
}

type SharedCapture = Arc<StdMutex<Capture>>;

pub fn safe_child_environment(config: &ProcessConfig) -> HashMap<String, String> {
    let environment = config
        .child_env_allowlist
        .iter()
        .filter_map(|name| std::env::var(name).ok().map(|value| (name.clone(), value)))
        .collect::<HashMap<_, _>>();

    #[cfg(windows)]
    {
        let mut environment = environment;
        for name in [
            "SystemRoot",
            "WINDIR",
            "USERPROFILE",
            "APPDATA",
            "LOCALAPPDATA",
            "TEMP",
            "TMP",
            "COMSPEC",
            "PATHEXT",
        ] {
            if let Ok(value) = std::env::var(name) {
                environment.insert(name.to_string(), value);
            }
        }
        environment
    }

    #[cfg(not(windows))]
    {
        environment
    }
}

#[cfg(windows)]
fn windows_prefers_powershell() -> bool {
    std::env::var("MCP_WINDOWS_SHELL")
        .ok()
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("powershell"))
}

pub fn native_shell_name() -> &'static str {
    #[cfg(windows)]
    {
        if windows_prefers_powershell() {
            "PowerShell"
        } else {
            "cmd"
        }
    }
    #[cfg(target_os = "macos")]
    {
        "zsh"
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        "bash"
    }
    #[cfg(not(any(unix, windows)))]
    {
        "shell"
    }
}

fn native_shell_command(command: &str) -> Command {
    #[cfg(windows)]
    {
        if windows_prefers_powershell() {
            let mut process = Command::new("powershell.exe");
            process.args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command"]);
            process.arg(command);
            process
        } else {
            let mut process = Command::new("cmd.exe");
            process.args(["/d", "/s", "/c"]);
            process.arg(command);
            process
        }
    }
    #[cfg(target_os = "macos")]
    {
        let mut process = Command::new("/bin/zsh");
        process.args(["-f", "-c"]).arg(command);
        process
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let mut process = Command::new("bash");
        process.arg("-c").arg(command);
        process
    }
    #[cfg(not(any(unix, windows)))]
    {
        let mut process = Command::new("sh");
        process.arg("-c").arg(command);
        process
    }
}

pub async fn run_shell(command: &str, directory: &Path, config: &ProcessConfig) -> ProcessOutput {
    let mut process = native_shell_command(command);
    process
        .current_dir(directory)
        .env_clear()
        .envs(safe_child_environment(config))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    configure_process_group(&mut process);
    run_command(
        process,
        config.shell_timeout,
        config.stdout_limit,
        config.stderr_limit,
    )
    .await
}

pub async fn run_program(
    program: &str,
    args: &[String],
    directory: Option<&Path>,
    timeout: Duration,
    config: &ProcessConfig,
) -> ProcessOutput {
    run_program_with_env(program, args, directory, timeout, config, &[]).await
}

pub async fn run_program_with_env(
    program: &str,
    args: &[String],
    directory: Option<&Path>,
    timeout: Duration,
    config: &ProcessConfig,
    extra_env: &[(&str, &str)],
) -> ProcessOutput {
    let mut process = Command::new(program);
    process
        .args(args)
        .env_clear()
        .envs(safe_child_environment(config))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (name, value) in extra_env {
        process.env(name, value);
    }
    if let Some(directory) = directory {
        process.current_dir(directory);
    }
    configure_process_group(&mut process);
    run_command(process, timeout, config.stdout_limit, config.stderr_limit).await
}

#[cfg(unix)]
fn configure_process_group(process: &mut Command) {
    process.process_group(0);
}

#[cfg(not(unix))]
fn configure_process_group(_process: &mut Command) {}

async fn run_command(
    mut command: Command,
    timeout: Duration,
    stdout_limit: usize,
    stderr_limit: usize,
) -> ProcessOutput {
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            return ProcessOutput {
                code: Some(1),
                stdout: String::new(),
                stderr: error.to_string(),
                stdout_truncated: false,
                stderr_truncated: false,
                timed_out: false,
            };
        }
    };

    let pid = child.id();
    let stdout_capture = new_capture(stdout_limit);
    let stderr_capture = new_capture(stderr_limit);
    let stdout_task = child.stdout.take().map(|stdout| {
        let capture = stdout_capture.clone();
        tokio::spawn(read_bounded_into(stdout, stdout_limit, capture))
    });
    let stderr_task = child.stderr.take().map(|stderr| {
        let capture = stderr_capture.clone();
        tokio::spawn(read_bounded_into(stderr, stderr_limit, capture))
    });

    let (code, timed_out) = match tokio::time::timeout(timeout, child.wait()).await {
        Ok(Ok(status)) => (status.code(), false),
        Ok(Err(error)) => {
            return output_from_tasks(
                Some(1),
                false,
                stdout_task,
                stderr_task,
                stdout_capture,
                stderr_capture,
                Some(error.to_string()),
            )
            .await;
        }
        Err(_) => {
            terminate_process_tree(&mut child, pid).await;
            let _ = child.wait().await;
            (None, true)
        }
    };

    output_from_tasks(
        code,
        timed_out,
        stdout_task,
        stderr_task,
        stdout_capture,
        stderr_capture,
        None,
    )
    .await
}

async fn output_from_tasks(
    code: Option<i32>,
    timed_out: bool,
    stdout_task: Option<JoinHandle<()>>,
    stderr_task: Option<JoinHandle<()>>,
    stdout_capture: SharedCapture,
    stderr_capture: SharedCapture,
    extra_error: Option<String>,
) -> ProcessOutput {
    finish_reader(stdout_task, &stdout_capture).await;
    finish_reader(stderr_task, &stderr_capture).await;
    let (stdout, stdout_truncated) = capture_text(&stdout_capture);
    let (mut stderr, stderr_truncated) = capture_text(&stderr_capture);
    if let Some(error) = extra_error {
        if !stderr.is_empty() {
            stderr.push('\n');
        }
        stderr.push_str(&error);
    }
    ProcessOutput {
        code,
        stdout,
        stderr,
        stdout_truncated,
        stderr_truncated,
        timed_out,
    }
}

fn new_capture(limit: usize) -> SharedCapture {
    Arc::new(StdMutex::new(Capture {
        bytes: Vec::with_capacity(limit.min(64 * 1024)),
        truncated: false,
    }))
}

async fn finish_reader(task: Option<JoinHandle<()>>, capture: &SharedCapture) {
    let Some(mut task) = task else {
        return;
    };
    match tokio::time::timeout(OUTPUT_DRAIN_GRACE, &mut task).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => append_capture_error(capture, &format!("output reader failed: {error}")),
        Err(_) => {
            task.abort();
            let _ = task.await;
            let mut locked = lock_capture(capture);
            locked.truncated = true;
        }
    }
}

fn append_capture_error(capture: &SharedCapture, message: &str) {
    let mut locked = lock_capture(capture);
    if !locked.bytes.is_empty() {
        locked.bytes.push(b'\n');
    }
    locked.bytes.extend_from_slice(message.as_bytes());
}

fn capture_text(capture: &SharedCapture) -> (String, bool) {
    let locked = lock_capture(capture);
    (
        String::from_utf8_lossy(&locked.bytes).into_owned(),
        locked.truncated,
    )
}

fn lock_capture(capture: &SharedCapture) -> std::sync::MutexGuard<'_, Capture> {
    capture
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

async fn read_bounded_into<R>(mut reader: R, limit: usize, capture: SharedCapture)
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut buffer = [0u8; 8192];
    loop {
        match reader.read(&mut buffer).await {
            Ok(0) => break,
            Ok(read) => {
                let mut locked = lock_capture(&capture);
                let remaining = limit.saturating_sub(locked.bytes.len());
                let keep = remaining.min(read);
                locked.bytes.extend_from_slice(&buffer[..keep]);
                if keep < read {
                    locked.truncated = true;
                }
            }
            Err(error) => {
                append_capture_error(&capture, &format!("[output read error: {error}]"));
                break;
            }
        }
    }
}

#[cfg(test)]
async fn read_bounded<R>(reader: R, limit: usize) -> (Vec<u8>, bool)
where
    R: tokio::io::AsyncRead + Unpin,
{
    let capture = new_capture(limit);
    read_bounded_into(reader, limit, capture.clone()).await;
    let locked = lock_capture(&capture);
    (locked.bytes.clone(), locked.truncated)
}

#[cfg(unix)]
async fn terminate_process_tree(child: &mut Child, pid: Option<u32>) {
    if let Some(pid) = pid {
        let pgid = -(pid as i32);
        unsafe {
            libc::kill(pgid, libc::SIGTERM);
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
        unsafe {
            libc::kill(pgid, libc::SIGKILL);
        }
    } else {
        let _ = child.kill().await;
    }
}

#[cfg(windows)]
async fn terminate_process_tree(child: &mut Child, pid: Option<u32>) {
    if let Some(pid) = pid {
        let status = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;
        if status.is_ok_and(|status| status.success()) {
            return;
        }
    }
    let _ = child.kill().await;
}

#[cfg(not(any(unix, windows)))]
async fn terminate_process_tree(child: &mut Child, _pid: Option<u32>) {
    let _ = child.kill().await;
}

#[cfg(test)]
mod tests {
    use super::{native_shell_name, read_bounded, run_shell, safe_child_environment};
    use crate::config::{ProcessConfig, Profile};
    use std::{collections::HashSet, time::Duration};

    fn config(names: &[&str]) -> ProcessConfig {
        ProcessConfig {
            shell_timeout: Duration::from_secs(1),
            browser_timeout: Duration::from_secs(1),
            stdout_limit: 1024,
            stderr_limit: 1024,
            shell_concurrency: 1,
            browser_concurrency: 1,
            child_env_allowlist: names
                .iter()
                .map(|value| value.to_string())
                .collect::<HashSet<_>>(),
        }
    }

    fn normal_shell_test_timeout() -> Duration {
        #[cfg(windows)]
        {
            Duration::from_secs(5)
        }
        #[cfg(not(windows))]
        {
            Duration::from_secs(5)
        }
    }

    #[tokio::test]
    async fn bounded_reader_caps_output() {
        let input = &b"abcdefghij"[..];
        let (output, truncated) = read_bounded(input, 4).await;
        assert_eq!(output, b"abcd");
        assert!(truncated);
    }

    #[tokio::test]
    async fn native_shell_executes_command() {
        let root = tempfile::tempdir().unwrap();
        let mut cfg = config(&["PATH", "HOME", "SystemRoot"]);
        cfg.shell_timeout = normal_shell_test_timeout();
        let output = run_shell("echo mcp-cross-platform", root.path(), &cfg).await;
        assert!(output.is_success(), "{}", output.render());
        assert!(output.stdout.contains("mcp-cross-platform"));
    }

    #[tokio::test]
    async fn shell_output_is_bounded_during_execution() {
        let root = tempfile::tempdir().unwrap();
        let mut cfg = config(&["PATH", "HOME", "SystemRoot"]);
        cfg.shell_timeout = normal_shell_test_timeout();
        cfg.stdout_limit = 128;
        #[cfg(windows)]
        let command = "for /L %i in (1,1,10000) do @<nul set /p =x";
        #[cfg(not(windows))]
        let command = r#"python3 -c 'print("x" * 10000)'"#;
        let output = run_shell(command, root.path(), &cfg).await;
        assert_eq!(output.code, Some(0), "{}", output.render());
        assert!(output.stdout.len() <= 128);
        assert!(output.stdout_truncated);
    }

    #[tokio::test]
    async fn shell_timeout_terminates_process() {
        let root = tempfile::tempdir().unwrap();
        let mut cfg = config(&["PATH", "HOME", "SystemRoot"]);
        cfg.shell_timeout = Duration::from_millis(100);
        #[cfg(windows)]
        let command = "ping 127.0.0.1 -n 31 >nul";
        #[cfg(not(windows))]
        let command = "sleep 30";
        let started = std::time::Instant::now();
        let output = run_shell(command, root.path(), &cfg).await;
        assert!(output.timed_out);
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn shell_timeout_terminates_process_group() {
        let root = tempfile::tempdir().unwrap();
        let mut cfg = config(&["PATH", "HOME"]);
        cfg.shell_timeout = Duration::from_millis(100);
        let started = std::time::Instant::now();
        let output = run_shell("sleep 30 & wait", root.path(), &cfg).await;
        assert!(output.timed_out);
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn detached_background_child_does_not_hold_tool_response_open() {
        let root = tempfile::tempdir().unwrap();
        let mut cfg = config(&["PATH", "HOME"]);
        cfg.shell_timeout = Duration::from_secs(5);
        let started = std::time::Instant::now();
        let output = run_shell("printf launched; sleep 2 &", root.path(), &cfg).await;
        assert!(output.is_success());
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(output.stdout.contains("launched"));
        assert!(output.stdout_truncated || output.stderr_truncated);
    }

    #[test]
    fn native_shell_matches_platform() {
        #[cfg(windows)]
        assert!(matches!(native_shell_name(), "cmd" | "PowerShell"));
        #[cfg(target_os = "macos")]
        assert_eq!(native_shell_name(), "zsh");
        #[cfg(all(unix, not(target_os = "macos")))]
        assert_eq!(native_shell_name(), "bash");
    }

    #[test]
    fn child_environment_only_contains_allowlisted_names() {
        unsafe {
            std::env::set_var("MCP_TEST_SECRET", "secret");
            std::env::set_var("MCP_TEST_SAFE", "safe");
        }
        let env = safe_child_environment(&config(&["MCP_TEST_SAFE"]));
        assert_eq!(env.get("MCP_TEST_SAFE").map(String::as_str), Some("safe"));
        assert!(!env.contains_key("MCP_TEST_SECRET"));
        let _ = Profile::PersonalDesktop;
    }
}
