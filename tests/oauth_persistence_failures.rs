// A directory blocks replacement by a regular state file on Unix and Windows.

mod common;

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use common::{BridgeProcess, spawn_bridge};
use reqwest::{Client, Response, StatusCode, header};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{fs, path::Path, time::Duration};
use tempfile::tempdir;

const USERNAME: &str = "persistence-test-user";
const PASSWORD: &str = "persistence-test-password-123";
const REDIRECT_URI: &str = "https://chatgpt.com/connector_platform_oauth_redirect";
const VERIFIER: &str = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-._~";

async fn start_bridge(state_file: &Path) -> BridgeProcess {
    fs::write(state_file, r#"{"version":1}"#).expect("initial state should be writable");
    spawn_bridge(|command, port| {
        command
            .env("MCP_PROFILE", "server-secure")
            .env("MCP_PUBLIC_URL", format!("http://127.0.0.1:{port}"))
            .env("MCP_OAUTH_ALLOW_INSECURE_HTTP", "true")
            .env("MCP_OAUTH_USERNAME", USERNAME)
            .env("MCP_OAUTH_PASSWORD", PASSWORD)
            .env("MCP_OAUTH_MAX_FAILED_LOGINS", "10")
            .env("MCP_STATE_FILE", state_file);
    })
    .await
}

fn http_client() -> Client {
    Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(10))
        .build()
        .expect("OAuth test client should build")
}

fn block_state_file(state_file: &Path) {
    fs::rename(state_file, state_file.with_extension("backup"))
        .expect("valid state file should move aside");
    fs::create_dir(state_file).expect("directory should block atomic file replacement");
}

fn restore_state_file(state_file: &Path) {
    fs::remove_dir(state_file).expect("injected blocker should be an empty directory");
    fs::rename(state_file.with_extension("backup"), state_file)
        .expect("original state file should restore");
}

fn snapshot(state_file: &Path) -> Value {
    serde_json::from_slice(&fs::read(state_file).expect("durable state should exist"))
        .expect("durable state should remain valid JSON")
}

async fn register(client: &Client, base_url: &str) -> Response {
    client
        .post(format!("{base_url}/oauth/register"))
        .json(&json!({
            "client_name": "persistence failure test",
            "redirect_uris": [REDIRECT_URI],
            "grant_types": ["authorization_code", "refresh_token"],
            "response_types": ["code"],
            "token_endpoint_auth_method": "none"
        }))
        .send()
        .await
        .expect("registration request should complete")
}

async fn registration_id(response: Response) -> String {
    assert_eq!(response.status(), StatusCode::CREATED);
    response
        .json::<Value>()
        .await
        .expect("registration response should be JSON")["client_id"]
        .as_str()
        .expect("registration should return a client ID")
        .to_string()
}

async fn authorize(client: &Client, base_url: &str, client_id: &str, resource: &str) -> String {
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(VERIFIER.as_bytes()));
    let response = client
        .post(format!("{base_url}/oauth/authorize"))
        .form(&[
            ("response_type", "code"),
            ("client_id", client_id),
            ("redirect_uri", REDIRECT_URI),
            ("state", "persistence-failure"),
            ("code_challenge", challenge.as_str()),
            ("code_challenge_method", "S256"),
            ("resource", resource),
            ("scope", "mcp:tools offline_access"),
            ("username", USERNAME),
            ("password", PASSWORD),
        ])
        .send()
        .await
        .expect("authorization should complete");
    assert_eq!(response.status(), StatusCode::FOUND);
    let location = response.headers()[header::LOCATION]
        .to_str()
        .expect("authorization redirect should be text");
    let location = reqwest::Url::parse(location).expect("authorization redirect should be a URL");
    location
        .query_pairs()
        .find_map(|(key, value)| (key == "code").then(|| value.into_owned()))
        .expect("authorization should issue a code")
}

async fn exchange_code(
    client: &Client,
    token_url: &str,
    client_id: &str,
    resource: &str,
    code: &str,
) -> Response {
    client
        .post(token_url)
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", REDIRECT_URI),
            ("client_id", client_id),
            ("code_verifier", VERIFIER),
            ("resource", resource),
        ])
        .send()
        .await
        .expect("authorization code exchange should complete")
}

async fn refresh(
    client: &Client,
    token_url: &str,
    client_id: &str,
    resource: &str,
    refresh_token: &str,
) -> Response {
    client
        .post(token_url)
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", client_id),
            ("resource", resource),
        ])
        .send()
        .await
        .expect("refresh request should complete")
}

async fn token_response(response: Response, resource: &str) -> Value {
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    let token: Value = response
        .json()
        .await
        .expect("token response should be JSON");
    assert_eq!(token["resource"], resource);
    assert_eq!(token["token_type"], "Bearer");
    assert_eq!(token["scope"], "mcp:tools offline_access");
    assert!(token["expires_in"].as_u64().is_some_and(|ttl| ttl > 0));
    for field in ["access_token", "refresh_token"] {
        assert!(token[field].as_str().is_some_and(|value| !value.is_empty()));
    }
    token
}

async fn assert_oauth_error(response: Response, error: &str) {
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body: Value = response.json().await.expect("OAuth error should be JSON");
    assert_eq!(body["error"], error);
    assert!(body.get("access_token").is_none());
    assert!(body.get("refresh_token").is_none());
}

#[tokio::test]
async fn failed_token_persistence_preserves_credentials_and_refresh_has_one_winner() {
    let directory = tempdir().expect("test directory should exist");
    let state_file = directory.path().join("state.json");
    let bridge = start_bridge(&state_file).await;
    let client = http_client();
    let resource = format!("{}/mcp", bridge.base_url);
    let token_url = format!("{}/oauth/token", bridge.base_url);
    let client_id = registration_id(register(&client, &bridge.base_url).await).await;
    let code = authorize(&client, &bridge.base_url, &client_id, &resource).await;

    block_state_file(&state_file);
    assert_oauth_error(
        exchange_code(&client, &token_url, &client_id, &resource, &code).await,
        "temporarily_unavailable",
    )
    .await;
    restore_state_file(&state_file);
    let issued = token_response(
        exchange_code(&client, &token_url, &client_id, &resource, &code).await,
        &resource,
    )
    .await;
    let persisted = snapshot(&state_file);
    assert_eq!(persisted["access_tokens"].as_object().unwrap().len(), 1);
    assert_eq!(persisted["refresh_tokens"].as_object().unwrap().len(), 1);
    assert_oauth_error(
        exchange_code(&client, &token_url, &client_id, &resource, &code).await,
        "invalid_grant",
    )
    .await;

    let original_refresh = issued["refresh_token"].as_str().unwrap();
    block_state_file(&state_file);
    assert_oauth_error(
        refresh(&client, &token_url, &client_id, &resource, original_refresh).await,
        "temporarily_unavailable",
    )
    .await;
    restore_state_file(&state_file);
    let rotated = token_response(
        refresh(&client, &token_url, &client_id, &resource, original_refresh).await,
        &resource,
    )
    .await;
    assert_ne!(rotated["refresh_token"], issued["refresh_token"]);
    let persisted = snapshot(&state_file);
    assert_eq!(persisted["access_tokens"].as_object().unwrap().len(), 2);
    assert_eq!(persisted["refresh_tokens"].as_object().unwrap().len(), 1);
    assert_oauth_error(
        refresh(&client, &token_url, &client_id, &resource, original_refresh).await,
        "invalid_grant",
    )
    .await;

    let current_refresh = rotated["refresh_token"].as_str().unwrap();
    let (first, second) = tokio::join!(
        refresh(&client, &token_url, &client_id, &resource, current_refresh),
        refresh(&client, &token_url, &client_id, &resource, current_refresh),
    );
    let (winner, loser) = if first.status() == StatusCode::OK {
        (first, second)
    } else {
        (second, first)
    };
    let winner = token_response(winner, &resource).await;
    assert_ne!(winner["refresh_token"], rotated["refresh_token"]);
    assert_oauth_error(loser, "invalid_grant").await;
    let persisted = snapshot(&state_file);
    assert_eq!(persisted["access_tokens"].as_object().unwrap().len(), 3);
    assert_eq!(persisted["refresh_tokens"].as_object().unwrap().len(), 1);
}

#[tokio::test]
async fn failed_registration_is_not_retained_in_memory_or_a_later_snapshot() {
    let directory = tempdir().expect("test directory should exist");
    let state_file = directory.path().join("state.json");
    let bridge = start_bridge(&state_file).await;
    let client = http_client();

    block_state_file(&state_file);
    assert_oauth_error(
        register(&client, &bridge.base_url).await,
        "temporarily_unavailable",
    )
    .await;
    restore_state_file(&state_file);

    // A later successful save exposes clients accidentally left in memory by
    // the failed registration, even though its random client ID was never sent.
    let client_id = registration_id(register(&client, &bridge.base_url).await).await;
    let persisted = snapshot(&state_file);
    let clients = persisted["dcr_clients"].as_object().unwrap();
    assert_eq!(
        clients.len(),
        1,
        "failed registration must not retain a client"
    );
    assert!(clients.contains_key(&client_id));
}
