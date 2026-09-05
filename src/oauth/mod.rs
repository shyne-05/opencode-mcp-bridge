mod client;
mod memory;
mod response;

use crate::{
    state::{
        AppState, AuthorizationCode, OAuthAccessToken, OAuthClient, OAuthClientKind,
        OAuthRefreshToken, access_token_lookup_key,
    },
    util::{constant_time_equal, now_seconds, pkce_challenge, random_token},
};
use axum::{
    Json,
    extract::{ConnectInfo, Form, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use client::{valid_pkce_verifier, validate_authorization_request, validate_redirect_uri_syntax};
use memory::{
    any_rate_limited, clear_login_success, insert_bounded, login_rate_limit_keys,
    record_rate_event, refresh_token_key, registration_rate_limit_keys, store_registered_client,
};
use response::{
    authorization_error, login_page, oauth_json, oauth_redirect, oauth_token_error,
    registration_error, with_security_headers,
};
use serde::Deserialize;
use serde_json::json;
use std::net::SocketAddr;

pub(crate) use memory::spawn_cleanup_task;

#[derive(Clone, Default, Deserialize)]
pub struct OAuthAuthorizeRequest {
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
pub struct OAuthLoginForm {
    #[serde(flatten)]
    request: OAuthAuthorizeRequest,
    username: String,
    password: String,
}

#[derive(Default, Deserialize)]
pub struct OAuthTokenRequest {
    grant_type: Option<String>,
    code: Option<String>,
    redirect_uri: Option<String>,
    client_id: Option<String>,
    code_verifier: Option<String>,
    refresh_token: Option<String>,
    resource: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct DynamicClientRegistrationRequest {
    redirect_uris: Vec<String>,
    client_name: Option<String>,
    grant_types: Option<Vec<String>>,
    response_types: Option<Vec<String>>,
    token_endpoint_auth_method: Option<String>,
    application_type: Option<String>,
}

pub async fn protected_resource_metadata(State(state): State<AppState>) -> Response {
    let Some(issuer) = state
        .config
        .oauth
        .public_url
        .as_ref()
        .filter(|_| state.config.oauth.enabled())
    else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Some(resource) = state.config.oauth.public_resource() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    Json(json!({
        "resource": resource,
        "authorization_servers": [issuer],
        "scopes_supported": ["mcp:tools", "mcp", "offline_access"],
        "bearer_methods_supported": ["header"]
    }))
    .into_response()
}

pub async fn authorization_server_metadata(State(state): State<AppState>) -> Response {
    let Some(issuer) = state
        .config
        .oauth
        .public_url
        .as_ref()
        .filter(|_| state.config.oauth.enabled())
    else {
        return StatusCode::NOT_FOUND.into_response();
    };
    Json(json!({
        "issuer": issuer,
        "authorization_endpoint": format!("{issuer}/oauth/authorize"),
        "token_endpoint": format!("{issuer}/oauth/token"),
        "registration_endpoint": format!("{issuer}/oauth/register"),
        "response_types_supported": ["code"],
        "grant_types_supported": ["authorization_code", "refresh_token"],
        "token_endpoint_auth_methods_supported": ["none"],
        "client_id_metadata_document_supported": true,
        "authorization_response_iss_parameter_supported": true,
        "code_challenge_methods_supported": ["S256"],
        "scopes_supported": ["mcp:tools", "mcp", "offline_access"]
    }))
    .into_response()
}

pub async fn register(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(request): Json<DynamicClientRegistrationRequest>,
) -> Response {
    if !state.config.oauth.enabled() {
        return oauth_json(
            StatusCode::SERVICE_UNAVAILABLE,
            json!({"error": "temporarily_unavailable", "error_description": "OAuth is not configured"}),
        );
    }

    let registration_keys = registration_rate_limit_keys(&state, peer, &headers);
    if any_rate_limited(&state, &registration_keys).await {
        return with_security_headers(
            (
                StatusCode::TOO_MANY_REQUESTS,
                [(header::RETRY_AFTER, HeaderValue::from_static("60"))],
                Json(json!({
                    "error": "temporarily_unavailable",
                    "error_description": "too many dynamic client registration attempts"
                })),
            )
                .into_response(),
        );
    }
    record_rate_event(&state, &registration_keys).await;

    let application_type = request.application_type.as_deref().unwrap_or("web");
    if !matches!(application_type, "web" | "native") {
        return registration_error(
            "invalid_client_metadata",
            "application_type must be web or native",
        );
    }
    if request.redirect_uris.is_empty() || request.redirect_uris.len() > 32 {
        return registration_error(
            "invalid_redirect_uri",
            "redirect_uris must contain between 1 and 32 entries",
        );
    }
    for redirect_uri in &request.redirect_uris {
        if let Err(error) = validate_redirect_uri_syntax(redirect_uri) {
            return registration_error("invalid_redirect_uri", &error);
        }
    }

    let grant_types = request.grant_types.unwrap_or_else(|| {
        vec![
            "authorization_code".to_string(),
            "refresh_token".to_string(),
        ]
    });
    if grant_types.is_empty()
        || grant_types
            .iter()
            .any(|grant| !matches!(grant.as_str(), "authorization_code" | "refresh_token"))
        || !grant_types
            .iter()
            .any(|grant| grant == "authorization_code")
    {
        return registration_error(
            "invalid_client_metadata",
            "only authorization_code and refresh_token grant types are supported",
        );
    }

    let response_types = request
        .response_types
        .unwrap_or_else(|| vec!["code".to_string()]);
    if response_types.as_slice() != ["code"] {
        return registration_error(
            "invalid_client_metadata",
            "response_types must contain only code",
        );
    }

    let token_method = request
        .token_endpoint_auth_method
        .unwrap_or_else(|| "none".to_string());
    if token_method != "none" {
        return registration_error(
            "invalid_client_metadata",
            "this authorization server supports only public clients with token_endpoint_auth_method=none",
        );
    }

    let client_name = request
        .client_name
        .unwrap_or_else(|| "MCP Client".to_string())
        .trim()
        .chars()
        .take(200)
        .collect::<String>();
    if client_name.is_empty() {
        return registration_error("invalid_client_metadata", "client_name must not be empty");
    }

    let client_id = random_token("mcp_dcr");
    let issued_at = now_seconds();
    let client = OAuthClient {
        client_name: client_name.clone(),
        redirect_uris: request.redirect_uris.clone(),
        grant_types: grant_types.clone(),
        response_types: response_types.clone(),
        token_endpoint_auth_methods: vec!["none".to_string()],
        kind: OAuthClientKind::DynamicRegistration,
        expires_at: issued_at + state.config.oauth.dcr_client_ttl,
    };
    if let Err(error) = store_registered_client(&state, client_id.clone(), client).await {
        return registration_error("temporarily_unavailable", &error);
    }

    oauth_json(
        StatusCode::CREATED,
        json!({
            "client_id": client_id,
            "client_id_issued_at": issued_at,
            "client_name": client_name,
            "redirect_uris": request.redirect_uris,
            "grant_types": grant_types,
            "response_types": response_types,
            "token_endpoint_auth_method": "none",
            "application_type": application_type
        }),
    )
}

pub async fn authorize_get(
    State(state): State<AppState>,
    Query(request): Query<OAuthAuthorizeRequest>,
) -> Response {
    if !state.config.oauth.enabled() {
        return oauth_json(
            StatusCode::SERVICE_UNAVAILABLE,
            json!({"error": "OAuth is not configured"}),
        );
    }
    match validate_authorization_request(&state, &request).await {
        Ok(validated) => login_page(
            &request,
            &validated.client,
            &validated.redirect_uri,
            &state.config.oauth.username,
            None,
        ),
        Err(error) => authorization_error(&state, &request, &error).await,
    }
}

pub async fn authorize_post(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Form(form): Form<OAuthLoginForm>,
) -> Response {
    if !state.config.oauth.enabled() {
        return oauth_json(
            StatusCode::SERVICE_UNAVAILABLE,
            json!({"error": "OAuth is not configured"}),
        );
    }
    let validated = match validate_authorization_request(&state, &form.request).await {
        Ok(validated) => validated,
        Err(error) => return authorization_error(&state, &form.request, &error).await,
    };

    let login_keys = login_rate_limit_keys(&state, peer, &headers, &form.username);
    if any_rate_limited(&state, &login_keys).await {
        return with_security_headers(
            (
                StatusCode::TOO_MANY_REQUESTS,
                [(header::RETRY_AFTER, HeaderValue::from_static("60"))],
                "Too many failed login attempts. Try again shortly.",
            )
                .into_response(),
        );
    }

    let expected_password = state.config.oauth.password.as_deref().unwrap_or_default();
    if !constant_time_equal(&state.config.oauth.username, &form.username)
        || !constant_time_equal(expected_password, &form.password)
    {
        record_rate_event(&state, &login_keys).await;
        return login_page(
            &form.request,
            &validated.client,
            &validated.redirect_uri,
            &state.config.oauth.username,
            Some("Invalid username or password."),
        );
    }
    clear_login_success(&state, &login_keys).await;

    let code = random_token("mcp_code");
    {
        let mut codes = state.oauth_codes.write().await;
        insert_bounded(
            &mut codes,
            code.clone(),
            AuthorizationCode {
                client_id: validated.client_id,
                redirect_uri: validated.redirect_uri.clone(),
                resource: validated.resource,
                scope: validated.scope,
                code_challenge: form.request.code_challenge.unwrap_or_default(),
                expires_at: now_seconds() + state.config.oauth.code_ttl,
                principal: state.config.oauth.username.clone(),
            },
            state.config.oauth.max_authorization_codes,
            |value| value.expires_at,
        );
    }
    let state_value = form.request.state.unwrap_or_default();
    let issuer = state.config.oauth.public_url.as_deref().unwrap_or_default();
    let separator = if validated.redirect_uri.contains('?') {
        '&'
    } else {
        '?'
    };
    let location = format!(
        "{}{separator}code={}&state={}&iss={}",
        validated.redirect_uri,
        urlencoding::encode(&code),
        urlencoding::encode(&state_value),
        urlencoding::encode(issuer),
    );
    oauth_redirect(location)
}

pub async fn token(
    State(state): State<AppState>,
    Form(request): Form<OAuthTokenRequest>,
) -> Response {
    let guard = state.durable_mutations.clone().lock_owned().await;
    tokio::spawn(async move {
        // Keep the transaction alive through save and publication if HTTP disconnects.
        let _guard = guard;
        exchange_tokens(&state, request).await
    })
    .await
    .unwrap_or_else(|error| {
        tracing::error!(%error, "OAuth token transaction failed");
        oauth_token_error("temporarily_unavailable", "token transaction failed")
    })
}

async fn exchange_tokens(state: &AppState, request: OAuthTokenRequest) -> Response {
    if !state.config.oauth.enabled() {
        return oauth_token_error("temporarily_unavailable", "OAuth is not configured");
    }
    let Some(client_id) = request
        .client_id
        .as_deref()
        .filter(|value| !value.is_empty())
    else {
        return oauth_token_error("invalid_request", "client_id is required");
    };
    let expected_resource = state.config.oauth.public_resource().unwrap_or_default();

    match request.grant_type.as_deref() {
        Some("authorization_code") => {
            let Some(code_value) = request.code.as_deref().filter(|value| !value.is_empty()) else {
                return oauth_token_error("invalid_request", "code is required");
            };
            let code = {
                let codes = state.oauth_codes.read().await;
                codes.get(code_value).cloned()
            };
            let Some(code) = code else {
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
            if !valid_pkce_verifier(verifier)
                || !constant_time_equal(&pkce_challenge(verifier), &code.code_challenge)
            {
                return oauth_token_error("invalid_grant", "PKCE verification failed");
            }

            issue_oauth_tokens(
                state,
                client_id,
                &code.principal,
                &code.resource,
                &code.scope,
                Some(code_value),
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
            let refresh_key = refresh_token_key(refresh_value);
            let refresh = state
                .oauth_refresh_tokens
                .read()
                .await
                .get(&refresh_key)
                .cloned();
            let Some(refresh) = refresh else {
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
                state,
                client_id,
                &refresh.principal,
                &refresh.resource,
                &refresh.scope,
                None,
                Some(&refresh_key),
            )
            .await
        }
        _ => oauth_token_error("unsupported_grant_type", "grant_type is not supported"),
    }
}

async fn issue_oauth_tokens(
    state: &AppState,
    client_id: &str,
    principal: &str,
    resource: &str,
    scope: &str,
    consumed_code: Option<&str>,
    consumed_refresh: Option<&str>,
) -> Response {
    let now = now_seconds();
    let access_token = random_token("mcp_access");
    let refresh_token = random_token("mcp_refresh");
    let access_expires_at = now + state.config.oauth.access_token_ttl;
    let mut snapshot = if state.config.state_file.is_none() {
        crate::durable::DurableSnapshot {
            access_tokens: state.oauth_access_tokens.read().await.clone(),
            refresh_tokens: state.oauth_refresh_tokens.read().await.clone(),
            ..Default::default()
        }
    } else {
        state.durable_snapshot().await
    };
    if let Some(key) = consumed_refresh {
        snapshot.refresh_tokens.remove(key);
    }
    insert_bounded(
        &mut snapshot.access_tokens,
        access_token_lookup_key(&access_token),
        OAuthAccessToken {
            principal: principal.to_string(),
            resource: resource.to_string(),
            expires_at: access_expires_at,
        },
        state.config.oauth.max_access_tokens,
        |value| value.expires_at,
    );
    insert_bounded(
        &mut snapshot.refresh_tokens,
        refresh_token_key(&refresh_token),
        OAuthRefreshToken {
            client_id: client_id.to_string(),
            principal: principal.to_string(),
            resource: resource.to_string(),
            scope: scope.to_string(),
            expires_at: now + state.config.oauth.refresh_token_ttl,
        },
        state.config.oauth.max_refresh_tokens,
        |value| value.expires_at,
    );
    let snapshot = match state.persist_snapshot(snapshot).await {
        Ok(snapshot) => snapshot,
        Err(error) => {
            return oauth_token_error(
                "temporarily_unavailable",
                &format!("failed to persist OAuth tokens: {error}"),
            );
        }
    };
    // Reads see only committed credentials. The mutation gate serializes replay checks.
    *state.oauth_access_tokens.write().await = snapshot.access_tokens;
    *state.oauth_refresh_tokens.write().await = snapshot.refresh_tokens;
    if let Some(code) = consumed_code {
        state.oauth_codes.write().await.remove(code);
    }
    oauth_json(
        StatusCode::OK,
        json!({
            "access_token": access_token,
            "token_type": "Bearer",
            "expires_in": access_expires_at.saturating_sub(now),
            "refresh_token": refresh_token,
            "scope": scope,
            // MCP clients use RFC 8707 Resource Indicators to bind the token
            // to this resource server. Returning it also keeps the token
            // response self-describing for clients that validate the audience.
            "resource": resource
        }),
    )
}

#[cfg(test)]
mod tests;
