mod common;

use axum::{
    Json, Router,
    extract::Query,
    http::StatusCode,
    routing::{get, post},
};
use common::{free_port, spawn_bridge, spawn_bridge_at_port};
use reqwest::{Client, header};
use serde_json::{Value, json};
use std::{collections::HashMap, path::PathBuf, process::Command, time::Duration};
use tempfile::tempdir;

const TOKEN: &str = "regression-test-token";
const USERNAME: &str = "restart-user";
const PASSWORD: &str = "restart-password-123";
const REDIRECT_URI: &str = "https://chatgpt.com/connector_platform_oauth_redirect";
const VERIFIER: &str = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-._~";

fn tool_text(value: &Value) -> String {
    value["result"]["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| item["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

async fn tool_call(base: &str, token: &str, name: &str, arguments: Value) -> (StatusCode, Value) {
    let response = Client::new()
        .post(format!("{base}/mcp"))
        .bearer_auth(token)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json, text/event-stream")
        .header("MCP-Protocol-Version", "2026-07-28")
        .header("Mcp-Method", "tools/call")
        .header("Mcp-Name", name)
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 99,
            "method": "tools/call",
            "params": {
                "name": name,
                "arguments": arguments,
                "_meta": {
                    "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                    "io.modelcontextprotocol/clientCapabilities": {}
                }
            }
        }))
        .send()
        .await
        .expect("MCP tool request should complete");
    let status = response.status();
    let body = response.json::<Value>().await.unwrap_or(Value::Null);
    (status, body)
}

#[tokio::test]
async fn unauthenticated_mode_rejects_non_loopback_listener() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let status = Command::new(env!("CARGO_BIN_EXE_mcp-bridge"))
        .env("MCP_PROFILE", "personal-desktop")
        .env("MCP_ALLOW_UNAUTHENTICATED", "true")
        .env("MCP_ENABLE_HOST_TOOLS", "true")
        .env("MCP_HOST", "0.0.0.0")
        .env("MCP_PORT", free_port().to_string())
        .env("MCP_STATE_FILE", ":memory:")
        .env("BRIDGE_WORKDIR", manifest)
        .status()
        .expect("bridge process should execute");
    assert!(!status.success());
}

#[tokio::test]
async fn mcp_request_body_limit_returns_413() {
    let bridge = spawn_bridge(|command, _| {
        command.env("MCP_TOKEN", TOKEN);
    })
    .await;
    let huge = "x".repeat(1_048_576 + 4096);
    let response = Client::new()
        .post(format!("{}/mcp", bridge.base_url))
        .bearer_auth(TOKEN)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json, text/event-stream")
        .header("MCP-Protocol-Version", "2026-07-28")
        .header("Mcp-Method", "tools/call")
        .header("Mcp-Name", "bridge_search")
        .json(&json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"bridge_search","arguments":{"pattern":huge},"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}}}))
        .send().await.expect("oversized request should complete");
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn readiness_is_unhealthy_when_backend_is_down() {
    let bridge = spawn_bridge(|command, _| {
        command.env("MCP_TOKEN", TOKEN);
    })
    .await;
    let client = Client::new();
    assert_eq!(
        client
            .get(format!("{}/live", bridge.base_url))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        client
            .get(format!("{}/ready", bridge.base_url))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(
        client
            .get(format!("{}/health", bridge.base_url))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
}

#[tokio::test]
async fn shell_and_agent_failures_are_tool_errors() {
    let bridge = spawn_bridge(|command, _| {
        command
            .env("MCP_PROFILE", "personal-desktop")
            .env("MCP_TOKEN", TOKEN)
            .env("MCP_ENABLE_SHELL", "true")
            .env("MCP_SHELL_TIMEOUT_SECONDS", "1")
            .env("MCP_ENABLE_AGENT", "true")
            .env("MCP_AGENT_COMMAND", "/bin/false")
            .env("MCP_AGENT_KIND", "codex");
    })
    .await;

    let (_, nonzero) = tool_call(
        &bridge.base_url,
        TOKEN,
        "shell",
        json!({"command":"exit 7"}),
    )
    .await;
    assert_eq!(nonzero["result"]["isError"], true);
    let (_, timeout) = tool_call(
        &bridge.base_url,
        TOKEN,
        "shell",
        json!({"command":"sleep 2"}),
    )
    .await;
    assert_eq!(timeout["result"]["isError"], true);
    let (_, agent) = tool_call(
        &bridge.base_url,
        TOKEN,
        "bridge_agent_prompt",
        json!({"prompt":"hello"}),
    )
    .await;
    assert_eq!(agent["result"]["isError"], true);
}

async fn spawn_fake_backend(
    workdir: PathBuf,
    outside: PathBuf,
) -> (String, tokio::task::JoinHandle<()>) {
    let app = Router::new()
        .route("/global/health", get(|| async { Json(json!({"ok":true})) }))
        .route("/find", get(move |Query(query): Query<HashMap<String,String>>| {
            let workdir = workdir.clone();
            let outside = outside.clone();
            async move {
                assert_eq!(query.get("directory"), Some(&workdir.to_string_lossy().into_owned()));
                Json(json!([
                    {"path":{"text":"inside.txt"},"lines":{"text":"INSIDE_MARKER"},"line_number":1,"absolute_offset":0,"submatches":[]},
                    {"path":{"text":outside.to_string_lossy()},"lines":{"text":"OUTSIDE_MARKER"},"line_number":1,"absolute_offset":0,"submatches":[]}
                ]))
            }
        }))
        .route("/session", post(|| async { Json(json!({"id":"ses_restart"})) }).get(|| async { Json(json!([])) }))
        .route("/session/{id}/prompt_async", post(|| async { StatusCode::NO_CONTENT }))
        .route("/session/status", get(|| async { Json(json!({"ses_restart":{"type":"busy"}})) }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{address}"), task)
}

#[tokio::test]
async fn search_and_session_ownership_respect_boundaries_across_restart() {
    let work = tempdir().unwrap();
    let outside_dir = tempdir().unwrap();
    std::fs::write(work.path().join("inside.txt"), "INSIDE_MARKER").unwrap();
    let outside = outside_dir.path().join("outside.txt");
    std::fs::write(&outside, "OUTSIDE_MARKER").unwrap();
    let (backend, backend_task) =
        spawn_fake_backend(work.path().to_path_buf(), outside.clone()).await;
    let state_dir = tempdir().unwrap();
    let state_file = state_dir.path().join("state.json");
    let port = free_port();

    let configure = |command: &mut Command, _: u16| {
        command
            .env("MCP_TOKEN", TOKEN)
            .env("BRIDGE_WORKDIR", work.path())
            .env("BRIDGE_BACKEND_URL", &backend)
            .env("MCP_STATE_FILE", &state_file);
    };
    let bridge = spawn_bridge_at_port(port, configure).await;
    let (_, search) = tool_call(
        &bridge.base_url,
        TOKEN,
        "bridge_search",
        json!({"pattern":"MARKER"}),
    )
    .await;
    let text = tool_text(&search);
    assert!(text.contains("INSIDE_MARKER"));
    assert!(!text.contains("OUTSIDE_MARKER"));

    let (_, created) = tool_call(
        &bridge.base_url,
        TOKEN,
        "bridge_prompt_async",
        json!({"prompt":"hello"}),
    )
    .await;
    assert_eq!(created["result"]["isError"], false);
    drop(bridge);
    tokio::time::sleep(Duration::from_millis(100)).await;

    let bridge = spawn_bridge_at_port(port, configure).await;
    let (_, status) = tool_call(
        &bridge.base_url,
        TOKEN,
        "bridge_session_status",
        json!({"sessionId":"ses_restart"}),
    )
    .await;
    assert_eq!(status["result"]["isError"], false);
    assert!(tool_text(&status).contains("busy"));
    drop(bridge);
    backend_task.abort();
}

fn pkce_challenge() -> String {
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    use sha2::{Digest, Sha256};
    URL_SAFE_NO_PAD.encode(Sha256::digest(VERIFIER.as_bytes()))
}

fn code_from_location(location: &str) -> String {
    location
        .split_once('?')
        .unwrap()
        .1
        .split('&')
        .find_map(|pair| pair.strip_prefix("code="))
        .map(|value| urlencoding::decode(value).unwrap().into_owned())
        .unwrap()
}

#[tokio::test]
async fn oauth_refresh_token_survives_bridge_restart() {
    let state_dir = tempdir().unwrap();
    let state_file = state_dir.path().join("state.json");
    let port = free_port();
    let resource = format!("http://127.0.0.1:{port}/mcp");
    let configure = |command: &mut Command, port: u16| {
        command
            .env("MCP_PROFILE", "server-secure")
            .env("MCP_PUBLIC_URL", format!("http://127.0.0.1:{port}"))
            .env("MCP_OAUTH_ALLOW_INSECURE_HTTP", "true")
            .env("MCP_OAUTH_USERNAME", USERNAME)
            .env("MCP_OAUTH_PASSWORD", PASSWORD)
            .env("MCP_STATE_FILE", &state_file);
    };
    let bridge = spawn_bridge_at_port(port, configure).await;
    let client = Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let registration: Value = client.post(format!("{}/oauth/register", bridge.base_url)).json(&json!({
        "client_name":"restart test","redirect_uris":[REDIRECT_URI],"grant_types":["authorization_code","refresh_token"],"response_types":["code"],"token_endpoint_auth_method":"none"
    })).send().await.unwrap().json().await.unwrap();
    let client_id = registration["client_id"].as_str().unwrap().to_string();
    let challenge = pkce_challenge();
    let approved = client
        .post(format!("{}/oauth/authorize", bridge.base_url))
        .form(&[
            ("response_type", "code"),
            ("client_id", client_id.as_str()),
            ("redirect_uri", REDIRECT_URI),
            ("state", "restart"),
            ("code_challenge", challenge.as_str()),
            ("code_challenge_method", "S256"),
            ("resource", resource.as_str()),
            ("scope", "mcp:tools offline_access"),
            ("username", USERNAME),
            ("password", PASSWORD),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(approved.status(), StatusCode::FOUND);
    let code = code_from_location(
        approved
            .headers()
            .get(header::LOCATION)
            .unwrap()
            .to_str()
            .unwrap(),
    );
    let token: Value = client
        .post(format!("{}/oauth/token", bridge.base_url))
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code.as_str()),
            ("redirect_uri", REDIRECT_URI),
            ("client_id", client_id.as_str()),
            ("code_verifier", VERIFIER),
            ("resource", resource.as_str()),
        ])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let refresh = token["refresh_token"].as_str().unwrap().to_string();
    drop(bridge);
    tokio::time::sleep(Duration::from_millis(100)).await;

    let bridge = spawn_bridge_at_port(port, configure).await;
    let client = Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let authorize_after_restart = client
        .get(format!("{}/oauth/authorize", bridge.base_url))
        .query(&[
            ("response_type", "code"),
            ("client_id", client_id.as_str()),
            ("redirect_uri", REDIRECT_URI),
            ("state", "restart-2"),
            ("code_challenge", challenge.as_str()),
            ("code_challenge_method", "S256"),
            ("resource", resource.as_str()),
            ("scope", "mcp:tools offline_access"),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(
        authorize_after_restart.status(),
        StatusCode::OK,
        "DCR client registration should survive restart"
    );
    let refreshed = client
        .post(format!("{}/oauth/token", bridge.base_url))
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh.as_str()),
            ("client_id", client_id.as_str()),
            ("resource", resource.as_str()),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(refreshed.status(), StatusCode::OK);
    let refreshed: Value = refreshed.json().await.unwrap();
    assert_ne!(refreshed["refresh_token"].as_str(), Some(refresh.as_str()));
}
