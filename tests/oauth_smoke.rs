mod common;

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use common::spawn_bridge;
use reqwest::{Client, StatusCode, header};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const USERNAME: &str = "oauth-test-user";
const PASSWORD: &str = "oauth-test-password-123";
const REDIRECT_URI: &str = "https://chatgpt.com/connector_platform_oauth_redirect";
const VERIFIER: &str = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-._~";

fn challenge() -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(VERIFIER.as_bytes()))
}

fn authorize_form<'a>(
    resource: &'a str,
    client_id: &'a str,
    password: &'a str,
    challenge: &'a str,
) -> Vec<(&'a str, &'a str)> {
    vec![
        ("response_type", "code"),
        ("client_id", client_id),
        ("redirect_uri", REDIRECT_URI),
        ("state", "integration-state"),
        ("code_challenge", challenge),
        ("code_challenge_method", "S256"),
        ("resource", resource),
        ("scope", "mcp:tools offline_access"),
        ("username", USERNAME),
        ("password", password),
    ]
}

fn code_from_location(location: &str) -> String {
    let query = location
        .split_once('?')
        .map(|(_, query)| query)
        .expect("OAuth redirect should contain query parameters");
    query
        .split('&')
        .find_map(|pair| pair.strip_prefix("code="))
        .map(|value| {
            urlencoding::decode(value)
                .expect("code should URL-decode")
                .into_owned()
        })
        .expect("OAuth redirect should contain code")
}

fn discover_body() -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": 20,
        "method": "server/discover",
        "params": {
            "_meta": {
                "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                "io.modelcontextprotocol/clientInfo": {"name": "oauth-integration-test", "version": "1.0"},
                "io.modelcontextprotocol/clientCapabilities": {}
            }
        }
    })
}

#[tokio::test]
async fn oauth_pkce_throttling_and_refresh_rotation_work_end_to_end() {
    let bridge = spawn_bridge(|command, port| {
        command
            .env("MCP_PROFILE", "server-secure")
            .env("MCP_PUBLIC_URL", format!("http://127.0.0.1:{port}"))
            .env("MCP_OAUTH_ALLOW_INSECURE_HTTP", "true")
            .env("MCP_OAUTH_USERNAME", USERNAME)
            .env("MCP_OAUTH_PASSWORD", PASSWORD)
            .env("MCP_OAUTH_MAX_FAILED_LOGINS", "2");
    })
    .await;
    let client = Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("OAuth test client should build");
    let authorize_url = format!("{}/oauth/authorize", bridge.base_url);
    let token_url = format!("{}/oauth/token", bridge.base_url);
    let register_url = format!("{}/oauth/register", bridge.base_url);
    let metadata_url = format!("{}/.well-known/oauth-authorization-server", bridge.base_url);
    let resource = format!("{}/mcp", bridge.base_url);
    let challenge = challenge();

    let metadata: Value = client
        .get(&metadata_url)
        .send()
        .await
        .expect("OAuth metadata should load")
        .json()
        .await
        .expect("OAuth metadata should be JSON");
    let protected_metadata: Value = client
        .get(format!(
            "{}/.well-known/oauth-protected-resource/mcp",
            bridge.base_url
        ))
        .send()
        .await
        .expect("path-specific protected resource metadata should load")
        .json()
        .await
        .expect("protected resource metadata should be JSON");
    assert_eq!(protected_metadata["resource"], resource);
    assert_eq!(metadata["client_id_metadata_document_supported"], true);
    assert_eq!(metadata["registration_endpoint"], register_url);
    assert!(
        metadata["scopes_supported"]
            .as_array()
            .expect("scopes_supported should be an array")
            .iter()
            .any(|scope| scope == "offline_access")
    );

    let registration = client
        .post(&register_url)
        .json(&json!({
            "client_name": "OAuth integration test",
            "redirect_uris": [REDIRECT_URI],
            "grant_types": ["authorization_code", "refresh_token"],
            "response_types": ["code"],
            "token_endpoint_auth_method": "none",
            "application_type": "web"
        }))
        .send()
        .await
        .expect("dynamic client registration should complete");
    assert_eq!(registration.status(), StatusCode::CREATED);
    let registration: Value = registration
        .json()
        .await
        .expect("registration response should be JSON");
    let client_id = registration["client_id"]
        .as_str()
        .expect("registration should issue a client_id")
        .to_string();

    let extra = client
        .post(&register_url)
        .header("CF-Connecting-IP", "203.0.113.50")
        .json(&json!({
            "client_name": "DCR throttle test",
            "redirect_uris": [REDIRECT_URI],
            "token_endpoint_auth_method": "none"
        }))
        .send()
        .await
        .expect("DCR throttle setup should complete");
    assert_eq!(extra.status(), StatusCode::CREATED);
    let dcr_limited = client
        .post(&register_url)
        .header("CF-Connecting-IP", "203.0.113.50")
        .json(&json!({
            "client_name": "DCR throttle test",
            "redirect_uris": [REDIRECT_URI],
            "token_endpoint_auth_method": "none"
        }))
        .send()
        .await
        .expect("DCR limited request should complete");
    assert_eq!(dcr_limited.status(), StatusCode::TOO_MANY_REQUESTS);

    let login = client
        .get(&authorize_url)
        .query(&[
            ("response_type", "code"),
            ("client_id", client_id.as_str()),
            ("redirect_uri", REDIRECT_URI),
            ("state", "integration-state"),
            ("code_challenge", challenge.as_str()),
            ("code_challenge_method", "S256"),
            ("resource", resource.as_str()),
            ("scope", "mcp:tools offline_access"),
        ])
        .send()
        .await
        .expect("OAuth login page should load");
    assert_eq!(login.status(), StatusCode::OK);
    assert_eq!(
        login
            .headers()
            .get("x-frame-options")
            .and_then(|v| v.to_str().ok()),
        Some("DENY")
    );
    let csp = login
        .headers()
        .get("content-security-policy")
        .and_then(|value| value.to_str().ok())
        .expect("OAuth login should have a CSP");
    assert!(csp.contains("form-action 'self' https://chatgpt.com"));

    let good = client
        .post(&authorize_url)
        .header("CF-Connecting-IP", "203.0.113.2")
        .form(&authorize_form(&resource, &client_id, PASSWORD, &challenge))
        .send()
        .await
        .expect("good login should complete");
    assert_eq!(good.status(), StatusCode::FOUND);
    let location = good
        .headers()
        .get(header::LOCATION)
        .expect("successful OAuth login should redirect")
        .to_str()
        .expect("location should be text");
    assert!(location.starts_with(REDIRECT_URI));
    let code = code_from_location(location);

    let token = client
        .post(&token_url)
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
        .expect("authorization code exchange should complete");
    assert_eq!(token.status(), StatusCode::OK);
    let token: Value = token.json().await.expect("token response should be JSON");
    assert_eq!(token["token_type"], "Bearer");
    assert_eq!(token["scope"], "mcp:tools offline_access");
    let access = token["access_token"]
        .as_str()
        .expect("access token should be present");
    let refresh = token["refresh_token"]
        .as_str()
        .expect("refresh token should be present");

    let discover = client
        .post(&resource)
        .bearer_auth(access)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json, text/event-stream")
        .header("MCP-Protocol-Version", "2026-07-28")
        .header("Mcp-Method", "server/discover")
        .json(&discover_body())
        .send()
        .await
        .expect("OAuth-authenticated MCP discovery should complete");
    assert_eq!(discover.status(), StatusCode::OK);

    let rotated = client
        .post(&token_url)
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh),
            ("client_id", client_id.as_str()),
            ("resource", resource.as_str()),
        ])
        .send()
        .await
        .expect("refresh should complete");
    assert_eq!(rotated.status(), StatusCode::OK);
    let rotated: Value = rotated
        .json()
        .await
        .expect("refresh response should be JSON");
    assert_ne!(rotated["refresh_token"].as_str(), Some(refresh));
    assert_eq!(rotated["scope"], "mcp:tools offline_access");

    let replay = client
        .post(&token_url)
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh),
            ("client_id", client_id.as_str()),
            ("resource", resource.as_str()),
        ])
        .send()
        .await
        .expect("refresh replay should complete");
    assert_eq!(replay.status(), StatusCode::BAD_REQUEST);
    let replay: Value = replay.json().await.expect("replay response should be JSON");
    assert_eq!(replay["error"], "invalid_grant");

    // Forwarded-IP spoofing is ignored unless a trusted proxy is explicitly configured.
    for _ in 0..2 {
        let bad = client
            .post(&authorize_url)
            .header("CF-Connecting-IP", "203.0.113.1")
            .form(&authorize_form(
                &resource,
                &client_id,
                "wrong-password",
                &challenge,
            ))
            .send()
            .await
            .expect("bad login should complete");
        assert_eq!(bad.status(), StatusCode::OK);
    }
    let limited = client
        .post(&authorize_url)
        .header("CF-Connecting-IP", "203.0.113.1")
        .form(&authorize_form(
            &resource,
            &client_id,
            "wrong-password",
            &challenge,
        ))
        .send()
        .await
        .expect("limited login should complete");
    assert_eq!(limited.status(), StatusCode::TOO_MANY_REQUESTS);
}
