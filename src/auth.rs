use crate::{
    state::{AppState, Principal, access_token_lookup_key},
    util::{constant_time_equal, now_seconds},
};
use axum::{
    Json,
    body::Body,
    extract::State,
    http::{HeaderMap, HeaderValue, Request, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use serde_json::json;

pub fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = value.split_once(' ')?;
    if scheme.eq_ignore_ascii_case("bearer") && !token.trim().is_empty() {
        Some(token.trim())
    } else {
        None
    }
}

pub async fn authenticated_principal(
    state: &AppState,
    headers: &HeaderMap,
    path_token: Option<&str>,
) -> Option<Principal> {
    let candidates = [
        path_token,
        headers
            .get("x-mcp-token")
            .and_then(|value| value.to_str().ok()),
        bearer_token(headers),
    ];

    if let Some(principal) = state.config.tokens.iter().find_map(|expected| {
        candidates
            .iter()
            .flatten()
            .any(|candidate| constant_time_equal(&expected.value, candidate))
            .then(|| Principal(expected.id.clone()))
    }) {
        return Some(principal);
    }

    if let Some(principal) = oauth_principal(state, headers).await {
        return Some(principal);
    }

    (state.config.tokens.is_empty()
        && !state.config.oauth.enabled()
        && state.config.allow_unauthenticated)
        .then(|| Principal("local".to_string()))
}

async fn oauth_principal(state: &AppState, headers: &HeaderMap) -> Option<Principal> {
    if !state.config.oauth.enabled() {
        return None;
    }
    let token = bearer_token(headers)?;
    let resource = state.config.oauth.public_resource()?;
    let tokens = state.oauth_access_tokens.read().await;
    let access = tokens
        .get(token)
        .or_else(|| tokens.get(&access_token_lookup_key(token)))?;
    (access.expires_at > now_seconds() && access.resource == resource)
        .then(|| Principal(access.principal.clone()))
}

pub async fn require_mcp_auth(
    State(state): State<AppState>,
    mut request: Request<Body>,
    next: Next,
) -> Response {
    let presented_bearer = bearer_token(request.headers()).is_some();
    let path_token = request
        .uri()
        .path()
        .strip_prefix("/mcp/")
        .filter(|value| !value.is_empty() && !value.contains('/'));
    match authenticated_principal(&state, request.headers(), path_token).await {
        Some(principal) => {
            request.extensions_mut().insert(principal);
            next.run(request).await
        }
        None => unauthorized_response_with_error(&state, presented_bearer),
    }
}

pub fn unauthorized_response(state: &AppState) -> Response {
    unauthorized_response_with_error(state, false)
}

fn bearer_challenge(metadata_url: Option<&str>, invalid_token: bool) -> HeaderValue {
    let mut challenge = metadata_url
        .map(|url| {
            format!(r#"Bearer realm="mcp-bridge", resource_metadata="{url}", scope="mcp:tools""#)
        })
        .unwrap_or_else(|| r#"Bearer realm="mcp-bridge""#.to_string());
    if invalid_token {
        challenge.push_str(r#", error="invalid_token""#);
    }
    HeaderValue::try_from(challenge)
        .unwrap_or_else(|_| HeaderValue::from_static(r#"Bearer realm="mcp-bridge""#))
}

fn unauthorized_response_with_error(state: &AppState, invalid_token: bool) -> Response {
    let metadata_url = state
        .config
        .oauth
        .public_url
        .as_ref()
        .filter(|_| state.config.oauth.enabled())
        .map(|url| format!("{url}/.well-known/oauth-protected-resource"));
    let body = Json(json!({
        "jsonrpc": "2.0",
        "id": null,
        "error": {"code": -32001, "message": "Unauthorized"}
    }));
    (
        StatusCode::UNAUTHORIZED,
        [(
            header::WWW_AUTHENTICATE,
            bearer_challenge(metadata_url.as_deref(), invalid_token),
        )],
        body,
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::{bearer_challenge, bearer_token};
    use axum::http::{HeaderMap, HeaderValue, header};

    #[test]
    fn parses_bearer_case_insensitively() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("bEaReR abc"),
        );
        assert_eq!(bearer_token(&headers), Some("abc"));
    }

    #[test]
    fn bearer_challenge_distinguishes_missing_and_invalid_credentials() {
        let metadata = "https://bridge.example/.well-known/oauth-protected-resource";
        let missing = bearer_challenge(Some(metadata), false);
        let invalid = bearer_challenge(Some(metadata), true);
        let missing = missing.to_str().unwrap();
        let invalid = invalid.to_str().unwrap();

        assert!(missing.contains(
            r#"resource_metadata="https://bridge.example/.well-known/oauth-protected-resource""#
        ));
        assert!(!missing.contains("invalid_token"));
        assert!(invalid.contains(r#"error="invalid_token""#));
    }
}
