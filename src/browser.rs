use crate::{
    backend::read_response_limited,
    process::{run_program, safe_child_environment},
    state::{AppState, BrowserWorker},
    util::{optional_string_arg, required_string_arg, trunc},
};
use serde_json::{Value, json};
use std::{
    process::Stdio,
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::Command,
};
use url::Url;

const CDP: &str = "http://127.0.0.1:9222";
pub const HELPER_PROTOCOL: &str = "mcp-browser-helper/2";
const MAX_WORKER_HANDSHAKE_BYTES: usize = 4 * 1024;
// A 15,000-character helper result needs at most 90,000 JSON-escaped bytes.
const MAX_WORKER_RESPONSE_BYTES: usize = 128 * 1024;
const MAX_BROWSER_RESPONSE_BYTES: usize = 1024 * 1024;

pub async fn warm_browser_worker(state: &AppState) -> Result<(), String> {
    if !state.config.tools.browser {
        return Ok(());
    }
    tokio::time::timeout(state.config.process.browser_timeout, async {
        let mut worker_guard = state.browser_worker.lock().await;
        ensure_browser_worker(state, &mut worker_guard).await
    })
    .await
    .map_err(|_| "browser worker prewarm timed out".to_string())?
}

pub async fn shutdown_browser_worker(state: &AppState) {
    let mut worker_guard = state.browser_worker.lock().await;
    terminate_browser_worker(&mut worker_guard).await;
}

pub async fn run_browser_action(
    state: &AppState,
    action: &str,
    args: &serde_json::Map<String, serde_json::Value>,
) -> Result<String, String> {
    let queued_at = Instant::now();
    let permit = state.browser_slots.acquire("browser").await?;
    let queue_ms = queued_at.elapsed().as_secs_f64() * 1_000.0;
    let started_at = Instant::now();

    // One execution budget covers HTTP calls, worker startup, lock waits, and the action.
    // Admission waiting has its own bounded budget in ToolGate.
    let timeout = state.config.process.browser_timeout;
    let result = tokio::time::timeout(timeout, async {
        let text = match action {
            "tabs" => list_tabs(state).await?,
            "new" => {
                let url = args
                    .get("url")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("about:blank");
                safe_browser_url(url)?;
                let response = state
                    .http
                    .put(format!("{CDP}/json/new?{}", urlencoding::encode(url)))
                    .send()
                    .await
                    .map_err(|error| format!("browser request failed: {error}"))?;
                browser_response_text(response).await?
            }
            "navigate" => {
                let url = args
                    .get("url")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| "url is required".to_string())?;
                safe_browser_url(url)?;
                run_script(
                    state,
                    "navigate",
                    optional_string_arg(args, "targetId"),
                    &[url],
                )
                .await?
            }
            "close" => {
                let target_id = required_string_arg(args, "targetId")?;
                ensure_page_target_exists(state, target_id).await?;
                let response = state
                    .http
                    .get(format!(
                        "{CDP}/json/close/{}",
                        urlencoding::encode(target_id)
                    ))
                    .send()
                    .await
                    .map_err(|error| format!("browser request failed: {error}"))?;
                let body = browser_response_text(response).await?;
                if body.contains("No such target id") {
                    return Err(format!(
                        "failed to close browser target {target_id}: {}",
                        trunc(&body, 1000).trim()
                    ));
                }
                body
            }
            "snapshot" => {
                run_script(
                    state,
                    "snapshot",
                    optional_string_arg(args, "targetId"),
                    &[],
                )
                .await?
            }
            "click" => {
                run_script(
                    state,
                    "click",
                    optional_string_arg(args, "targetId"),
                    &[required_string_arg(args, "selector")?],
                )
                .await?
            }
            "fill" => {
                let selector = required_string_arg(args, "selector")?;
                let value = args
                    .get("value")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default();
                run_script(
                    state,
                    "fill",
                    optional_string_arg(args, "targetId"),
                    &[selector, value],
                )
                .await?
            }
            "evaluate" => {
                run_script(
                    state,
                    "evaluate",
                    optional_string_arg(args, "targetId"),
                    &[required_string_arg(args, "expression")?],
                )
                .await?
            }
            _ => return Err(format!("unknown browser action: {action}")),
        };
        Ok(trunc(&text, 15_000))
    })
    .await
    .unwrap_or_else(|_| {
        Err(format!(
            "browser action {action} timed out after {}s",
            timeout.as_secs_f64()
        ))
    });

    tracing::info!(
        target: "mcp_bridge::latency",
        tool = "browser",
        action,
        queue_ms,
        elapsed_ms = started_at.elapsed().as_secs_f64() * 1_000.0,
        success = result.is_ok(),
        "tool latency"
    );
    drop(permit);
    result
}

async fn fetch_targets(state: &AppState) -> Result<serde_json::Value, String> {
    let response = state
        .http
        .get(format!("{CDP}/json/list"))
        .send()
        .await
        .map_err(|error| format!("browser request failed: {error}"))?;
    let body = browser_response_text(response).await?;
    serde_json::from_str(&body)
        .map_err(|error| format!("browser response was not valid JSON: {error}"))
}

async fn browser_response_text(response: reqwest::Response) -> Result<String, String> {
    let (status, bytes, oversized) =
        read_response_limited(response, MAX_BROWSER_RESPONSE_BYTES).await?;
    if !status.is_success() {
        return Err(format!(
            "browser returned status {status}: {}",
            trunc(&String::from_utf8_lossy(&bytes), 1000).trim()
        ));
    }
    if oversized {
        return Err(format!(
            "browser response exceeded {MAX_BROWSER_RESPONSE_BYTES} bytes"
        ));
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

async fn list_tabs(state: &AppState) -> Result<String, String> {
    page_targets(fetch_targets(state).await?)
}

async fn ensure_page_target_exists(state: &AppState, target_id: &str) -> Result<(), String> {
    if page_target_exists(&fetch_targets(state).await?, target_id)? {
        Ok(())
    } else {
        Err(format!("browser page target not found: {target_id}"))
    }
}

fn page_target_exists(value: &serde_json::Value, target_id: &str) -> Result<bool, String> {
    let serde_json::Value::Array(items) = value else {
        return Err("browser target list was not an array".to_string());
    };
    Ok(items.iter().any(|item| {
        item.get("type").and_then(serde_json::Value::as_str) == Some("page")
            && item.get("id").and_then(serde_json::Value::as_str) == Some(target_id)
    }))
}

fn page_targets(value: serde_json::Value) -> Result<String, String> {
    let serde_json::Value::Array(items) = value else {
        return Err("browser target list was not an array".to_string());
    };
    let tabs = items
        .into_iter()
        .filter(|item| item.get("type").and_then(serde_json::Value::as_str) == Some("page"))
        .map(|item| {
            serde_json::json!({
                "id": item.get("id").cloned().unwrap_or(serde_json::Value::Null),
                "title": item.get("title").cloned().unwrap_or(serde_json::Value::Null),
                "url": item.get("url").cloned().unwrap_or(serde_json::Value::Null),
            })
        })
        .collect::<Vec<_>>();
    serde_json::to_string_pretty(&serde_json::json!({
        "count": tabs.len(),
        "tabs": tabs,
    }))
    .map_err(|error| format!("failed to encode browser tabs: {error}"))
}

async fn run_script(
    state: &AppState,
    action: &str,
    target_id: Option<&str>,
    args: &[&str],
) -> Result<String, String> {
    if !state.config.browser_script.is_file() {
        return Err(format!(
            "browser script not found: {}",
            state.config.browser_script.display()
        ));
    }

    let mut worker_guard = state.browser_worker.lock().await;
    ensure_browser_worker(state, &mut worker_guard).await?;

    run_worker_request(
        &mut worker_guard,
        state.config.process.browser_timeout,
        action,
        target_id,
        args,
    )
    .await
}

async fn run_worker_request(
    worker_slot: &mut Option<BrowserWorker>,
    timeout: Duration,
    action: &str,
    target_id: Option<&str>,
    args: &[&str],
) -> Result<String, String> {
    // Keep the slot empty until the response is fully consumed and validated.
    // Cancellation drops this owned worker (kill_on_drop), so the next request
    // starts fresh instead of reading a response from the canceled operation.
    let mut in_flight = worker_slot.take();
    let worker = in_flight
        .as_mut()
        .ok_or_else(|| "browser worker was not initialized".to_string())?;
    let request_id = worker.next_id;
    worker.next_id = worker.next_id.wrapping_add(1).max(1);
    let request = json!({
        "id": request_id,
        "action": action,
        "targetId": target_id.unwrap_or_default(),
        "args": args,
    });
    let response = tokio::time::timeout(timeout, browser_worker_round_trip(worker, &request)).await;

    let response = match response {
        Err(_) => {
            terminate_browser_worker(&mut in_flight).await;
            return Err(format!(
                "browser worker timed out after {}s",
                timeout.as_secs()
            ));
        }
        Ok(Err(error)) => {
            terminate_browser_worker(&mut in_flight).await;
            return Err(error);
        }
        Ok(Ok(response)) => response,
    };

    if response.get("id").and_then(Value::as_u64) != Some(request_id) {
        terminate_browser_worker(&mut in_flight).await;
        return Err("browser worker response was out of sequence".to_string());
    }
    let result = match response.get("ok").and_then(Value::as_bool) {
        Some(true) => response
            .get("result")
            .and_then(Value::as_str)
            .map(|value| Ok(value.to_string())),
        Some(false) => response
            .get("error")
            .and_then(Value::as_str)
            .map(|value| Err(value.to_string())),
        None => None,
    };
    match result {
        Some(result) => {
            *worker_slot = in_flight;
            result
        }
        None => {
            terminate_browser_worker(&mut in_flight).await;
            Err("browser worker response was malformed".to_string())
        }
    }
}

async fn ensure_browser_worker(
    state: &AppState,
    worker_slot: &mut Option<BrowserWorker>,
) -> Result<(), String> {
    let needs_spawn = match worker_slot.as_mut() {
        Some(worker) => match worker.child.try_wait() {
            Ok(None) => false,
            Ok(Some(_)) | Err(_) => true,
        },
        None => true,
    };
    if needs_spawn {
        *worker_slot = Some(spawn_browser_worker(state).await?);
    }
    Ok(())
}

async fn spawn_browser_worker(state: &AppState) -> Result<BrowserWorker, String> {
    let node_path = discover_node_path(state).await;
    let mut command = Command::new("node");
    command
        .arg(state.config.browser_script.to_string_lossy().into_owned())
        .arg("serve")
        .env_clear()
        .envs(safe_child_environment(&state.config.process))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    if let Some(node_path) = node_path.as_deref() {
        command.env("NODE_PATH", node_path);
    }
    configure_worker_process_group(&mut command);

    let mut child = command
        .spawn()
        .map_err(|error| format!("failed to start persistent browser worker: {error}"))?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| "browser worker stdin is unavailable".to_string())?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "browser worker stdout is unavailable".to_string())?;
    let mut worker = BrowserWorker {
        child,
        stdin,
        stdout: BufReader::new(stdout),
        next_id: 1,
    };

    let ready = tokio::time::timeout(
        state.config.process.browser_timeout,
        read_worker_message(&mut worker.stdout, MAX_WORKER_HANDSHAKE_BYTES, "handshake"),
    )
    .await
    .map_err(|_| "browser worker did not become ready before timeout".to_string())??;
    if ready.get("type").and_then(Value::as_str) != Some("ready")
        || ready.get("protocol").and_then(Value::as_str) != Some(HELPER_PROTOCOL)
    {
        return Err(format!(
            "browser helper protocol mismatch: bridge expects {HELPER_PROTOCOL}"
        ));
    }

    tracing::info!(
        target: "mcp_bridge::latency",
        protocol = HELPER_PROTOCOL,
        "persistent browser worker ready"
    );
    Ok(worker)
}

async fn browser_worker_round_trip(
    worker: &mut BrowserWorker,
    request: &Value,
) -> Result<Value, String> {
    let mut encoded = serde_json::to_vec(request)
        .map_err(|error| format!("failed to encode browser worker request: {error}"))?;
    encoded.push(b'\n');
    worker
        .stdin
        .write_all(&encoded)
        .await
        .map_err(|error| format!("failed to write browser worker request: {error}"))?;
    worker
        .stdin
        .flush()
        .await
        .map_err(|error| format!("failed to flush browser worker request: {error}"))?;

    read_worker_message(&mut worker.stdout, MAX_WORKER_RESPONSE_BYTES, "response").await
}

async fn read_worker_message<R>(
    reader: &mut R,
    max_bytes: usize,
    message_kind: &str,
) -> Result<Value, String>
where
    R: AsyncBufRead + Unpin,
{
    // Limit before reading, including unterminated output from a broken helper.
    let mut line = Vec::new();
    reader
        .take((max_bytes + 1) as u64)
        .read_until(b'\n', &mut line)
        .await
        .map_err(|error| format!("failed to read browser worker {message_kind}: {error}"))?;
    if line.len() > max_bytes {
        return Err(format!(
            "browser worker {message_kind} exceeded {max_bytes} bytes"
        ));
    }
    if line.is_empty() {
        return Err(format!("browser worker exited before {message_kind}"));
    }
    if !line.ends_with(b"\n") {
        return Err(format!("browser worker {message_kind} was incomplete"));
    }
    serde_json::from_slice(&line)
        .map_err(|error| format!("browser worker {message_kind} was invalid JSON: {error}"))
}

async fn terminate_browser_worker(worker_guard: &mut Option<BrowserWorker>) {
    if let Some(mut worker) = worker_guard.take() {
        let _ = worker.child.kill().await;
        let _ = worker.child.wait().await;
    }
}

#[cfg(unix)]
fn configure_worker_process_group(command: &mut Command) {
    command.process_group(0);
}

#[cfg(not(unix))]
fn configure_worker_process_group(_command: &mut Command) {}

async fn discover_node_path(state: &AppState) -> Option<String> {
    state
        .node_path
        .get_or_init(|| async {
            if let Some(path) = state.config.node_path.clone() {
                return Some(path);
            }
            let output = run_program(
                if cfg!(windows) { "npm.cmd" } else { "npm" },
                &["root".to_string(), "-g".to_string()],
                None,
                state.config.process.browser_timeout,
                &state.config.process,
            )
            .await;
            (output.code == Some(0) && !output.timed_out)
                .then(|| output.stdout.trim().to_string())
                .filter(|value| !value.is_empty())
        })
        .await
        .clone()
}

pub fn safe_browser_url(url: &str) -> Result<&str, String> {
    if url == "about:blank" {
        return Ok(url);
    }
    let parsed = Url::parse(url)
        .map_err(|_| "browser URL must be a valid http:// or https:// URL".to_string())?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err("browser URLs must use http://, https://, or about:blank".to_string());
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("browser URLs with username/password userinfo are not allowed".to_string());
    }
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::{page_target_exists, page_targets, read_worker_message, safe_browser_url};
    use serde_json::json;
    use tokio::io::BufReader;

    fn test_state(timeout: std::time::Duration) -> crate::state::AppState {
        use crate::config::{Config, OAuthConfig, ProcessConfig, Profile, ToolConfig, TrustProxy};
        use std::{path::PathBuf, time::Duration};

        let workdir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        crate::state::AppState::new(Config {
            profile: Profile::PersonalDesktop,
            host: "127.0.0.1".into(),
            port: 0,
            browser_script: workdir.join("scripts/browser.cjs"),
            workdir,
            backend_url: "http://127.0.0.1:9".into(),
            backend_response_limit: 1024,
            max_sessions_per_principal: 1,
            node_path: None,
            trust_proxy: TrustProxy::None,
            state_file: None,
            tokens: vec![],
            allow_unauthenticated: true,
            tools: ToolConfig {
                shell: false,
                browser: true,
            },
            process: ProcessConfig {
                shell_timeout: timeout,
                browser_timeout: timeout,
                stdout_limit: 4096,
                stderr_limit: 4096,
                shell_concurrency: 1,
                browser_concurrency: 1,
                child_env_allowlist: Default::default(),
            },
            oauth: OAuthConfig {
                public_url: None,
                username: String::new(),
                password: None,
                access_token_ttl: 60,
                refresh_token_ttl: 60,
                code_ttl: 60,
                max_failed_logins: 1,
                max_login_buckets: 1,
                max_authorization_codes: 1,
                max_clients: 1,
                max_access_tokens: 1,
                max_refresh_tokens: 1,
                dcr_client_ttl: 60,
                client_metadata_timeout: Duration::from_secs(1),
                client_metadata_max_bytes: 1024,
                client_metadata_cache_ttl: 60,
                login_window: Duration::from_secs(60),
            },
        })
        .unwrap()
    }

    #[tokio::test]
    async fn browser_action_deadline_includes_worker_lock_wait() {
        use std::time::Duration;
        let state = test_state(Duration::from_millis(50));
        let guard = state.browser_worker.lock().await;
        let error = tokio::time::timeout(
            Duration::from_secs(2),
            super::run_browser_action(&state, "snapshot", &Default::default()),
        )
        .await
        .expect("the action deadline must cover waiting for the worker")
        .unwrap_err();
        assert!(
            error.contains("browser action snapshot timed out"),
            "{error}"
        );
        assert!(guard.is_none());
        drop(guard);
        assert!(state.browser_slots.acquire("browser").await.is_ok());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn browser_action_deadline_discards_stalled_worker() {
        use std::time::Duration;
        use tokio::io::AsyncReadExt;
        let state = test_state(Duration::from_millis(50));
        let (worker, mut stderr) = synthetic_worker("IFS= read -r request; IFS= read -r ignored");
        *state.browser_worker.lock().await = Some(worker);
        let error = tokio::time::timeout(
            Duration::from_secs(2),
            super::run_browser_action(&state, "snapshot", &Default::default()),
        )
        .await
        .expect("a stalled worker must respect the action deadline")
        .unwrap_err();
        assert!(error.contains("timed out"), "{error}");
        assert!(state.browser_worker.lock().await.is_none());
        tokio::time::timeout(Duration::from_secs(2), stderr.read_to_end(&mut Vec::new()))
            .await
            .expect("the timed out worker must exit")
            .unwrap();
    }

    fn test_response(body: impl Into<Vec<u8>>, status: u16) -> reqwest::Response {
        axum::http::Response::builder()
            .status(status)
            .body(body.into())
            .unwrap()
            .into()
    }

    #[tokio::test]
    async fn browser_http_errors_are_reported_with_bounded_details() {
        let error = super::browser_response_text(test_response(vec![b'x'; 10_000], 503))
            .await
            .unwrap_err();
        assert!(error.contains("503"), "{error}");
        assert!(error.len() < 1200, "HTTP errors must not flood tool output");
        assert_eq!(
            super::browser_response_text(test_response("héllo 🌍", 200))
                .await
                .unwrap(),
            "héllo 🌍",
        );
    }

    #[tokio::test]
    async fn browser_http_body_limit_rejects_oversized_success_responses() {
        let limit = super::MAX_BROWSER_RESPONSE_BYTES;
        assert_eq!(
            super::browser_response_text(test_response(vec![b'x'; limit], 200))
                .await
                .unwrap()
                .len(),
            limit,
        );
        let error = super::browser_response_text(test_response(vec![b'x'; limit + 1], 200))
            .await
            .unwrap_err();
        assert!(error.contains("exceeded"), "{error}");
    }

    #[cfg(unix)]
    fn synthetic_worker(
        script: &str,
    ) -> (super::BrowserWorker, BufReader<tokio::process::ChildStderr>) {
        let mut child = tokio::process::Command::new("sh")
            .args(["-c", script])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let stderr = BufReader::new(child.stderr.take().unwrap());
        (
            super::BrowserWorker {
                child,
                stdin,
                stdout,
                next_id: 1,
            },
            stderr,
        )
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancellation_discards_in_flight_worker() {
        use std::time::Duration;
        use tokio::io::{AsyncBufReadExt, AsyncReadExt};

        let (worker, mut stderr) = synthetic_worker(
            "IFS= read -r request; printf 'received\\n' >&2; IFS= read -r ignored",
        );
        let mut slot = Some(worker);
        let mut request = Box::pin(super::run_worker_request(
            &mut slot,
            Duration::from_secs(30),
            "snapshot",
            None,
            &[],
        ));
        let mut acknowledged = String::new();
        tokio::time::timeout(Duration::from_secs(5), async {
            tokio::select! {
                result = &mut request => panic!("request completed before cancellation: {result:?}"),
                read = stderr.read_line(&mut acknowledged) => { read.unwrap(); }
            }
        }).await.unwrap();
        assert_eq!(
            acknowledged, "received\n",
            "worker must receive the request before cancellation"
        );
        drop(request);
        assert!(
            slot.is_none(),
            "canceled worker must never return to the shared slot"
        );
        tokio::time::timeout(Duration::from_secs(5), stderr.read_to_end(&mut Vec::new()))
            .await
            .unwrap()
            .unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn complete_success_and_action_error_keep_worker_reusable() {
        use std::time::Duration;
        let (worker, _) = synthetic_worker(
            r#"
            IFS= read -r request
            printf '{"id":1,"ok":true,"result":"first"}\n'
            IFS= read -r request
            printf '{"id":2,"ok":false,"error":"selector missing"}\n'
            IFS= read -r request
            printf '{"id":3,"ok":true,"result":"third"}\n'
            IFS= read -r ignored
        "#,
        );
        let pid = worker.child.id();
        let mut slot = Some(worker);
        for expected in [Ok("first"), Err("selector missing"), Ok("third")] {
            let response =
                super::run_worker_request(&mut slot, Duration::from_secs(5), "snapshot", None, &[])
                    .await;
            assert_eq!(response.as_deref().map_err(String::as_str), expected);
            assert_eq!(slot.as_ref().unwrap().child.id(), pid);
        }
        super::terminate_browser_worker(&mut slot).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn protocol_errors_discard_worker() {
        use std::time::Duration;
        for (reply, expected) in [
            (r#"{"id":2,"ok":true,"result":"late"}"#, "out of sequence"),
            (r#"{"id":1,"ok":true}"#, "malformed"),
            (r#"{"id":1,"ok":false}"#, "malformed"),
            (r#"{"id":1,"ok":"true","result":"invalid"}"#, "malformed"),
            ("invalid JSON", "invalid JSON"),
        ] {
            let script =
                format!("IFS= read -r request; printf '%s\\n' '{reply}'; IFS= read -r ignored");
            let (worker, _) = synthetic_worker(&script);
            let mut slot = Some(worker);
            let error =
                super::run_worker_request(&mut slot, Duration::from_secs(5), "snapshot", None, &[])
                    .await
                    .unwrap_err();
            assert!(error.contains(expected), "{error}");
            assert!(slot.is_none());
        }
    }

    #[tokio::test]
    async fn worker_frames_preserve_message_boundaries_and_unicode() {
        let input = "{\"result\":\"héllo 🌍\"}\n{\"id\":2}\n".as_bytes();
        let mut reader = BufReader::new(input);
        let first = read_worker_message(&mut reader, 64, "response")
            .await
            .unwrap();
        let second = read_worker_message(&mut reader, 64, "response")
            .await
            .unwrap();
        assert_eq!(first["result"], "héllo 🌍");
        assert_eq!(second["id"], 2);
    }

    #[tokio::test]
    async fn worker_frame_limit_applies_before_newline_or_eof() {
        let input = b"{\"id\":1}\n";
        let mut reader = BufReader::new(&input[..]);
        assert!(
            read_worker_message(&mut reader, input.len(), "response")
                .await
                .is_ok()
        );

        let input = vec![b'x'; 4096];
        let mut reader = BufReader::new(&input[..]);
        let error = read_worker_message(&mut reader, 32, "response")
            .await
            .unwrap_err();
        assert!(error.contains("exceeded 32 bytes"));
        assert_eq!(reader.buffer().len(), input.len() - 33);
    }

    #[tokio::test]
    async fn worker_rejects_closed_incomplete_and_invalid_frames() {
        for (input, expected) in [
            ("", "exited before response"),
            ("{\"id\":1}", "was incomplete"),
            ("not JSON\n", "was invalid JSON"),
        ] {
            let mut reader = BufReader::new(input.as_bytes());
            let error = read_worker_message(&mut reader, 64, "response")
                .await
                .unwrap_err();
            assert!(error.contains(expected), "{error}");
        }
    }

    #[test]
    fn tabs_only_include_page_targets() {
        let rendered = page_targets(json!([
            {"type":"page","id":"p1","title":"One","url":"https://one.example"},
            {"type":"iframe","id":"f1","title":"Frame","url":"https://frame.example"},
            {"type":"worker","id":"w1","title":"Worker","url":"https://worker.example"},
            {"type":"page","id":"p2","title":"Two","url":"https://two.example"}
        ]))
        .unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&rendered).unwrap();
        assert_eq!(parsed["count"], 2);
        assert_eq!(parsed["tabs"][0]["id"], "p1");
        assert_eq!(parsed["tabs"][1]["id"], "p2");
    }

    #[test]
    fn missing_browser_page_target_is_detected() {
        let targets = json!([
            {"type":"page","id":"real"},
            {"type":"iframe","id":"frame"}
        ]);
        assert!(page_target_exists(&targets, "real").unwrap());
        assert!(!page_target_exists(&targets, "missing").unwrap());
        assert!(!page_target_exists(&targets, "frame").unwrap());
    }

    #[test]
    fn only_allows_safe_browser_urls() {
        assert!(safe_browser_url("about:blank").is_ok());
        assert!(safe_browser_url("https://example.com").is_ok());
        assert!(safe_browser_url("http://127.0.0.1:4097/health").is_ok());
        assert!(safe_browser_url("https://").is_err());
        assert!(safe_browser_url("https://user:pass@example.com").is_err());
        assert!(safe_browser_url("file:///etc/passwd").is_err());
        assert!(safe_browser_url("javascript:alert(1)").is_err());
    }
}
