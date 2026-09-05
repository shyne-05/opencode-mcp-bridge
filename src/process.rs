use crate::config::ProcessConfig;
use std::{
    collections::HashMap,
    future::Future,
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
    limit: usize,
}

impl Capture {
    fn append(&mut self, bytes: &[u8]) {
        let keep = self.limit.saturating_sub(self.bytes.len()).min(bytes.len());
        self.bytes.extend_from_slice(&bytes[..keep]);
        self.truncated |= keep < bytes.len();
    }
}

struct OutputReader(JoinHandle<()>);

impl Drop for OutputReader {
    fn drop(&mut self) {
        // Dropping a JoinHandle normally detaches the task.
        self.0.abort();
    }
}

struct ProcessCleanup {
    #[cfg(unix)]
    group_id: Option<i32>,
}

impl ProcessCleanup {
    fn new(pid: Option<u32>) -> Self {
        #[cfg(unix)]
        {
            Self {
                group_id: pid
                    .and_then(|pid| i32::try_from(pid).ok())
                    .filter(|pid| *pid > 0),
            }
        }
        #[cfg(not(unix))]
        {
            let _ = pid;
            Self {}
        }
    }

    fn disarm(&mut self) {
        #[cfg(unix)]
        {
            self.group_id = None;
        }
    }
}

impl Drop for ProcessCleanup {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(group_id) = self.group_id {
            // Cancellation cannot await asynchronous process-tree cleanup.
            unsafe {
                libc::kill(-group_id, libc::SIGKILL);
            }
        }
    }
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

#[cfg(windows)]
fn windows_cmd_command(command: &str) -> Command {
    let mut process = Command::new("cmd.exe");
    // cmd uses different quoting rules from the C runtime. /s removes only
    // these outer quotes, preserving quoted paths and arguments inside.
    process.args(["/d", "/s", "/c"]);
    process.raw_arg(format!("\"{command}\""));
    process
}

#[cfg(any(windows, test))]
struct PowerShellScript {
    path: std::path::PathBuf,
}

#[cfg(any(windows, test))]
impl PowerShellScript {
    fn create(script: &str) -> Result<Self, String> {
        use std::io::Write;

        let path =
            std::env::temp_dir().join(format!("{}.ps1", crate::util::random_token("mcp-bridge")));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&path)
            .map_err(|error| format!("could not create temporary PowerShell script: {error}"))?;
        let guard = Self { path };
        let written = file
            .write_all(b"\xef\xbb\xbf")
            .and_then(|()| file.write_all(script.as_bytes()));
        // Windows cannot remove an open file if writing fails.
        drop(file);
        written.map_err(|error| format!("could not write temporary PowerShell script: {error}"))?;
        Ok(guard)
    }
}

#[cfg(any(windows, test))]
impl Drop for PowerShellScript {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(any(windows, test))]
fn encode_powershell_script(script: &str) -> String {
    use base64::{Engine, engine::general_purpose::STANDARD};

    STANDARD.encode(
        script
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>(),
    )
}

#[cfg(any(windows, test))]
fn windows_powershell_command(
    command: &str,
) -> Result<(Command, Option<PowerShellScript>), String> {
    // Console output must match the UTF-8 capture used by all platforms.
    let script = format!(
        "[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false); $OutputEncoding = [Console]::OutputEncoding;\n{command}"
    );
    let mut encoded = encode_powershell_script(&script);
    let mut script_file = None;
    // Base64 expands UTF-16 input. Reserve space below Windows' 32,767 UTF-16
    // command-line limit for the executable, switches, quoting, and terminator.
    if encoded.len().saturating_add(1024) > 32_767 {
        let temporary = PowerShellScript::create(&script)?;
        let path = temporary.path.to_string_lossy().replace('\'', "''");
        // Read source into the same command scope, preserving -EncodedCommand
        // behavior without changing the machine's script execution policy.
        encoded = encode_powershell_script(&format!(
            ". ([ScriptBlock]::Create([IO.File]::ReadAllText('{path}')))"
        ));
        script_file = Some(temporary);
    }
    let mut process = Command::new("powershell.exe");
    process.args([
        "-NoLogo",
        "-NoProfile",
        "-NonInteractive",
        "-EncodedCommand",
        &encoded,
    ]);
    Ok((process, script_file))
}

#[cfg(not(windows))]
fn native_shell_command(command: &str) -> Command {
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
    #[cfg(windows)]
    let (mut process, script_file) = if windows_prefers_powershell() {
        match windows_powershell_command(command) {
            Ok(prepared) => prepared,
            Err(error) => return process_error(error, config.stderr_limit),
        }
    } else {
        (windows_cmd_command(command), None)
    };
    #[cfg(not(windows))]
    let mut process = native_shell_command(command);
    process
        .current_dir(directory)
        .env_clear()
        .envs(safe_child_environment(config))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    configure_process_group(&mut process);
    #[cfg(windows)]
    {
        run_command_supervised(
            process,
            config.shell_timeout,
            config.stdout_limit,
            config.stderr_limit,
            script_file,
        )
        .await
    }
    #[cfg(not(windows))]
    {
        run_command(
            process,
            config.shell_timeout,
            config.stdout_limit,
            config.stderr_limit,
        )
        .await
    }
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
    command: Command,
    timeout: Duration,
    stdout_limit: usize,
    stderr_limit: usize,
) -> ProcessOutput {
    #[cfg(windows)]
    {
        run_command_supervised(command, timeout, stdout_limit, stderr_limit, None).await
    }
    #[cfg(not(windows))]
    {
        run_command_inner(
            command,
            timeout,
            stdout_limit,
            stderr_limit,
            std::future::pending(),
        )
        .await
    }
}

#[cfg(any(windows, test))]
async fn run_command_supervised(
    command: Command,
    timeout: Duration,
    stdout_limit: usize,
    stderr_limit: usize,
    script_file: Option<PowerShellScript>,
) -> ProcessOutput {
    let (keep_alive, cancelled) = tokio::sync::oneshot::channel::<()>();
    // On Windows cleanup must run while the root process still exists, so that
    // taskkill can find its descendants. Dropping the caller signals this task.
    let supervisor = tokio::spawn(async move {
        let output = run_command_inner(command, timeout, stdout_limit, stderr_limit, async move {
            let _ = cancelled.await;
        })
        .await;
        drop(script_file);
        output
    });
    let output = supervisor.await;
    drop(keep_alive);
    output.unwrap_or_else(|error| process_error(error.to_string(), stderr_limit))
}

fn process_error(error: String, stderr_limit: usize) -> ProcessOutput {
    let capture = new_capture(stderr_limit);
    append_capture_error(&capture, &error);
    let (stderr, stderr_truncated) = capture_text(&capture);
    ProcessOutput {
        code: Some(1),
        stdout: String::new(),
        stderr,
        stdout_truncated: false,
        stderr_truncated,
        timed_out: false,
    }
}

async fn run_command_inner(
    mut command: Command,
    timeout: Duration,
    stdout_limit: usize,
    stderr_limit: usize,
    cancelled: impl Future<Output = ()>,
) -> ProcessOutput {
    // No tool accepts stdin. Inheriting it could consume the bridge's input.
    command.stdin(Stdio::null()).kill_on_drop(true);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => return process_error(error.to_string(), stderr_limit),
    };

    let pid = child.id();
    let mut cleanup = ProcessCleanup::new(pid);
    let stdout_capture = new_capture(stdout_limit);
    let stderr_capture = new_capture(stderr_limit);
    let stdout_task = child.stdout.take().map(|stdout| {
        let capture = stdout_capture.clone();
        OutputReader(tokio::spawn(read_bounded_into(stdout, capture)))
    });
    let stderr_task = child.stderr.take().map(|stderr| {
        let capture = stderr_capture.clone();
        OutputReader(tokio::spawn(read_bounded_into(stderr, capture)))
    });

    let wait = tokio::select! {
        result = tokio::time::timeout(timeout, child.wait()) => Some(result),
        () = cancelled => None,
    };
    let (code, timed_out) = match wait {
        Some(Ok(Ok(status))) => {
            // Successful completion may intentionally leave detached background work.
            cleanup.disarm();
            (status.code(), false)
        }
        Some(Ok(Err(error))) => {
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
        Some(Err(_)) | None => {
            terminate_process_tree(&mut child, pid).await;
            let _ = child.wait().await;
            cleanup.disarm();
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
    stdout_task: Option<OutputReader>,
    stderr_task: Option<OutputReader>,
    stdout_capture: SharedCapture,
    stderr_capture: SharedCapture,
    extra_error: Option<String>,
) -> ProcessOutput {
    tokio::join!(
        finish_reader(stdout_task, &stdout_capture),
        finish_reader(stderr_task, &stderr_capture),
    );
    if let Some(error) = extra_error {
        append_capture_error(&stderr_capture, &error);
    }
    let (stdout, stdout_truncated) = capture_text(&stdout_capture);
    let (stderr, stderr_truncated) = capture_text(&stderr_capture);
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
        limit,
    }))
}

async fn finish_reader(task: Option<OutputReader>, capture: &SharedCapture) {
    let Some(mut task) = task else {
        return;
    };
    match tokio::time::timeout(OUTPUT_DRAIN_GRACE, &mut task.0).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => append_capture_error(capture, &format!("output reader failed: {error}")),
        Err(_) => {
            task.0.abort();
            let _ = (&mut task.0).await;
            let mut locked = lock_capture(capture);
            locked.truncated = true;
        }
    }
}

fn append_capture_error(capture: &SharedCapture, message: &str) {
    let mut locked = lock_capture(capture);
    if !locked.bytes.is_empty() {
        locked.append(b"\n");
    }
    locked.append(message.as_bytes());
}

fn capture_text(capture: &SharedCapture) -> (String, bool) {
    let locked = lock_capture(capture);
    let mut text = String::from_utf8_lossy(&locked.bytes).into_owned();
    let mut truncated = locked.truncated;
    // Replacement characters can expand invalid UTF-8 beyond the byte budget.
    if text.len() > locked.limit {
        let mut end = locked.limit;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        truncated = true;
    }
    (text, truncated)
}

fn lock_capture(capture: &SharedCapture) -> std::sync::MutexGuard<'_, Capture> {
    capture
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

async fn read_bounded_into<R>(mut reader: R, capture: SharedCapture)
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut buffer = [0u8; 8192];
    let mut truncated = false;
    loop {
        match reader.read(&mut buffer).await {
            Ok(0) => break,
            Ok(read) => {
                if !truncated {
                    let mut locked = lock_capture(&capture);
                    locked.append(&buffer[..read]);
                    truncated = locked.truncated;
                }
                // Keep draining after the limit without taking the capture lock.
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
    read_bounded_into(reader, capture.clone()).await;
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
        let mut terminate = Command::new("taskkill.exe");
        terminate
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        if tokio::time::timeout(Duration::from_secs(2), terminate.status())
            .await
            .is_ok_and(|status| status.is_ok_and(|status| status.success()))
        {
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
    use super::{
        OutputReader, append_capture_error, capture_text, lock_capture, native_shell_name,
        new_capture, read_bounded, run_shell, safe_child_environment,
    };
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
        Duration::from_secs(5)
    }

    #[tokio::test]
    async fn bounded_reader_caps_output() {
        let input = &b"abcdefghij"[..];
        let (output, truncated) = read_bounded(input, 4).await;
        assert_eq!(output, b"abcd");
        assert!(truncated);
    }

    #[tokio::test]
    async fn bounded_reader_handles_exact_and_zero_limits() {
        let (output, truncated) = read_bounded(&b"abcd"[..], 4).await;
        assert_eq!(output, b"abcd");
        assert!(!truncated);
        let (output, truncated) = read_bounded(&b"abcd"[..], 0).await;
        assert!(output.is_empty());
        assert!(truncated);
        let (output, truncated) = read_bounded(&b""[..], 0).await;
        assert!(output.is_empty());
        assert!(!truncated);
    }

    #[test]
    fn diagnostics_respect_capture_byte_limit() {
        let capture = new_capture(6);
        lock_capture(&capture).append(b"abcd");
        append_capture_error(&capture, "reader failed");
        let (output, truncated) = capture_text(&capture);
        assert_eq!(output, "abcd\nr");
        assert!(truncated);
        assert_eq!(lock_capture(&capture).bytes.len(), 6);
    }

    #[test]
    fn lossy_utf8_respects_rendered_byte_limit() {
        let capture = new_capture(4);
        lock_capture(&capture).append(&[0xff, 0xff, 0xff, 0xff]);
        let (output, truncated) = capture_text(&capture);
        assert_eq!(output, "\u{fffd}");
        assert!(output.len() <= 4);
        assert!(truncated);
    }

    #[tokio::test]
    async fn dropping_output_reader_aborts_its_task() {
        let task = tokio::spawn(std::future::pending::<()>());
        let abort = task.abort_handle();
        drop(OutputReader(task));
        tokio::time::timeout(Duration::from_secs(1), async {
            while !abort.is_finished() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("dropped output reader must not remain detached");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancelling_shell_terminates_background_descendants() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().to_path_buf();
        let mut cfg = config(&["PATH", "HOME"]);
        cfg.shell_timeout = Duration::from_secs(5);
        let task = tokio::spawn(async move {
            run_shell(
                "(sleep 1; printf escaped > escaped) & printf ready > ready; wait",
                &directory,
                &cfg,
            )
            .await
        });
        let ready = tokio::time::timeout(Duration::from_secs(3), async {
            while !root.path().join("ready").exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        ready.expect("shell must start before testing cancellation");
        tokio::time::sleep(Duration::from_millis(1250)).await;
        assert!(
            !root.path().join("escaped").exists(),
            "a descendant survived cancellation and continued running",
        );
    }

    #[tokio::test]
    async fn supervised_cancellation_terminates_background_descendants() {
        let root = tempfile::tempdir().unwrap();
        let mut cfg = config(&["PATH", "HOME", "SystemRoot"]);
        cfg.shell_timeout = Duration::from_secs(10);
        #[cfg(windows)]
        let mut command = super::windows_cmd_command(
            r#"start "" /b cmd.exe /d /s /c "ping 127.0.0.1 -n 3 >nul & echo escaped>escaped" & echo ready>ready & ping 127.0.0.1 -n 31 >nul"#,
        );
        #[cfg(not(windows))]
        let mut command = super::native_shell_command(
            "(sleep 1; printf escaped > escaped) & printf ready > ready; wait",
        );
        command
            .current_dir(root.path())
            .env_clear()
            .envs(safe_child_environment(&cfg))
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        super::configure_process_group(&mut command);
        let script_file = super::PowerShellScript::create("# retained by supervisor").unwrap();
        let script_path = script_file.path.clone();
        let task = tokio::spawn(super::run_command_supervised(
            command,
            cfg.shell_timeout,
            cfg.stdout_limit,
            cfg.stderr_limit,
            Some(script_file),
        ));
        let ready = tokio::time::timeout(Duration::from_secs(5), async {
            while !root.path().join("ready").exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        assert!(
            script_path.exists(),
            "active supervisor must retain its script"
        );
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        ready.expect("supervised shell must start before testing cancellation");
        #[cfg(windows)]
        tokio::time::sleep(Duration::from_millis(3500)).await;
        #[cfg(not(windows))]
        tokio::time::sleep(Duration::from_millis(1250)).await;
        assert!(
            !root.path().join("escaped").exists(),
            "supervised cancellation must terminate descendants before their next command",
        );
        assert!(
            !script_path.exists(),
            "cancelled supervisor must remove its script"
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn cmd_preserves_quoted_executable_paths_and_text() {
        let root = tempfile::tempdir().unwrap();
        let executable = root.path().join("command with spaces.exe");
        let system_root = std::env::var_os("SystemRoot").unwrap();
        std::fs::copy(
            std::path::Path::new(&system_root).join("System32/cmd.exe"),
            &executable,
        )
        .unwrap();
        let script = format!(
            r#""{}" /d /s /c "echo quoted value" && echo "two words""#,
            executable.display(),
        );
        let mut command = super::windows_cmd_command(&script);
        command
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let output = super::run_command(command, Duration::from_secs(5), 1024, 1024).await;
        assert!(output.is_success(), "{}", output.render());
        assert_eq!(output.stdout.trim(), "quoted value\r\n\"two words\"");
    }

    #[test]
    fn long_powershell_scripts_use_bounded_arguments_and_utf8_files() {
        let script = format!("Write-Output 'café 雪'; #{}", "x".repeat(15_000));
        let (command, temporary) = super::windows_powershell_command(&script).unwrap();
        let temporary = temporary.expect("long script should avoid the Windows argument limit");
        let path = temporary.path.clone();
        let bytes = std::fs::read(&path).unwrap();
        assert!(
            bytes.starts_with(b"\xef\xbb\xbf"),
            "Windows PowerShell needs a UTF-8 BOM"
        );
        assert!(std::str::from_utf8(&bytes[3..]).unwrap().ends_with(&script));
        let command_line_units = command
            .as_std()
            .get_args()
            .map(|arg| arg.to_string_lossy().encode_utf16().count() + 3)
            .sum::<usize>();
        assert!(command_line_units + 1024 < 32_767);
        drop(temporary);
        assert!(!path.exists(), "finished script must not remain on disk");

        let (_, temporary) = super::windows_powershell_command("Write-Output 'small'").unwrap();
        assert!(
            temporary.is_none(),
            "short commands should not create files"
        );
    }

    #[tokio::test]
    async fn supervised_spawn_failure_removes_temporary_script() {
        let root = tempfile::tempdir().unwrap();
        let script = super::PowerShellScript::create("# temporary source").unwrap();
        let path = script.path.clone();
        let command = tokio::process::Command::new(root.path().join("missing-executable"));
        let output = super::run_command_supervised(
            command,
            Duration::from_secs(1),
            1024,
            1024,
            Some(script),
        )
        .await;
        assert!(!output.is_success());
        assert!(!path.exists(), "failed startup must remove its script");
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn powershell_executes_long_scripts_and_cleans_up() {
        let script = format!(
            "$value = '{}'; Write-Output $value.Length; Write-Output 'café 雪'",
            "x".repeat(15_000),
        );
        let (mut command, temporary) = super::windows_powershell_command(&script).unwrap();
        let path = temporary.as_ref().unwrap().path.clone();
        command
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let output =
            super::run_command_supervised(command, Duration::from_secs(5), 1024, 1024, temporary)
                .await;
        assert!(output.is_success(), "{}", output.render());
        assert_eq!(output.stdout.trim(), "15000\r\ncafé 雪");
        assert!(!path.exists(), "completed long script must be removed");
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn powershell_preserves_nested_quotes_and_unicode() {
        let (mut command, script_file) =
            super::windows_powershell_command(r#"Write-Output 'a "quoted" value: café 雪'"#)
                .unwrap();
        assert!(script_file.is_none());
        command
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let output = super::run_command(command, Duration::from_secs(5), 1024, 1024).await;
        assert!(output.is_success(), "{}", output.render());
        assert_eq!(output.stdout.trim(), "a \"quoted\" value: café 雪");
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
        let command = if super::windows_prefers_powershell() {
            "Write-Output ('x' * 10000)"
        } else {
            "for /L %i in (1,1,10000) do @echo x"
        };
        #[cfg(not(windows))]
        let command = "printf '%010000d' 0";
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
        let command = if super::windows_prefers_powershell() {
            "Start-Sleep -Seconds 30"
        } else {
            "ping 127.0.0.1 -n 31 >nul"
        };
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
