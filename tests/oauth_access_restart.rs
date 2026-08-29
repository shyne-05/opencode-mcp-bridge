mod common;

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use common::{free_port, spawn_bridge_at_port};
use reqwest::{Client, StatusCode, header};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{process::Command, time::Duration};
use tempfile::tempdir;

const USERNAME: &str = "access-restart-user";
const PASSWORD: &str = "access-restart-password-123";
const REDIRECT_URI: &str = "https://chatgpt.com/connector_platform_oauth_redirect";
const VERIFIER: &str = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-._~";

fn pkce_challenge() -> String {
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

async fn discover(base_url: &str, access_token: &str) -> StatusCode {
    Client::new()
        .post(format!("{base_url}/mcp"))
        .bearer_auth(access_token)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json, text/event-stream")
        .header("MCP-Protocol-Version", "2026-07-28")
        .header("Mcp-Method", "server/discover")
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "server/discover",
            "params": {
                "_meta": {
                    "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                    "io.modelcontextprotocol/clientInfo": {
                        "name": "oauth-access-restart-test",
                        "version": "1.0"
                    },
                    "io.modelcontextprotocol/clientCapabilities": {}
                }
            }
        }))
        .send()
        .await
        .expect("discover request should complete")
        .status()
}

#[tokio::test]
async fn oauth_access_token_survives_bridge_restart_without_plaintext_persistence() {
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

    let registration: Value = client
        .post(format!("{}/oauth/register", bridge.base_url))
        .json(&json!({
            "client_name": "access restart test",
            "redirect_uris": [REDIRECT_URI],
            "grant_types": ["authorization_code", "refresh_token"],
            "response_types": ["code"],
            "token_endpoint_auth_method": "none"
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let client_id = registration["client_id"].as_str().unwrap().to_string();
    let challenge = pkce_challenge();

    let approved = client
        .post(format!("{}/oauth/authorize", bridge.base_url))
        .form(&[
            ("response_type", "code"),
            ("client_id", client_id.as_str()),
            ("redirect_uri", REDIRECT_URI),
            ("state", "access-restart"),
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
    let access_token = token["access_token"].as_str().unwrap().to_string();

    assert_eq!(
        discover(&bridge.base_url, &access_token).await,
        StatusCode::OK
    );
    let persisted = std::fs::read_to_string(&state_file).unwrap();
    assert!(!persisted.contains(&access_token));
    assert!(!persisted.contains("mcp_access_"));
    assert!(persisted.contains("sha256:"));

    drop(bridge);
    tokio::time::sleep(Duration::from_millis(100)).await;

    let bridge = spawn_bridge_at_port(port, configure).await;
    assert_eq!(
        discover(&bridge.base_url, &access_token).await,
        StatusCode::OK,
        "an unexpired OAuth access token should remain valid across a bridge restart"
    );
}
