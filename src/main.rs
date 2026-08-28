use axum::{
    extract::{DefaultBodyLimit, Form, Path, Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    env,
    path::{Path as FsPath, PathBuf},
    process::Stdio,
    sync::{Arc, OnceLock},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::RwLock;

const APP_NAME: &str = "mcp-bridge";
const DEFAULT_BACKEND_URL: &str = "http://127.0.0.1:4097";
const MAX_REQUEST_BYTES: usize = 1_048_576;

static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();

#[derive(Clone, Default)]
struct AppState {
    sessions: Arc<RwLock<HashMap<String, HashSet<String>>>>,
    oauth_codes: Arc<RwLock<HashMap<String, AuthorizationCode>>>,
    oauth_access_tokens: Arc<RwLock<HashMap<String, OAuthAccessToken>>>,
    oauth_refresh_tokens: Arc<RwLock<HashMap<String, OAuthRefreshToken>>>,
}

#[derive(Clone)]
struct AccessToken {
    id: String,
    value: String,
}

#[derive(Clone)]
struct AuthorizationCode {
    client_id: String,
    redirect_uri: String,
    resource: String,
    scope: String,
    code_challenge: String,
    expires_at: u64,
    principal: String,
}

#[derive(Clone)]
struct OAuthAccessToken {
    principal: String,
    resource: String,
    expires_at: u64,
}

#[derive(Clone)]
struct OAuthRefreshToken {
    client_id: String,
    principal: String,
    resource: String,
    scope: String,
    expires_at: u64,
}

#[derive(Deserialize)]
struct RpcRequest {
    jsonrpc: String,
    id: Option<Value>,
    method: String,
    params: Option<Value>,
}

#[derive(Clone, Default, Deserialize)]
struct OAuthAuthorizeRequest {
    response_type: Option<String>,
    client_id: Option<String>,
    redirect_uri: Option<String>,
    state: Option<String>,
    code_challenge: Option<String>,
    code_challenge_method: Option<String>,
    resource: Option<String>,
    scope: Option<String>,
}

#[derive(Deserialize)]
struct OAuthLoginForm {
    #[serde(flatten)]
    request: OAuthAuthorizeRequest,
    username: String,
    password: String,
}

#[derive(Default, Deserialize)]
struct OAuthTokenRequest {
    grant_type: Option<String>,
    code: Option<String>,
    redirect_uri: Option<String>,
    client_id: Option<String>,
    code_verifier: Option<String>,
    refresh_token: Option<String>,
    resource: Option<String>,
}

struct BashOut {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

fn client() -> &'static reqwest::Client {
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .pool_idle_timeout(Duration::from_secs(30))
            .tcp_keepalive(Duration::from_secs(30))
            .build()
            .expect("failed to build HTTP client")
    })
}

fn now_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn trunc(value: &str, limit: usize) -> String {
    if value.chars().count() <= limit {
        value.to_string()
    } else {
        value.chars().take(limit).collect()
    }
}

fn env_bool(name: &str, default: bool) -> bool {
    match env::var(name) {
        Ok(value) => matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        ),
        Err(_) => default,
    }
}

fn backend_url() -> String {
    env::var("BRIDGE_BACKEND_URL")
        .unwrap_or_else(|_| DEFAULT_BACKEND_URL.to_string())
        .trim_end_matches('/')
        .to_string()
}

fn configured_workdir() -> String {
    env::var("BRIDGE_WORKDIR").unwrap_or_else(|_| ".".to_string())
}

fn host_tools_enabled() -> bool {
    env_bool("MCP_ENABLE_HOST_TOOLS", false)
}

fn allow_unauthenticated() -> bool {
    env_bool("MCP_ALLOW_UNAUTHENTICATED", false)
}

fn public_url() -> Option<String> {
    let value = env::var("MCP_PUBLIC_URL")
        .ok()?
        .trim()
        .trim_end_matches('/')
        .to_string();
    let secure = value.starts_with("https://")
        || (env_bool("MCP_OAUTH_ALLOW_INSECURE_HTTP", false)
            && (value.starts_with("http://127.0.0.1") || value.starts_with("http://localhost")));
    let has_path = value
        .split_once("://")
        .is_some_and(|(_, authority)| authority.contains('/'));
    if secure && !has_path && !value.contains([' ', '?', '#']) {
        Some(value)
    } else {
        None
    }
}

fn public_resource() -> Option<String> {
    public_url().map(|url| format!("{url}/mcp"))
}

fn oauth_username() -> String {
    env::var("MCP_OAUTH_USERNAME")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "user".to_string())
}

fn oauth_password() -> Option<String> {
    env::var("MCP_OAUTH_PASSWORD")
        .ok()
        .filter(|value| !value.trim().is_empty())
}

fn oauth_enabled() -> bool {
    public_url().is_some() && oauth_password().is_some()
}

fn oauth_ttl(name: &str, default: u64) -> u64 {
    env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
        .max(60)
}

fn now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn random_token(prefix: &str) -> String {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("operating system randomness is unavailable");
    format!("{prefix}_{}", URL_SAFE_NO_PAD.encode(bytes))
}

fn pkce_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

fn valid_client_id(client_id: &str) -> bool {
    client_id == "chatgpt-mcp"
        || client_id == "https://chatgpt.com/oauth/client.json"
        || (client_id.starts_with("https://chatgpt.com/oauth/")
            && client_id.ends_with("/client.json"))
}

fn valid_redirect_uri(redirect_uri: &str) -> bool {
    redirect_uri == "https://chatgpt.com/connector_platform_oauth_redirect"
        || (redirect_uri
            .strip_prefix("https://chatgpt.com/connector/oauth/")
            .is_some_and(|callback_id| {
                !callback_id.is_empty()
                    && !callback_id.contains(['/', '?', '#'])
                    && callback_id
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            }))
}

fn requested_scope(scope: Option<&str>) -> Result<String, String> {
    let scope = scope.unwrap_or("mcp:tools").trim();
    if scope.is_empty() || scope.split_whitespace().all(|value| value == "mcp:tools") {
        Ok("mcp:tools".to_string())
    } else if scope.split_whitespace().all(|value| value == "mcp") {
        Ok("mcp".to_string())
    } else {
        Err("unsupported scope".to_string())
    }
}

fn configured_tokens() -> Vec<AccessToken> {
    if let Ok(raw) = env::var("MCP_TOKENS") {
        let tokens = raw
            .split(',')
            .enumerate()
            .filter_map(|(index, entry)| {
                let entry = entry.trim();
                if entry.is_empty() {
                    return None;
                }
                let (id, value) = match entry.split_once('=') {
                    Some((id, value)) if !id.trim().is_empty() && !value.trim().is_empty() => {
                        (id.trim().to_string(), value.trim().to_string())
                    }
                    _ => (format!("user-{}", index + 1), entry.to_string()),
                };
                Some(AccessToken { id, value })
            })
            .collect::<Vec<_>>();
        if !tokens.is_empty() {
            return tokens;
        }
    }

    env::var("MCP_TOKEN")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(|value| {
            vec![AccessToken {
                id: "default".to_string(),
                value: value.trim().to_string(),
            }]
        })
        .unwrap_or_default()
}

fn constant_time_equal(left: &str, right: &str) -> bool {
    let left = left.as_bytes();
    let right = right.as_bytes();
    let mut difference = (left.len() ^ right.len()) as u64;
    let length = left.len().max(right.len());
    for index in 0..length {
        difference |= u64::from(
            left.get(index).copied().unwrap_or_default()
                ^ right.get(index).copied().unwrap_or_default(),
        );
    }
    difference == 0
}

fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = value.split_once(' ')?;
    if scheme.eq_ignore_ascii_case("bearer") && !token.trim().is_empty() {
        Some(token.trim())
    } else {
        None
    }
}

async fn authenticated_principal(
    state: &AppState,
    headers: &HeaderMap,
    path_token: Option<&str>,
) -> Option<String> {
    let configured = configured_tokens();
    let candidates = [
        path_token,
        headers
            .get("x-mcp-token")
            .and_then(|value| value.to_str().ok()),
        bearer_token(headers),
    ];
    if let Some(principal) = configured.iter().find_map(|expected| {
        candidates
            .iter()
            .flatten()
            .any(|candidate| constant_time_equal(&expected.value, candidate))
            .then(|| expected.id.clone())
    }) {
        return Some(principal);
    }
    if let Some(principal) = oauth_principal(state, headers).await {
        return Some(principal);
    }
    (configured.is_empty() && allow_unauthenticated()).then(|| "local".to_string())
}

async fn oauth_principal(state: &AppState, headers: &HeaderMap) -> Option<String> {
    if !oauth_enabled() {
        return None;
    }
    let token = bearer_token(headers)?;
    let resource = public_resource()?;
    let tokens = state.oauth_access_tokens.read().await;
    let access = tokens.get(token)?;
    (access.expires_at > now_seconds() && access.resource == resource)
        .then(|| access.principal.clone())
}

fn unauthorized_response(id: Value) -> Response {
    let metadata_url = public_url()
        .filter(|_| oauth_enabled())
        .map(|url| format!("{url}/.well-known/oauth-protected-resource"));
    let body = Json(json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {"code": -32001, "message": "Unauthorized"},
        "_meta": metadata_url.as_ref().map(|url| json!({
            "mcp/www_authenticate": format!(
                "Bearer error=\"invalid_token\", error_description=\"Authentication is required\", resource_metadata=\"{url}\", scope=\"mcp:tools\""
            )
        }))
    }));
    let challenge = metadata_url
        .as_ref()
        .map(|url| {
            format!(r#"Bearer realm="mcp-bridge", resource_metadata="{url}", scope="mcp:tools""#)
        })
        .unwrap_or_else(|| r#"Bearer realm="mcp-bridge""#.to_string());
    let challenge = HeaderValue::try_from(challenge)
        .unwrap_or_else(|_| HeaderValue::from_static(r#"Bearer realm="mcp-bridge""#));
    (
        StatusCode::UNAUTHORIZED,
        [(header::WWW_AUTHENTICATE, challenge)],
        body,
    )
        .into_response()
}

fn oauth_json(status: StatusCode, body: Value) -> Response {
    (
        status,
        [
            (header::CACHE_CONTROL, HeaderValue::from_static("no-store")),
            (header::PRAGMA, HeaderValue::from_static("no-cache")),
        ],
        Json(body),
    )
        .into_response()
}

fn oauth_redirect(location: String) -> Response {
    match HeaderValue::try_from(location) {
        Ok(location) => (StatusCode::FOUND, [(header::LOCATION, location)]).into_response(),
        Err(_) => oauth_json(
            StatusCode::BAD_REQUEST,
            json!({"error": "invalid_request", "error_description": "invalid redirect URI"}),
        ),
    }
}

fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn html_hidden(name: &str, value: Option<&str>) -> String {
    format!(
        "<input type=\"hidden\" name=\"{}\" value=\"{}\">",
        html_escape(name),
        html_escape(value.unwrap_or_default())
    )
}

fn oauth_login_page(request: &OAuthAuthorizeRequest, error: Option<&str>) -> Response {
    let hidden = [
        html_hidden("response_type", request.response_type.as_deref()),
        html_hidden("client_id", request.client_id.as_deref()),
        html_hidden("redirect_uri", request.redirect_uri.as_deref()),
        html_hidden("state", request.state.as_deref()),
        html_hidden("code_challenge", request.code_challenge.as_deref()),
        html_hidden(
            "code_challenge_method",
            request.code_challenge_method.as_deref(),
        ),
        html_hidden("resource", request.resource.as_deref()),
        html_hidden("scope", request.scope.as_deref()),
    ]
    .join("");
    let error = error
        .map(|value| {
            format!(
                "<p role=\"alert\" style=\"color:#b00020\">{}</p>",
                html_escape(value)
            )
        })
        .unwrap_or_default();
    let body = format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>MCP Bridge authorization</title></head><body><main><h1>MCP Bridge authorization</h1><p>Sign in to authorize this MCP connection.</p>{error}<form method=\"post\" action=\"/oauth/authorize\">{hidden}<label>Username<br><input name=\"username\" autocomplete=\"username\" required></label><br><label>Password<br><input type=\"password\" name=\"password\" autocomplete=\"current-password\" required></label><br><button type=\"submit\">Authorize</button></form></main></body></html>"
    );
    (
        StatusCode::OK,
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/html; charset=utf-8"),
        )],
        body,
    )
        .into_response()
}

fn oauth_authorization_error(request: &OAuthAuthorizeRequest, description: &str) -> Response {
    if let (Some(redirect_uri), Some(issuer)) = (request.redirect_uri.as_deref(), public_url()) {
        if valid_redirect_uri(redirect_uri) {
            let mut parameters = vec![
                "error=invalid_request".to_string(),
                format!("error_description={}", urlencoding::encode(description)),
                format!("iss={}", urlencoding::encode(&issuer)),
            ];
            if let Some(state) = request.state.as_deref() {
                parameters.push(format!("state={}", urlencoding::encode(state)));
            }
            let location = format!("{redirect_uri}?{}", parameters.join("&"));
            return oauth_redirect(location);
        }
    }
    oauth_json(
        StatusCode::BAD_REQUEST,
        json!({"error": "invalid_request", "error_description": description}),
    )
}

fn validate_authorization_request(request: &OAuthAuthorizeRequest) -> Result<String, String> {
    if request.response_type.as_deref() != Some("code") {
        return Err("response_type must be code".to_string());
    }
    let client_id = request
        .client_id
        .as_deref()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "client_id is required".to_string())?;
    if !valid_client_id(client_id) {
        return Err("unsupported client_id".to_string());
    }
    let redirect_uri = request
        .redirect_uri
        .as_deref()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "redirect_uri is required".to_string())?;
    if !valid_redirect_uri(redirect_uri) {
        return Err("redirect_uri is not allowed".to_string());
    }
    if request
        .code_challenge
        .as_deref()
        .is_none_or(|value| value.is_empty() || value.contains(char::is_whitespace))
    {
        return Err("code_challenge is required".to_string());
    }
    if request.code_challenge_method.as_deref() != Some("S256") {
        return Err("code_challenge_method must be S256".to_string());
    }
    let resource =
        public_resource().ok_or_else(|| "MCP_PUBLIC_URL is not configured".to_string())?;
    if request
        .resource
        .as_deref()
        .is_some_and(|value| value != resource)
    {
        return Err("resource does not match MCP_PUBLIC_URL".to_string());
    }
    requested_scope(request.scope.as_deref())
}

async fn oauth_protected_resource_metadata() -> Response {
    let Some(issuer) = public_url().filter(|_| oauth_enabled()) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Some(resource) = public_resource() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    Json(json!({
        "resource": resource,
        "authorization_servers": [issuer],
        "scopes_supported": ["mcp:tools", "mcp"],
        "bearer_methods_supported": ["header"]
    }))
    .into_response()
}

async fn oauth_authorization_server_metadata() -> Response {
    let Some(issuer) = public_url().filter(|_| oauth_enabled()) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    Json(json!({
        "issuer": issuer,
        "authorization_endpoint": format!("{issuer}/oauth/authorize"),
        "token_endpoint": format!("{issuer}/oauth/token"),
        "response_types_supported": ["code"],
        "grant_types_supported": ["authorization_code", "refresh_token"],
        "token_endpoint_auth_methods_supported": ["none"],
        "client_id_metadata_document_supported": true,
        "authorization_response_iss_parameter_supported": true,
        "code_challenge_methods_supported": ["S256"],
        "scopes_supported": ["mcp:tools", "mcp"]
    }))
    .into_response()
}

async fn oauth_authorize_get(Query(request): Query<OAuthAuthorizeRequest>) -> Response {
    if !oauth_enabled() {
        return oauth_json(
            StatusCode::SERVICE_UNAVAILABLE,
            json!({"error": "OAuth is not configured"}),
        );
    }
    match validate_authorization_request(&request) {
        Ok(_) => oauth_login_page(&request, None),
        Err(error) => oauth_authorization_error(&request, &error),
    }
}

async fn oauth_authorize_post(
    State(state): State<AppState>,
    Form(form): Form<OAuthLoginForm>,
) -> Response {
    if !oauth_enabled() {
        return oauth_json(
            StatusCode::SERVICE_UNAVAILABLE,
            json!({"error": "OAuth is not configured"}),
        );
    }
    if let Err(error) = validate_authorization_request(&form.request) {
        return oauth_authorization_error(&form.request, &error);
    }
    let Some(expected_password) = oauth_password() else {
        return oauth_json(
            StatusCode::SERVICE_UNAVAILABLE,
            json!({"error": "OAuth is not configured"}),
        );
    };
    if !constant_time_equal(&oauth_username(), &form.username)
        || !constant_time_equal(&expected_password, &form.password)
    {
        return oauth_login_page(&form.request, Some("Invalid username or password."));
    }

    let client_id = form.request.client_id.clone().unwrap_or_default();
    let redirect_uri = form.request.redirect_uri.clone().unwrap_or_default();
    let resource = public_resource().unwrap_or_default();
    let scope = requested_scope(form.request.scope.as_deref()).unwrap_or_default();
    let code = random_token("mcp_code");
    state.oauth_codes.write().await.insert(
        code.clone(),
        AuthorizationCode {
            client_id,
            redirect_uri: redirect_uri.clone(),
            resource,
            scope,
            code_challenge: form.request.code_challenge.unwrap_or_default(),
            expires_at: now_seconds() + oauth_ttl("MCP_OAUTH_CODE_TTL", 300),
            principal: oauth_username(),
        },
    );
    let state_value = form.request.state.unwrap_or_default();
    let issuer = public_url().unwrap_or_default();
    let location = format!(
        "{redirect_uri}?code={}&state={}&iss={}",
        urlencoding::encode(&code),
        urlencoding::encode(&state_value),
        urlencoding::encode(&issuer),
    );
    oauth_redirect(location)
}

fn oauth_token_error(error: &str, description: &str) -> Response {
    oauth_json(
        StatusCode::BAD_REQUEST,
        json!({"error": error, "error_description": description}),
    )
}

async fn issue_oauth_tokens(
    state: &AppState,
    client_id: &str,
    principal: &str,
    resource: &str,
    scope: &str,
    refresh_token: Option<String>,
) -> Response {
    let now = now_seconds();
    let access_token = random_token("mcp_access");
    let access_expires_at = now + oauth_ttl("MCP_OAUTH_ACCESS_TOKEN_TTL", 3600);
    state.oauth_access_tokens.write().await.insert(
        access_token.clone(),
        OAuthAccessToken {
            principal: principal.to_string(),
            resource: resource.to_string(),
            expires_at: access_expires_at,
        },
    );

    let refresh_token = refresh_token.unwrap_or_else(|| random_token("mcp_refresh"));
    state.oauth_refresh_tokens.write().await.insert(
        refresh_token.clone(),
        OAuthRefreshToken {
            client_id: client_id.to_string(),
            principal: principal.to_string(),
            resource: resource.to_string(),
            scope: scope.to_string(),
            expires_at: now + oauth_ttl("MCP_OAUTH_REFRESH_TOKEN_TTL", 2_592_000),
        },
    );

    oauth_json(
        StatusCode::OK,
        json!({
            "access_token": access_token,
            "token_type": "Bearer",
            "expires_in": access_expires_at.saturating_sub(now),
            "refresh_token": refresh_token,
            "scope": scope
        }),
    )
}

async fn oauth_token(
    State(state): State<AppState>,
    Form(request): Form<OAuthTokenRequest>,
) -> Response {
    if !oauth_enabled() {
        return oauth_token_error("temporarily_unavailable", "OAuth is not configured");
    }
    let Some(client_id) = request
        .client_id
        .as_deref()
        .filter(|value| !value.is_empty())
    else {
        return oauth_token_error("invalid_request", "client_id is required");
    };
    if !valid_client_id(client_id) {
        return oauth_token_error("invalid_client", "unsupported client_id");
    }
    let expected_resource = public_resource().unwrap_or_default();
    match request.grant_type.as_deref() {
        Some("authorization_code") => {
            let Some(code_value) = request.code.as_deref().filter(|value| !value.is_empty()) else {
                return oauth_token_error("invalid_request", "code is required");
            };
            let Some(code) = state.oauth_codes.write().await.remove(code_value) else {
                return oauth_token_error(
                    "invalid_grant",
                    "authorization code is invalid or expired",
                );
            };
            if code.expires_at <= now_seconds()
                || code.client_id != client_id
                || request.redirect_uri.as_deref() != Some(code.redirect_uri.as_str())
                || code.resource != expected_resource
                || request
                    .resource
                    .as_deref()
                    .is_some_and(|value| value != code.resource)
            {
                return oauth_token_error(
                    "invalid_grant",
                    "authorization code is invalid or expired",
                );
            }
            let Some(verifier) = request.code_verifier.as_deref() else {
                return oauth_token_error("invalid_grant", "code_verifier is required");
            };
            if !constant_time_equal(&pkce_challenge(verifier), &code.code_challenge) {
                return oauth_token_error("invalid_grant", "PKCE verification failed");
            }
            issue_oauth_tokens(
                &state,
                client_id,
                &code.principal,
                &code.resource,
                &code.scope,
                None,
            )
            .await
        }
        Some("refresh_token") => {
            let Some(refresh_value) = request
                .refresh_token
                .as_deref()
                .filter(|value| !value.is_empty())
            else {
                return oauth_token_error("invalid_request", "refresh_token is required");
            };
            let Some(refresh) = state
                .oauth_refresh_tokens
                .read()
                .await
                .get(refresh_value)
                .cloned()
            else {
                return oauth_token_error("invalid_grant", "refresh token is invalid or expired");
            };
            if refresh.expires_at <= now_seconds()
                || refresh.client_id != client_id
                || refresh.resource != expected_resource
                || request
                    .resource
                    .as_deref()
                    .is_some_and(|value| value != refresh.resource)
            {
                return oauth_token_error("invalid_grant", "refresh token is invalid or expired");
            }
            issue_oauth_tokens(
                &state,
                client_id,
                &refresh.principal,
                &refresh.resource,
                &refresh.scope,
                Some(refresh_value.to_string()),
            )
            .await
        }
        _ => oauth_token_error("unsupported_grant_type", "grant_type is not supported"),
    }
}

fn rpc_error(id: Value, code: i64, message: impl Into<String>) -> Response {
    Json(json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {"code": code, "message": message.into()}
    }))
    .into_response()
}

async fn health() -> impl IntoResponse {
    let url = format!("{}/global/health", backend_url());
    match client().get(url).send().await {
        Ok(response) if response.status().is_success() => {
            Json(json!({"ok": true, "backend": true})).into_response()
        }
        Ok(_) | Err(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"ok": false, "backend": false})),
        )
            .into_response(),
    }
}

async fn index() -> impl IntoResponse {
    Json(json!({
        "name": APP_NAME,
        "version": env!("CARGO_PKG_VERSION"),
        "mcp": "/mcp",
        "authentication": if oauth_enabled() { "oauth2-and-bearer-token" } else { "bearer-token" }
    }))
}

fn tool_definition(name: &str, description: &str, properties: Value, required: Value) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": {"type": "object", "properties": properties, "required": required},
        "securitySchemes": [{"type": "oauth2", "scopes": ["mcp:tools"]}]
    })
}

fn tools_list() -> Value {
    let mut tools = vec![
        tool_definition(
            "bridge_prompt",
            "Send a prompt to the configured agent backend.",
            json!({
                "prompt": {"type": "string"},
                "sessionId": {"type": "string"},
                "agent": {"type": "string"},
                "model": {"type": "string"},
                "directory": {"type": "string"}
            }),
            json!(["prompt"]),
        ),
        tool_definition(
            "bridge_prompt_async",
            "Send a prompt to the configured agent backend without waiting for completion.",
            json!({
                "prompt": {"type": "string"},
                "sessionId": {"type": "string"},
                "directory": {"type": "string"}
            }),
            json!(["prompt"]),
        ),
        tool_definition(
            "bridge_session_messages",
            "Read messages from a session created through this bridge.",
            json!({
                "sessionId": {"type": "string"},
                "limit": {"type": "number"}
            }),
            json!(["sessionId"]),
        ),
        tool_definition(
            "bridge_read_file",
            "Read a file through the configured agent backend.",
            json!({"path": {"type": "string"}}),
            json!(["path"]),
        ),
        tool_definition(
            "bridge_search",
            "Search the configured agent backend workspace.",
            json!({"pattern": {"type": "string"}}),
            json!(["pattern"]),
        ),
        tool_definition(
            "bridge_list_sessions",
            "List sessions created through this bridge instance.",
            json!({}),
            json!([]),
        ),
        tool_definition(
            "bridge_session_status",
            "Read the status of a session created through this bridge.",
            json!({"sessionId": {"type": "string"}}),
            json!(["sessionId"]),
        ),
    ];

    if host_tools_enabled() {
        tools.extend([
            tool_definition(
                "shell",
                "Run a bash command on the host. This is unrestricted host access; enable only on a trusted private network.",
                json!({
                    "command": {"type": "string"},
                    "directory": {"type": "string"}
                }),
                json!(["command"]),
            ),
            tool_definition(
                "bridge_agent_prompt",
                "Run a prompt through the configured command-line agent with an explicit sandbox mode.",
                json!({
                    "prompt": {"type": "string"},
                    "directory": {"type": "string"},
                    "sandbox": {"type": "string", "enum": ["read-only", "workspace-write", "danger-full-access"]}
                }),
                json!(["prompt"]),
            ),
            tool_definition(
                "browser",
                "Control the local Chrome debugging session. Browser cookies and page data are available to this tool.",
                json!({
                    "action": {"type": "string", "enum": ["navigate", "tabs", "close", "evaluate", "new", "snapshot", "click", "fill"]},
                    "url": {"type": "string"},
                    "targetId": {"type": "string"},
                    "expression": {"type": "string"},
                    "selector": {"type": "string"},
                    "value": {"type": "string"}
                }),
                json!(["action"]),
            ),
        ]);
    }

    Value::Array(tools)
}

fn tool_disabled(name: &str) -> Value {
    json!({
        "content": [{"type": "text", "text": format!("Tool '{}' is disabled. Set MCP_ENABLE_HOST_TOOLS=true and restart the bridge.", name)}],
        "isError": true
    })
}

fn valid_directory(requested: Option<&str>) -> Result<String, String> {
    let base = std::fs::canonicalize(configured_workdir())
        .map_err(|error| format!("invalid BRIDGE_WORKDIR: {error}"))?;
    let candidate = requested.unwrap_or_else(|| base.to_str().unwrap_or("."));
    let candidate = FsPath::new(candidate);
    let candidate = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        base.join(candidate)
    };
    let resolved = std::fs::canonicalize(&candidate)
        .map_err(|error| format!("invalid directory '{}': {}", candidate.display(), error))?;
    if !resolved.starts_with(&base) {
        return Err(format!(
            "directory must be inside BRIDGE_WORKDIR ({})",
            base.display()
        ));
    }
    Ok(resolved.to_string_lossy().into_owned())
}

async fn remember_session(state: &AppState, principal: &str, session_id: &str) {
    let mut sessions = state.sessions.write().await;
    sessions
        .entry(principal.to_string())
        .or_default()
        .insert(session_id.to_string());
}

async fn owns_session(state: &AppState, principal: &str, session_id: &str) -> bool {
    state
        .sessions
        .read()
        .await
        .get(principal)
        .is_some_and(|sessions| sessions.contains(session_id))
}

async fn owned_sessions(state: &AppState, principal: &str) -> HashSet<String> {
    state
        .sessions
        .read()
        .await
        .get(principal)
        .cloned()
        .unwrap_or_default()
}

fn response_text(text: impl Into<String>) -> Value {
    json!({"content": [{"type": "text", "text": text.into()}]})
}

fn response_error(text: impl Into<String>) -> Value {
    json!({"content": [{"type": "text", "text": text.into()}], "isError": true})
}

async fn run_bash(command: &str, directory: &str, timeout: Duration) -> BashOut {
    let mut process = tokio::process::Command::new("bash");
    process
        .arg("-c")
        .arg(command)
        .current_dir(directory)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    for key in [
        "DISPLAY",
        "WAYLAND_DISPLAY",
        "XDG_RUNTIME_DIR",
        "DBUS_SESSION_BUS_ADDRESS",
        "XDG_SESSION_TYPE",
    ] {
        if let Ok(value) = env::var(key) {
            process.env(key, value);
        }
    }

    let child = match process.spawn() {
        Ok(child) => child,
        Err(error) => {
            return BashOut {
                code: Some(1),
                stdout: String::new(),
                stderr: error.to_string(),
            }
        }
    };
    let pid = child.id();
    match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(output)) => BashOut {
            code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        },
        Ok(Err(error)) => BashOut {
            code: Some(1),
            stdout: String::new(),
            stderr: error.to_string(),
        },
        Err(_) => {
            if let Some(pid) = pid {
                let pid_string = pid.to_string();
                let _ = tokio::process::Command::new("kill")
                    .args(["-9", pid_string.as_str()])
                    .status()
                    .await;
            }
            BashOut {
                code: None,
                stdout: String::new(),
                stderr: "command timed out".to_string(),
            }
        }
    }
}

async fn run_agent(prompt: &str, directory: &str, sandbox: &str) -> String {
    let command = match env::var("MCP_AGENT_COMMAND") {
        Ok(command) if !command.trim().is_empty() => command,
        _ => return "MCP_AGENT_COMMAND is required for bridge_agent_prompt".to_string(),
    };
    let args = [
        "exec",
        "--json",
        "-C",
        directory,
        "--skip-git-repo-check",
        "--sandbox",
        sandbox,
        prompt,
    ];
    let child = match tokio::process::Command::new(command.trim())
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(error) => return format!("failed to start configured agent: {error}"),
    };
    let pid = child.id();
    match tokio::time::timeout(Duration::from_secs(180), child.wait_with_output()).await {
        Ok(Ok(output)) => format!(
            "exit:{:?}\nSTDOUT:\n{}\nSTDERR:\n{}",
            output.status.code(),
            trunc(&String::from_utf8_lossy(&output.stdout), 20_000),
            trunc(&String::from_utf8_lossy(&output.stderr), 4_000)
        ),
        Ok(Err(error)) => format!("agent error: {error}"),
        Err(_) => {
            if let Some(pid) = pid {
                let pid_string = pid.to_string();
                let _ = tokio::process::Command::new("kill")
                    .args(["-9", pid_string.as_str()])
                    .status()
                    .await;
            }
            "agent timed out after 180 seconds".to_string()
        }
    }
}

fn browser_script_path() -> PathBuf {
    if let Ok(path) = env::var("MCP_BROWSER_SCRIPT") {
        return PathBuf::from(path);
    }
    if let Ok(executable) = env::current_exe() {
        if let Some(parent) = executable.parent() {
            let adjacent = parent.join("browser.cjs");
            if adjacent.is_file() {
                return adjacent;
            }
        }
    }
    PathBuf::from("scripts/browser.cjs")
}

async fn node_path() -> Option<String> {
    if let Ok(path) = env::var("NODE_PATH") {
        if !path.trim().is_empty() {
            return Some(path);
        }
    }
    let npm = if cfg!(windows) { "npm.cmd" } else { "npm" };
    let output = tokio::process::Command::new(npm)
        .args(["root", "-g"])
        .output()
        .await
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

async fn run_browser_script(action: &str, args: &[&str]) -> String {
    let script = browser_script_path();
    if !script.is_file() {
        return format!("browser script not found: {}", script.display());
    }
    let mut process = tokio::process::Command::new("node");
    process.arg(script).arg(action).args(args);
    if let Some(path) = node_path().await {
        process.env("NODE_PATH", path);
    }
    match tokio::time::timeout(Duration::from_secs(30), process.output()).await {
        Ok(Ok(output)) => format!(
            "exit:{:?}\nSTDOUT:\n{}\nSTDERR:\n{}",
            output.status.code(),
            trunc(&String::from_utf8_lossy(&output.stdout), 12_000),
            trunc(&String::from_utf8_lossy(&output.stderr), 3_000)
        ),
        Ok(Err(error)) => format!("browser process error: {error}"),
        Err(_) => "browser operation timed out after 30 seconds".to_string(),
    }
}

fn safe_browser_url(url: &str) -> Result<&str, String> {
    if url == "about:blank" || url.starts_with("http://") || url.starts_with("https://") {
        Ok(url)
    } else {
        Err("browser URLs must use http://, https://, or about:blank".to_string())
    }
}

async fn call_tool(state: &AppState, principal: &str, name: &str, args: &Value) -> Value {
    if matches!(name, "shell" | "browser" | "bridge_agent_prompt") && !host_tools_enabled() {
        return tool_disabled(name);
    }

    let backend = backend_url();
    let http = client();
    match name {
        "bridge_read_file" => {
            let path = args.get("path").and_then(Value::as_str).unwrap_or_default();
            if path.is_empty() {
                return response_error("path is required");
            }
            let url = format!(
                "{}/file/content?path={}",
                backend,
                urlencoding::encode(path)
            );
            let text = match http.get(url).send().await {
                Ok(response) => response.text().await.unwrap_or_default(),
                Err(error) => format!("backend request failed: {error}"),
            };
            if let Ok(value) = serde_json::from_str::<Value>(&text) {
                if let Some(content) = value.get("content").and_then(Value::as_str) {
                    return response_text(trunc(content, 15_000));
                }
                if let Some(content) = value.get("text").and_then(Value::as_str) {
                    return response_text(trunc(content, 15_000));
                }
            }
            response_text(trunc(&text, 15_000))
        }
        "bridge_search" => {
            let pattern = args
                .get("pattern")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if pattern.is_empty() {
                return response_error("pattern is required");
            }
            let url = format!("{}/find?pattern={}", backend, urlencoding::encode(pattern));
            let text = match http.get(url).send().await {
                Ok(response) => response.text().await.unwrap_or_default(),
                Err(error) => format!("backend request failed: {error}"),
            };
            response_text(trunc(&text, 15_000))
        }
        "bridge_list_sessions" => {
            let text = match http.get(format!("{backend}/session")).send().await {
                Ok(response) => response.text().await.unwrap_or_default(),
                Err(error) => format!("backend request failed: {error}"),
            };
            let owned = owned_sessions(state, principal).await;
            match serde_json::from_str::<Value>(&text) {
                Ok(Value::Array(items)) => response_text(
                    serde_json::to_string(
                        &items
                            .into_iter()
                            .filter(|item| {
                                item.get("id")
                                    .and_then(Value::as_str)
                                    .is_some_and(|id| owned.contains(id))
                            })
                            .collect::<Vec<_>>(),
                    )
                    .unwrap_or_else(|_| "[]".to_string()),
                ),
                _ => response_error("backend returned an invalid session list"),
            }
        }
        "bridge_session_messages" => {
            let session_id = args
                .get("sessionId")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if !owns_session(state, principal, session_id).await {
                return response_error("session does not belong to this authenticated user");
            }
            let limit = args
                .get("limit")
                .and_then(Value::as_u64)
                .unwrap_or(10)
                .min(100);
            let url = format!(
                "{}/session/{}/message?limit={}",
                backend,
                urlencoding::encode(session_id),
                limit
            );
            let text = match http.get(url).send().await {
                Ok(response) => response.text().await.unwrap_or_default(),
                Err(error) => format!("backend request failed: {error}"),
            };
            response_text(trunc(&text, 15_000))
        }
        "bridge_session_status" => {
            let session_id = args
                .get("sessionId")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if !owns_session(state, principal, session_id).await {
                return response_error("session does not belong to this authenticated user");
            }
            let url = format!("{}/session/{}", backend, urlencoding::encode(session_id));
            let text = match http.get(url).send().await {
                Ok(response) => response.text().await.unwrap_or_default(),
                Err(error) => format!("backend request failed: {error}"),
            };
            response_text(trunc(&text, 8_000))
        }
        "bridge_prompt" | "bridge_prompt_async" => {
            let user_prompt = args
                .get("prompt")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if user_prompt.is_empty() {
                return response_error("prompt is required");
            }
            let session_id = args
                .get("sessionId")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if !session_id.is_empty() && !owns_session(state, principal, &session_id).await {
                return response_error("session does not belong to this authenticated user");
            }
            let prompt = if session_id.is_empty() {
                format!(
                    "{}\n\nUser request: {}",
                    include_str!("../system_prompt.md"),
                    user_prompt
                )
            } else {
                user_prompt.to_string()
            };
            let mut session_id = session_id;
            if session_id.is_empty() {
                let title = format!("{}-{}", APP_NAME, now_millis());
                if let Ok(response) = http
                    .post(format!("{backend}/session"))
                    .json(&json!({"title": title}))
                    .send()
                    .await
                {
                    if let Ok(value) = response.json::<Value>().await {
                        session_id = value
                            .get("id")
                            .and_then(Value::as_str)
                            .or_else(|| {
                                value
                                    .get("data")
                                    .and_then(|data| data.get("id"))
                                    .and_then(Value::as_str)
                            })
                            .unwrap_or_default()
                            .to_string();
                    }
                }
                if session_id.is_empty() {
                    return response_error("failed to create backend session");
                }
                remember_session(state, principal, &session_id).await;
            }

            let directory = match valid_directory(args.get("directory").and_then(Value::as_str)) {
                Ok(directory) => directory,
                Err(error) => return response_error(error),
            };
            let mut body = json!({"parts": [{"type": "text", "text": prompt}]});
            if let Some(agent) = args.get("agent").and_then(Value::as_str) {
                body["agent"] = json!(agent);
            }
            if let Some(model) = args.get("model").and_then(Value::as_str) {
                if let Some((provider, model_id)) = model.split_once('/') {
                    body["model"] = json!({"providerID": provider, "modelID": model_id});
                }
            }
            let query = format!("?directory={}", urlencoding::encode(&directory));
            if name == "bridge_prompt_async" {
                let url = format!(
                    "{}/session/{}/prompt_async{}",
                    backend,
                    urlencoding::encode(&session_id),
                    query
                );
                return match http.post(url).json(&body).send().await {
                    Ok(response) if response.status().is_success() => {
                        response_text(format!("Async request sent for session {session_id}"))
                    }
                    Ok(response) => {
                        response_error(format!("backend returned status {}", response.status()))
                    }
                    Err(error) => response_error(format!("backend request failed: {error}")),
                };
            }

            let url = format!(
                "{}/session/{}/message{}",
                backend,
                urlencoding::encode(&session_id),
                query
            );
            match http.post(url).json(&body).send().await {
                Ok(response) => {
                    let text = response.text().await.unwrap_or_default();
                    if let Ok(value) = serde_json::from_str::<Value>(&text) {
                        if let Some(parts) = value.get("parts").and_then(Value::as_array) {
                            let answer = parts
                                .iter()
                                .filter(|part| {
                                    part.get("type").and_then(Value::as_str) == Some("text")
                                })
                                .filter_map(|part| part.get("text").and_then(Value::as_str))
                                .collect::<Vec<_>>()
                                .join("\n");
                            if !answer.is_empty() {
                                return response_text(format!("Session:{session_id}\n{answer}"));
                            }
                        }
                    }
                    response_text(format!("Session:{}\n{}", session_id, trunc(&text, 8_000)))
                }
                Err(error) => response_error(format!("backend request failed: {error}")),
            }
        }
        "shell" => {
            let command = args
                .get("command")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if command.is_empty() {
                return response_error("command is required");
            }
            let directory = match valid_directory(args.get("directory").and_then(Value::as_str)) {
                Ok(directory) => directory,
                Err(error) => return response_error(error),
            };
            let output = run_bash(command, &directory, Duration::from_secs(30)).await;
            response_text(format!(
                "dir:{}\nexit:{:?}\nSTDOUT:\n{}\nSTDERR:\n{}",
                directory,
                output.code,
                trunc(&output.stdout, 15_000),
                trunc(&output.stderr, 4_000)
            ))
        }
        "bridge_agent_prompt" => {
            let prompt = args
                .get("prompt")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if prompt.is_empty() {
                return response_error("prompt is required");
            }
            let sandbox = args
                .get("sandbox")
                .and_then(Value::as_str)
                .unwrap_or("read-only");
            if !matches!(
                sandbox,
                "read-only" | "workspace-write" | "danger-full-access"
            ) {
                return response_error(
                    "sandbox must be read-only, workspace-write, or danger-full-access",
                );
            }
            let directory = match valid_directory(args.get("directory").and_then(Value::as_str)) {
                Ok(directory) => directory,
                Err(error) => return response_error(error),
            };
            response_text(trunc(&run_agent(prompt, &directory, sandbox).await, 25_000))
        }
        "browser" => {
            let action = args.get("action").and_then(Value::as_str).unwrap_or("tabs");
            let cdp = "http://127.0.0.1:9222";
            let text = match action {
                "tabs" => match http.get(format!("{cdp}/json/list")).send().await {
                    Ok(response) => response.text().await.unwrap_or_default(),
                    Err(error) => format!("browser request failed: {error}"),
                },
                "new" | "navigate" => {
                    let url = args
                        .get("url")
                        .and_then(Value::as_str)
                        .unwrap_or("about:blank");
                    if let Err(error) = safe_browser_url(url) {
                        return response_error(error);
                    }
                    match http
                        .put(format!("{}/json/new?{}", cdp, urlencoding::encode(url)))
                        .send()
                        .await
                    {
                        Ok(response) => response.text().await.unwrap_or_default(),
                        Err(error) => format!("browser request failed: {error}"),
                    }
                }
                "close" => {
                    let target_id = args
                        .get("targetId")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    if target_id.is_empty() {
                        return response_error("targetId is required");
                    }
                    match http
                        .get(format!(
                            "{}/json/close/{}",
                            cdp,
                            urlencoding::encode(target_id)
                        ))
                        .send()
                        .await
                    {
                        Ok(response) => response.text().await.unwrap_or_default(),
                        Err(error) => format!("browser request failed: {error}"),
                    }
                }
                "snapshot" => run_browser_script("snapshot", &[]).await,
                "click" => {
                    let selector = args
                        .get("selector")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    if selector.is_empty() {
                        return response_error("selector is required");
                    }
                    run_browser_script("click", &[selector]).await
                }
                "fill" => {
                    let selector = args
                        .get("selector")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let value = args
                        .get("value")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    if selector.is_empty() {
                        return response_error("selector is required");
                    }
                    run_browser_script("fill", &[selector, value]).await
                }
                "evaluate" => {
                    let expression = args
                        .get("expression")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    if expression.is_empty() {
                        return response_error("expression is required");
                    }
                    run_browser_script("evaluate", &[expression]).await
                }
                _ => return response_error(format!("unknown browser action: {action}")),
            };
            response_text(trunc(&text, 15_000))
        }
        _ => response_error(format!("unknown tool: {name}")),
    }
}

async fn handle_rpc(
    state: &AppState,
    principal: &str,
    request: RpcRequest,
) -> Result<Value, (i64, String)> {
    if request.jsonrpc != "2.0" {
        return Err((-32600, "jsonrpc must be 2.0".to_string()));
    }
    match request.method.as_str() {
        "initialize" => Ok(json!({
            "protocolVersion": "2025-03-26",
            "capabilities": {"tools": {"listChanged": true}},
            "serverInfo": {"name": APP_NAME, "version": env!("CARGO_PKG_VERSION")}
        })),
        "tools/list" => Ok(json!({"tools": tools_list()})),
        "tools/call" => {
            let params = request.params.unwrap_or_else(|| json!({}));
            let name = params
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if name.is_empty() {
                return Err((-32602, "tool name is required".to_string()));
            }
            let args = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            Ok(call_tool(state, principal, name, &args).await)
        }
        "ping" => Ok(json!({})),
        method => Err((-32601, format!("method not found: {method}"))),
    }
}

async fn process_mcp(
    state: State<AppState>,
    headers: HeaderMap,
    path_token: Option<String>,
    Json(request): Json<RpcRequest>,
) -> Response {
    let id = request.id.clone().unwrap_or(Value::Null);
    let principal = match authenticated_principal(&state, &headers, path_token.as_deref()).await {
        Some(principal) => principal,
        None => return unauthorized_response(id),
    };
    match handle_rpc(&state, &principal, request).await {
        Ok(result) => Json(json!({"jsonrpc": "2.0", "id": id, "result": result})).into_response(),
        Err((code, message)) => rpc_error(id, code, message),
    }
}

async fn mcp_handler(
    state: State<AppState>,
    headers: HeaderMap,
    body: Json<RpcRequest>,
) -> Response {
    process_mcp(state, headers, None, body).await
}

async fn mcp_handler_path(
    state: State<AppState>,
    Path(token): Path<String>,
    headers: HeaderMap,
    body: Json<RpcRequest>,
) -> Response {
    process_mcp(state, headers, Some(token), body).await
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();
    let tokens = configured_tokens();
    if tokens.is_empty() && !oauth_enabled() && !allow_unauthenticated() {
        eprintln!("MCP_TOKEN, MCP_TOKENS, or complete OAuth configuration is required. Set MCP_ALLOW_UNAUTHENTICATED=true only for local development.");
        std::process::exit(2);
    }

    let host = env::var("MCP_HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
    let port = env::var("MCP_PORT")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(3000);
    let address = format!("{host}:{port}");
    let state = AppState::default();
    let app = Router::new()
        .route("/", get(index))
        .route("/health", get(health))
        .route(
            "/.well-known/oauth-protected-resource",
            get(oauth_protected_resource_metadata),
        )
        .route(
            "/.well-known/oauth-authorization-server",
            get(oauth_authorization_server_metadata),
        )
        .route(
            "/oauth/authorize",
            get(oauth_authorize_get).post(oauth_authorize_post),
        )
        .route("/oauth/token", post(oauth_token))
        .route("/mcp", post(mcp_handler))
        .route("/mcp/:token", post(mcp_handler_path))
        .layer(DefaultBodyLimit::max(MAX_REQUEST_BYTES))
        .with_state(state);

    println!("{APP_NAME} listening on http://{address}");
    let listener = tokio::net::TcpListener::bind(&address)
        .await
        .expect("failed to bind listener");
    axum::serve(listener, app).await.expect("server error");
}

#[cfg(test)]
mod tests {
    use super::{constant_time_equal, safe_browser_url, trunc};

    #[test]
    fn compares_tokens_without_accepting_prefixes() {
        assert!(constant_time_equal("token", "token"));
        assert!(!constant_time_equal("token", "token-extra"));
        assert!(!constant_time_equal("token", "Token"));
    }

    #[test]
    fn limits_text_by_characters() {
        assert_eq!(trunc("hello", 10), "hello");
        assert_eq!(trunc("héllo", 3), "hél");
    }

    #[test]
    fn only_allows_safe_browser_urls() {
        assert!(safe_browser_url("about:blank").is_ok());
        assert!(safe_browser_url("https://example.com").is_ok());
        assert!(safe_browser_url("file:///etc/passwd").is_err());
        assert!(safe_browser_url("javascript:alert(1)").is_err());
    }
}
