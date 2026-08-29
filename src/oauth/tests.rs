use super::{
    client::{
        ClientMetadataDocument, client_metadata_cache_ttl, is_public_ip, legacy_chatgpt_redirect,
        requested_scope, valid_pkce_challenge, valid_pkce_verifier,
        validate_client_metadata_document, validate_client_metadata_url,
        validate_redirect_uri_syntax,
    },
    memory::{effective_client_ip_for, insert_bounded},
    response::redirect_origin,
};
use crate::config::TrustProxy;
use axum::http::{HeaderMap, HeaderValue, header};
use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
};

#[test]
fn validates_client_metadata_urls_without_private_url_forms() {
    assert!(validate_client_metadata_url("https://chatgpt.com/oauth/client.json").is_ok());
    assert!(validate_client_metadata_url("https://example.com/client.json?version=1").is_ok());
    assert!(validate_client_metadata_url("http://example.com/client.json").is_err());
    assert!(validate_client_metadata_url("https://127.0.0.1/client.json").is_err());
    assert!(validate_client_metadata_url("https://example.com/").is_err());
    assert!(validate_client_metadata_url("https://user@example.com/client.json").is_err());
}

#[test]
fn validates_known_chatgpt_cimd_shape() {
    let document: ClientMetadataDocument = serde_json::from_value(serde_json::json!({
        "client_id": "https://chatgpt.com/oauth/client.json",
        "client_uri": "https://chatgpt.com/",
        "redirect_uris": ["https://chatgpt.com/connector_platform_oauth_redirect"],
        "token_endpoint_auth_methods_supported": ["none", "private_key_jwt"],
        "grant_types": ["authorization_code", "refresh_token"],
        "response_types": ["code"],
        "client_name": "ChatGPT",
        "jwks_uri": "https://chatgpt.com/oauth/jwks.json"
    }))
    .unwrap();
    let client =
        validate_client_metadata_document("https://chatgpt.com/oauth/client.json", document, 300)
            .unwrap();
    assert_eq!(client.client_name, "ChatGPT");
    assert!(
        client
            .redirect_uris
            .contains(&"https://chatgpt.com/connector_platform_oauth_redirect".to_string())
    );
}

#[test]
fn validates_redirect_uri_security_and_legacy_chatgpt_callbacks() {
    assert!(validate_redirect_uri_syntax("https://chatgpt.com/callback").is_ok());
    assert!(validate_redirect_uri_syntax("https://example.com:8443/callback").is_ok());
    assert!(validate_redirect_uri_syntax("http://localhost/callback").is_ok());
    assert!(validate_redirect_uri_syntax("http://localhost:3000/callback").is_ok());
    assert!(validate_redirect_uri_syntax("http://127.0.0.1:3000/callback").is_ok());
    assert!(validate_redirect_uri_syntax("http://[::1]:49152/callback").is_ok());
    assert!(validate_redirect_uri_syntax("http://example.com/callback").is_err());
    assert!(validate_redirect_uri_syntax("file:///tmp/callback").is_err());
    assert!(validate_redirect_uri_syntax("com.example.app:/callback").is_err());
    assert!(validate_redirect_uri_syntax("https://user:pass@example.com/callback").is_err());
    assert!(validate_redirect_uri_syntax("https://example.com/callback#fragment").is_err());
    assert!(legacy_chatgpt_redirect(
        "https://chatgpt.com/connector_platform_oauth_redirect"
    ));
    assert!(legacy_chatgpt_redirect(
        "https://chatgpt.com/connector/oauth/abc-123"
    ));
    assert!(!legacy_chatgpt_redirect("https://evil.example/callback"));
}

#[test]
fn oauth_login_csp_origin_serialization_handles_supported_redirects() {
    assert_eq!(
        redirect_origin("https://chatgpt.com/callback"),
        Some("https://chatgpt.com".to_string())
    );
    assert_eq!(
        redirect_origin("https://example.com:8443/callback"),
        Some("https://example.com:8443".to_string())
    );
    assert_eq!(
        redirect_origin("http://127.0.0.1:49152/callback"),
        Some("http://127.0.0.1:49152".to_string())
    );
    assert_eq!(
        redirect_origin("http://[::1]:49152/callback"),
        Some("http://[::1]:49152".to_string())
    );
    assert_eq!(redirect_origin("com.example.app:/callback"), None);
}

#[test]
fn rejects_private_and_reserved_client_metadata_addresses() {
    for value in [
        "127.0.0.1",
        "10.0.0.1",
        "192.168.1.1",
        "169.254.1.1",
        "100.64.0.1",
        "198.18.0.1",
        "::1",
        "fc00::1",
        "fe80::1",
        "2001:db8::1",
    ] {
        assert!(!is_public_ip(value.parse::<IpAddr>().unwrap()), "{value}");
    }
    assert!(is_public_ip("8.8.8.8".parse().unwrap()));
    assert!(is_public_ip("2606:4700:4700::1111".parse().unwrap()));
}

#[test]
fn cache_control_respects_no_store_and_caps_max_age() {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=900"),
    );
    assert_eq!(client_metadata_cache_ttl(&headers, 300), 300);
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    assert_eq!(client_metadata_cache_ttl(&headers, 300), 0);
}

#[test]
fn scopes_support_refresh_friendly_offline_access() {
    assert_eq!(requested_scope(None).unwrap(), "mcp:tools");
    assert_eq!(
        requested_scope(Some("mcp:tools offline_access")).unwrap(),
        "mcp:tools offline_access"
    );
    assert_eq!(
        requested_scope(Some("mcp offline_access")).unwrap(),
        "mcp offline_access"
    );
    assert!(requested_scope(Some("openid email")).is_err());
}

#[test]
fn bounded_oauth_maps_evict_earliest_expiry() {
    let mut map = HashMap::from([
        ("old".to_string(), u64::MAX - 20),
        ("newer".to_string(), u64::MAX - 10),
    ]);
    insert_bounded(&mut map, "new".to_string(), u64::MAX - 5, 2, |value| *value);
    assert_eq!(map.len(), 2);
    assert!(!map.contains_key("old"));
    assert!(map.contains_key("newer"));
    assert!(map.contains_key("new"));
}

#[test]
fn proxy_header_is_trusted_only_from_explicit_loopback_proxy() {
    let mut headers = HeaderMap::new();
    headers.insert("cf-connecting-ip", HeaderValue::from_static("203.0.113.7"));
    let loopback: SocketAddr = "127.0.0.1:1234".parse().unwrap();
    let direct: SocketAddr = "198.51.100.9:1234".parse().unwrap();
    assert_eq!(
        effective_client_ip_for(TrustProxy::None, loopback, &headers),
        loopback.ip()
    );
    assert_eq!(
        effective_client_ip_for(TrustProxy::Cloudflare, loopback, &headers),
        "203.0.113.7".parse::<IpAddr>().unwrap()
    );
    assert_eq!(
        effective_client_ip_for(TrustProxy::Cloudflare, direct, &headers),
        direct.ip()
    );
}

#[test]
fn validates_pkce_shapes() {
    assert!(valid_pkce_challenge(&"a".repeat(43)));
    assert!(!valid_pkce_challenge("short"));
    assert!(valid_pkce_verifier(&"a".repeat(43)));
    assert!(valid_pkce_verifier(&"A-._~9".repeat(10)));
    assert!(!valid_pkce_verifier("short"));
}
