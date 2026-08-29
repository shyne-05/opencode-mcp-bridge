use super::{
    OAuthAuthorizeRequest,
    client::{
        client_allows_redirect, is_loopback_redirect_host, resolve_oauth_client,
        validate_redirect_uri_syntax,
    },
};
use crate::state::{AppState, OAuthClient};
use axum::{
    Json,
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};
use url::Url;

const SECURITY_HEADERS: [(&str, &str); 4] = [
    (
        "content-security-policy",
        "default-src 'none'; style-src 'unsafe-inline'; form-action 'self'; frame-ancestors 'none'; base-uri 'none'",
    ),
    ("x-frame-options", "DENY"),
    ("referrer-policy", "no-referrer"),
    ("x-content-type-options", "nosniff"),
];

pub(super) fn login_page(
    request: &OAuthAuthorizeRequest,
    client: &OAuthClient,
    error: Option<&str>,
) -> Response {
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
    let redirect_host = request
        .redirect_uri
        .as_deref()
        .and_then(|value| Url::parse(value).ok())
        .and_then(|url| url.host_str().map(str::to_string))
        .unwrap_or_else(|| "unknown".to_string());
    let client_id_host = request
        .client_id
        .as_deref()
        .and_then(|value| Url::parse(value).ok())
        .and_then(|url| url.host_str().map(str::to_string));
    let client_origin = client_id_host
        .map(|host| {
            format!(
                "<p><small>Client metadata: {}</small></p>",
                html_escape(&host)
            )
        })
        .unwrap_or_default();
    let localhost_warning = if request
        .redirect_uri
        .as_deref()
        .and_then(|value| Url::parse(value).ok())
        .is_some_and(|url| url.scheme() == "http" && is_loopback_redirect_host(&url))
    {
        "<p><strong>Warning:</strong> this client redirects to a local application. Only approve if you recognize it.</p>"
    } else {
        ""
    };
    let body = format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>MCP Bridge authorization</title></head><body><main><h1>Authorize {client_name}</h1><p>Sign in to allow <strong>{client_name}</strong> to access this MCP Bridge.</p>{client_origin}<p><small>Redirect after approval: {redirect_host}</small></p>{localhost_warning}{error}<form method=\"post\" action=\"/oauth/authorize\">{hidden}<label>Username<br><input name=\"username\" autocomplete=\"username\" required></label><br><label>Password<br><input type=\"password\" name=\"password\" autocomplete=\"current-password\" required></label><br><button type=\"submit\">Authorize</button></form></main></body></html>",
        client_name = html_escape(&client.client_name),
        redirect_host = html_escape(&redirect_host),
    );
    with_security_headers(
        (
            StatusCode::OK,
            [(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/html; charset=utf-8"),
            )],
            body,
        )
            .into_response(),
    )
}

pub(super) async fn authorization_error(
    state: &AppState,
    request: &OAuthAuthorizeRequest,
    description: &str,
) -> Response {
    if let (Some(client_id), Some(redirect_uri), Some(issuer)) = (
        request.client_id.as_deref(),
        request.redirect_uri.as_deref(),
        state.config.oauth.public_url.as_deref(),
    ) && let Ok(client) = resolve_oauth_client(state, client_id).await
        && validate_redirect_uri_syntax(redirect_uri).is_ok()
        && client_allows_redirect(&client, redirect_uri)
    {
        let mut parameters = vec![
            "error=invalid_request".to_string(),
            format!("error_description={}", urlencoding::encode(description)),
            format!("iss={}", urlencoding::encode(issuer)),
        ];
        if let Some(state_value) = request.state.as_deref() {
            parameters.push(format!("state={}", urlencoding::encode(state_value)));
        }
        let separator = if redirect_uri.contains('?') { '&' } else { '?' };
        return oauth_redirect(format!("{redirect_uri}{separator}{}", parameters.join("&")));
    }
    oauth_json(
        StatusCode::BAD_REQUEST,
        json!({"error": "invalid_request", "error_description": description}),
    )
}

pub(super) fn registration_error(error: &str, description: &str) -> Response {
    oauth_json(
        StatusCode::BAD_REQUEST,
        json!({"error": error, "error_description": description}),
    )
}

pub(super) fn oauth_token_error(error: &str, description: &str) -> Response {
    oauth_json(
        StatusCode::BAD_REQUEST,
        json!({"error": error, "error_description": description}),
    )
}

pub(super) fn oauth_json(status: StatusCode, body: Value) -> Response {
    let mut response = (
        status,
        [
            (header::CACHE_CONTROL, HeaderValue::from_static("no-store")),
            (header::PRAGMA, HeaderValue::from_static("no-cache")),
        ],
        Json(body),
    )
        .into_response();
    add_security_headers(&mut response);
    response
}

pub(super) fn oauth_redirect(location: String) -> Response {
    match HeaderValue::try_from(location) {
        Ok(location) => {
            let mut response = (StatusCode::FOUND, [(header::LOCATION, location)]).into_response();
            add_security_headers(&mut response);
            response
        }
        Err(_) => oauth_json(
            StatusCode::BAD_REQUEST,
            json!({"error": "invalid_request", "error_description": "invalid redirect URI"}),
        ),
    }
}

pub(super) fn with_security_headers(mut response: Response) -> Response {
    add_security_headers(&mut response);
    response
}

fn add_security_headers(response: &mut Response) {
    for (name, value) in SECURITY_HEADERS {
        let name = header::HeaderName::from_static(name);
        let value = HeaderValue::from_static(value);
        response.headers_mut().insert(name, value);
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
