use super::{OAuthAuthorizeRequest, memory::cache_client};
use crate::{
    state::{AppState, OAuthClient, OAuthClientKind},
    util::now_seconds,
};
use axum::http::{HeaderMap, header};
use serde::Deserialize;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use url::{Host, Url};

#[derive(Debug, Deserialize)]
pub(super) struct ClientMetadataDocument {
    pub(super) client_id: String,
    client_name: String,
    redirect_uris: Vec<String>,
    #[serde(default)]
    grant_types: Vec<String>,
    #[serde(default)]
    response_types: Vec<String>,
    token_endpoint_auth_method: Option<String>,
    #[serde(default)]
    token_endpoint_auth_methods_supported: Vec<String>,
}

#[derive(Debug)]
pub(super) struct ValidatedAuthorization {
    pub(super) client_id: String,
    pub(super) redirect_uri: String,
    pub(super) resource: String,
    pub(super) scope: String,
    pub(super) client: OAuthClient,
}

pub(super) async fn validate_authorization_request(
    state: &AppState,
    request: &OAuthAuthorizeRequest,
) -> Result<ValidatedAuthorization, String> {
    if request.response_type.as_deref() != Some("code") {
        return Err("response_type must be code".to_string());
    }
    let client_id = request
        .client_id
        .as_deref()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "client_id is required".to_string())?;
    let client = resolve_oauth_client(state, client_id).await?;
    if !client
        .grant_types
        .iter()
        .any(|grant| grant == "authorization_code")
    {
        return Err("client does not support authorization_code".to_string());
    }
    if !client
        .response_types
        .iter()
        .any(|response| response == "code")
    {
        return Err("client does not support response_type=code".to_string());
    }
    if !client
        .token_endpoint_auth_methods
        .iter()
        .any(|method| method == "none")
    {
        return Err("client does not support public token authentication".to_string());
    }

    let redirect_uri = request
        .redirect_uri
        .as_deref()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "redirect_uri is required".to_string())?;
    validate_redirect_uri_syntax(redirect_uri)?;
    if !client_allows_redirect(&client, redirect_uri) {
        return Err("redirect_uri is not registered for this client".to_string());
    }

    let challenge = request
        .code_challenge
        .as_deref()
        .ok_or_else(|| "code_challenge is required".to_string())?;
    if !valid_pkce_challenge(challenge) {
        return Err("code_challenge must be a valid S256 challenge".to_string());
    }
    if request.code_challenge_method.as_deref() != Some("S256") {
        return Err("code_challenge_method must be S256".to_string());
    }

    let resource = state
        .config
        .oauth
        .public_resource()
        .ok_or_else(|| "MCP_PUBLIC_URL is not configured".to_string())?;
    if request
        .resource
        .as_deref()
        .is_some_and(|value| value != resource)
    {
        return Err("resource does not match MCP_PUBLIC_URL".to_string());
    }
    let scope = requested_scope(request.scope.as_deref())?;
    Ok(ValidatedAuthorization {
        client_id: client_id.to_string(),
        redirect_uri: redirect_uri.to_string(),
        resource,
        scope,
        client,
    })
}

pub(super) async fn resolve_oauth_client(
    state: &AppState,
    client_id: &str,
) -> Result<OAuthClient, String> {
    if client_id == "chatgpt-mcp" {
        return Ok(legacy_chatgpt_client());
    }

    let now = now_seconds();
    {
        let clients = state.oauth_clients.read().await;
        if let Some(client) = clients
            .get(client_id)
            .filter(|client| client.expires_at > now)
        {
            return Ok(client.clone());
        }
    }
    state.oauth_clients.write().await.remove(client_id);

    let metadata_url = validate_client_metadata_url(client_id)?;
    let (client, cacheable) = fetch_client_metadata(state, &metadata_url).await?;
    if cacheable {
        cache_client(state, client_id.to_string(), client.clone()).await;
    }
    Ok(client)
}

fn legacy_chatgpt_client() -> OAuthClient {
    OAuthClient {
        client_name: "ChatGPT (legacy client)".to_string(),
        redirect_uris: vec!["https://chatgpt.com/connector_platform_oauth_redirect".to_string()],
        grant_types: vec![
            "authorization_code".to_string(),
            "refresh_token".to_string(),
        ],
        response_types: vec!["code".to_string()],
        token_endpoint_auth_methods: vec!["none".to_string()],
        kind: OAuthClientKind::LegacyPreRegistered,
        expires_at: u64::MAX,
    }
}

pub(super) fn validate_client_metadata_url(client_id: &str) -> Result<Url, String> {
    let url = Url::parse(client_id)
        .map_err(|_| "unknown client_id; expected an HTTPS Client ID Metadata Document URL or a dynamically registered client_id".to_string())?;
    if url.scheme() != "https"
        || url.path().is_empty()
        || url.path() == "/"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(
            "Client ID Metadata Document client_id must be an HTTPS URL with a non-root path and no credentials or fragment"
                .to_string(),
        );
    }
    match url.host() {
        Some(Host::Domain(domain)) if !domain.is_empty() => Ok(url),
        _ => {
            Err("Client ID Metadata Document client_id must use a public DNS hostname".to_string())
        }
    }
}

async fn fetch_client_metadata(
    state: &AppState,
    metadata_url: &Url,
) -> Result<(OAuthClient, bool), String> {
    let domain = metadata_url
        .host_str()
        .ok_or_else(|| "client metadata URL has no host".to_string())?;
    let port = metadata_url.port_or_known_default().unwrap_or(443);
    let addresses = tokio::time::timeout(
        state.config.oauth.client_metadata_timeout,
        tokio::net::lookup_host((domain, port)),
    )
    .await
    .map_err(|_| "client metadata DNS lookup timed out".to_string())?
    .map_err(|error| format!("client metadata DNS lookup failed: {error}"))?
    .collect::<Vec<_>>();
    if addresses.is_empty() {
        return Err("client metadata hostname did not resolve".to_string());
    }
    if addresses.iter().any(|address| !is_public_ip(address.ip())) {
        return Err(
            "client metadata hostname resolves to a private or reserved network address"
                .to_string(),
        );
    }

    let client = reqwest::Client::builder()
        .timeout(state.config.oauth.client_metadata_timeout)
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .resolve_to_addrs(domain, &addresses)
        .build()
        .map_err(|error| format!("failed to create client metadata HTTP client: {error}"))?;
    let mut response = client
        .get(metadata_url.clone())
        .header(header::ACCEPT, "application/json")
        .send()
        .await
        .map_err(|error| format!("failed to fetch Client ID Metadata Document: {error}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "Client ID Metadata Document returned HTTP {}",
            response.status()
        ));
    }
    if response
        .content_length()
        .is_some_and(|length| length > state.config.oauth.client_metadata_max_bytes as u64)
    {
        return Err("Client ID Metadata Document is too large".to_string());
    }

    let cache_ttl = client_metadata_cache_ttl(
        response.headers(),
        state.config.oauth.client_metadata_cache_ttl,
    );
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| format!("failed to read Client ID Metadata Document: {error}"))?
    {
        if body.len().saturating_add(chunk.len()) > state.config.oauth.client_metadata_max_bytes {
            return Err("Client ID Metadata Document is too large".to_string());
        }
        body.extend_from_slice(&chunk);
    }
    let document: ClientMetadataDocument = serde_json::from_slice(&body)
        .map_err(|error| format!("Client ID Metadata Document is invalid JSON: {error}"))?;
    let client = validate_client_metadata_document(metadata_url.as_str(), document, cache_ttl)?;
    Ok((client, cache_ttl > 0))
}

pub(super) fn validate_client_metadata_document(
    expected_client_id: &str,
    document: ClientMetadataDocument,
    cache_ttl: u64,
) -> Result<OAuthClient, String> {
    if document.client_id != expected_client_id {
        return Err("Client ID Metadata Document client_id does not match its URL".to_string());
    }
    let client_name = document.client_name.trim();
    if client_name.is_empty() || client_name.chars().count() > 200 {
        return Err("Client ID Metadata Document has an invalid client_name".to_string());
    }
    if document.redirect_uris.is_empty() || document.redirect_uris.len() > 32 {
        return Err("Client ID Metadata Document must contain 1 to 32 redirect_uris".to_string());
    }
    for redirect_uri in &document.redirect_uris {
        validate_redirect_uri_syntax(redirect_uri)?;
    }

    let grant_types = if document.grant_types.is_empty() {
        vec!["authorization_code".to_string()]
    } else {
        document.grant_types
    };
    if !grant_types
        .iter()
        .any(|grant| grant == "authorization_code")
    {
        return Err("Client ID Metadata Document does not allow authorization_code".to_string());
    }

    let response_types = if document.response_types.is_empty() {
        vec!["code".to_string()]
    } else {
        document.response_types
    };
    if !response_types.iter().any(|response| response == "code") {
        return Err("Client ID Metadata Document does not allow response_type=code".to_string());
    }

    let mut token_methods = document.token_endpoint_auth_methods_supported;
    if let Some(method) = document.token_endpoint_auth_method {
        token_methods.push(method);
    }
    if token_methods.is_empty() {
        token_methods.push("none".to_string());
    }
    token_methods.sort();
    token_methods.dedup();
    if !token_methods.iter().any(|method| method == "none") {
        return Err(
            "Client ID Metadata Document must support token_endpoint_auth_method=none for this public MCP authorization server"
                .to_string(),
        );
    }

    Ok(OAuthClient {
        client_name: client_name.to_string(),
        redirect_uris: document.redirect_uris,
        grant_types,
        response_types,
        token_endpoint_auth_methods: token_methods,
        kind: OAuthClientKind::ClientMetadataDocument,
        expires_at: now_seconds().saturating_add(cache_ttl),
    })
}

pub(super) fn client_metadata_cache_ttl(headers: &HeaderMap, default_ttl: u64) -> u64 {
    let Some(value) = headers
        .get(header::CACHE_CONTROL)
        .and_then(|value| value.to_str().ok())
    else {
        return default_ttl;
    };
    let directives = value.split(',').map(str::trim).collect::<Vec<_>>();
    if directives
        .iter()
        .any(|directive| directive.eq_ignore_ascii_case("no-store"))
    {
        return 0;
    }
    directives
        .iter()
        .find_map(|directive| {
            directive
                .strip_prefix("max-age=")
                .and_then(|value| value.trim_matches('"').parse::<u64>().ok())
        })
        .map(|ttl| ttl.min(default_ttl))
        .unwrap_or(default_ttl)
}

pub(super) fn client_allows_redirect(client: &OAuthClient, redirect_uri: &str) -> bool {
    if client
        .redirect_uris
        .iter()
        .any(|registered| registered == redirect_uri)
    {
        return true;
    }
    matches!(client.kind, OAuthClientKind::LegacyPreRegistered)
        && legacy_chatgpt_redirect(redirect_uri)
}

pub(super) fn legacy_chatgpt_redirect(redirect_uri: &str) -> bool {
    let Ok(url) = Url::parse(redirect_uri) else {
        return false;
    };
    if url.scheme() != "https" || url.host_str() != Some("chatgpt.com") || url.query().is_some() {
        return false;
    }
    if url.path() == "/connector_platform_oauth_redirect" {
        return true;
    }
    url.path()
        .strip_prefix("/connector/oauth/")
        .is_some_and(|callback_id| {
            !callback_id.is_empty()
                && !callback_id.contains('/')
                && callback_id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
}

pub(super) fn validate_redirect_uri_syntax(redirect_uri: &str) -> Result<(), String> {
    let url =
        Url::parse(redirect_uri).map_err(|_| "redirect_uri is not a valid URL".to_string())?;
    if url.fragment().is_some() || !url.username().is_empty() || url.password().is_some() {
        return Err("redirect_uri must not contain credentials or a fragment".to_string());
    }
    match url.scheme() {
        "https" if url.host().is_some() => Ok(()),
        "http" if is_loopback_redirect_host(&url) => Ok(()),
        _ => Err("redirect_uri must use HTTPS or HTTP on localhost/loopback".to_string()),
    }
}

pub(super) fn is_loopback_redirect_host(url: &Url) -> bool {
    match url.host() {
        Some(Host::Domain(domain)) => domain.eq_ignore_ascii_case("localhost"),
        Some(Host::Ipv4(ip)) => ip.is_loopback(),
        Some(Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}

pub(super) fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => is_public_ipv4(ip),
        IpAddr::V6(ip) => is_public_ipv6(ip),
    }
}

pub(super) fn is_public_ipv4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !(ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_documentation()
        || ip.is_unspecified()
        || ip.is_multicast()
        || a == 0
        || (a == 100 && (64..=127).contains(&b))
        || (a == 192 && b == 0 && c == 0)
        || (a == 198 && matches!(b, 18 | 19))
        || a >= 240)
}

pub(super) fn is_public_ipv6(ip: Ipv6Addr) -> bool {
    if let Some(mapped) = ip.to_ipv4_mapped() {
        return is_public_ipv4(mapped);
    }
    let segments = ip.segments();
    !(ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_multicast()
        || (segments[0] & 0xfe00) == 0xfc00
        || (segments[0] & 0xffc0) == 0xfe80
        || (segments[0] == 0x2001 && segments[1] == 0x0db8))
}

pub(super) fn valid_pkce_challenge(value: &str) -> bool {
    value.len() == 43
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

pub(super) fn valid_pkce_verifier(value: &str) -> bool {
    (43..=128).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~'))
}

pub(super) fn requested_scope(scope: Option<&str>) -> Result<String, String> {
    let raw = scope.unwrap_or("mcp:tools").trim();
    let values = if raw.is_empty() {
        vec!["mcp:tools"]
    } else {
        raw.split_whitespace().collect::<Vec<_>>()
    };
    if values
        .iter()
        .any(|value| !matches!(*value, "mcp:tools" | "mcp" | "offline_access"))
    {
        return Err("unsupported scope".to_string());
    }
    let base = if values.contains(&"mcp") {
        "mcp"
    } else {
        "mcp:tools"
    };
    if values.contains(&"offline_access") {
        Ok(format!("{base} offline_access"))
    } else {
        Ok(base.to_string())
    }
}
