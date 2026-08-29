use crate::config::ProcessConfig;
use std::{collections::HashMap, path::Path, process::Stdio, time::Duration};
use tokio::{
    io::AsyncReadExt,
    process::{Child, Command},
    task::JoinHandle,
};

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
            "\n[stdout truncated]"
        } else {
            ""
        };
        let stderr_suffix = if self.stderr_truncated {
            "\n[stderr truncated]"
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

pub fn safe_child_environment(config: &ProcessConfig) -> HashMap<String, String> {
    config
        .child_env_allowlist
        .iter()
        .filter_map(|name| std::env::var(name).ok().map(|value| (name.clone(), value)))
        .collect()
}

pub async fn run_bash(command: &str, directory: &Path, config: &ProcessConfig) -> ProcessOutput {
    let mut process = Command::new("bash");
    process
        .arg("-c")
        .arg(command)
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
    let stdout_task = child
        .stdout
        .take()
        .map(|stdout| tokio::spawn(read_bounded(stdout, stdout_limit)));
    let stderr_task = child
        .stderr
        .take()
        .map(|stderr| tokio::spawn(read_bounded(stderr, stderr_limit)));

    let (code, timed_out) = match tokio::time::timeout(timeout, child.wait()).await {
        Ok(Ok(status)) => (status.code(), false),
        Ok(Err(error)) => {
            return output_from_tasks(
                Some(1),
                false,
                stdout_task,
                stderr_task,
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

    output_from_tasks(code, timed_out, stdout_task, stderr_task, None).await
}

async fn output_from_tasks(
    code: Option<i32>,
    timed_out: bool,
    stdout_task: Option<JoinHandle<(Vec<u8>, bool)>>,
    stderr_task: Option<JoinHandle<(Vec<u8>, bool)>>,
    extra_error: Option<String>,
) -> ProcessOutput {
    let (stdout, stdout_truncated) = join_reader(stdout_task).await;
    let (mut stderr, stderr_truncated) = join_reader(stderr_task).await;
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

async fn join_reader(task: Option<JoinHandle<(Vec<u8>, bool)>>) -> (String, bool) {
    let Some(task) = task else {
        return (String::new(), false);
    };
    match task.await {
        Ok((bytes, truncated)) => (String::from_utf8_lossy(&bytes).into_owned(), truncated),
        Err(error) => (format!("output reader failed: {error}"), false),
    }
}

async fn read_bounded<R>(mut reader: R, limit: usize) -> (Vec<u8>, bool)
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut captured = Vec::with_capacity(limit.min(64 * 1024));
    let mut buffer = [0u8; 8192];
    let mut truncated = false;
    loop {
        match reader.read(&mut buffer).await {
            Ok(0) => break,
            Ok(read) => {
                let remaining = limit.saturating_sub(captured.len());
                let keep = remaining.min(read);
                captured.extend_from_slice(&buffer[..keep]);
                if keep < read {
                    truncated = true;
                }
            }
            Err(error) => {
                let message = format!("\n[output read error: {error}]");
                let remaining = limit.saturating_sub(captured.len());
                captured.extend_from_slice(&message.as_bytes()[..message.len().min(remaining)]);
                break;
            }
        }
    }
    (captured, truncated)
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

#[cfg(not(unix))]
async fn terminate_process_tree(child: &mut Child, _pid: Option<u32>) {
    let _ = child.kill().await;
}

pub async fn spawn_detached(
    program: &str,
    args: &[String],
    directory: Option<&Path>,
    config: &ProcessConfig,
) -> Result<String, String> {
    let mut command = Command::new(program);
    command
        .args(args)
        .env_clear()
        .envs(safe_child_environment(config))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(directory) = directory {
        command.current_dir(directory);
    }
    configure_process_group(&mut command);
    let mut child = command
        .spawn()
        .map_err(|error| format!("failed to start {program}: {error}"))?;
    let pid = child.id();
    tokio::time::sleep(Duration::from_millis(600)).await;
    match child
        .try_wait()
        .map_err(|error| format!("failed to inspect {program}: {error}"))?
    {
        Some(status) if !status.success() => Err(format!("{program} exited with {status}")),
        Some(_) => Ok(format!("started {program}")),
        None => {
            tokio::spawn(async move {
                let _ = child.wait().await;
            });
            Ok(format!(
                "started {program} (pid {})",
                pid.unwrap_or_default()
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{read_bounded, safe_child_environment};
    use crate::config::{ProcessConfig, Profile};
    use std::{collections::HashSet, time::Duration};

    fn config(names: &[&str]) -> ProcessConfig {
        ProcessConfig {
            shell_timeout: Duration::from_secs(1),
            agent_timeout: Duration::from_secs(1),
            browser_timeout: Duration::from_secs(1),
            stdout_limit: 1024,
            stderr_limit: 1024,
            shell_concurrency: 1,
            agent_concurrency: 1,
            browser_concurrency: 1,
            child_env_allowlist: names
                .iter()
                .map(|value| value.to_string())
                .collect::<HashSet<_>>(),
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
    async fn bash_output_is_bounded_during_execution() {
        let root = tempfile::tempdir().unwrap();
        let mut cfg = config(&["PATH", "HOME"]);
        cfg.stdout_limit = 128;
        let output = super::run_bash(r#"python3 -c 'print("x" * 10000)'"#, root.path(), &cfg).await;
        assert_eq!(output.code, Some(0));
        assert!(output.stdout.len() <= 128);
        assert!(output.stdout_truncated);
    }

    #[tokio::test]
    async fn bash_timeout_terminates_process_group() {
        let root = tempfile::tempdir().unwrap();
        let mut cfg = config(&["PATH", "HOME"]);
        cfg.shell_timeout = Duration::from_millis(100);
        let started = std::time::Instant::now();
        let output = super::run_bash("sleep 30 & wait", root.path(), &cfg).await;
        assert!(output.timed_out);
        assert!(started.elapsed() < Duration::from_secs(3));
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
