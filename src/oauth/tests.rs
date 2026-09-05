use super::{
    client::{
        ClientMetadataDocument, client_metadata_cache_ttl, is_public_ip, legacy_chatgpt_redirect,
        requested_scope, valid_pkce_challenge, valid_pkce_verifier,
        validate_client_metadata_document, validate_client_metadata_url,
        validate_redirect_uri_syntax,
    },
    memory::{effective_client_ip_for, insert_bounded, rate_limited},
    response::redirect_origin,
};
use crate::config::TrustProxy;
use axum::http::{HeaderMap, HeaderValue, header};
use std::{
    collections::{HashMap, VecDeque},
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

#[test]
fn bounded_oauth_maps_replace_without_evicting_another_entry() {
    let mut map = HashMap::from([("old", u64::MAX - 20), ("newer", u64::MAX - 10)]);
    insert_bounded(&mut map, "newer", u64::MAX - 5, 2, |value| *value);
    assert_eq!(
        map,
        HashMap::from([("old", u64::MAX - 20), ("newer", u64::MAX - 5)])
    );
}

#[test]
fn bounded_oauth_maps_expire_entries_before_evicting_live_values() {
    let mut map = HashMap::from([("expired", 0), ("live", u64::MAX - 10)]);
    insert_bounded(&mut map, "new", u64::MAX - 5, 2, |value| *value);
    assert_eq!(
        map,
        HashMap::from([("live", u64::MAX - 10), ("new", u64::MAX - 5)])
    );
}

#[test]
fn bounded_oauth_maps_support_reduced_and_zero_capacity() {
    let mut map = HashMap::from([
        ("oldest", u64::MAX - 40),
        ("old", u64::MAX - 30),
        ("newer", u64::MAX - 20),
        ("newest", u64::MAX - 10),
    ]);
    insert_bounded(&mut map, "new", u64::MAX - 5, 2, |value| *value);
    assert_eq!(
        map,
        HashMap::from([("newest", u64::MAX - 10), ("new", u64::MAX - 5)])
    );
    insert_bounded(&mut map, "last", u64::MAX, 1, |value| *value);
    assert_eq!(map, HashMap::from([("last", u64::MAX)]));
    insert_bounded(&mut map, "disabled", u64::MAX, 0, |value| *value);
    assert!(map.is_empty());
}

#[test]
fn rate_limit_checks_do_not_allocate_unrecorded_buckets() {
    let mut failed = HashMap::new();
    for index in 0..100 {
        assert!(!rate_limited(
            &mut failed,
            &format!("login:source:{index}"),
            6,
            100
        ));
    }
    assert!(failed.is_empty());
    assert!(rate_limited(&mut failed, "disabled", 0, 100));
    assert!(failed.is_empty());
}

#[test]
fn rate_limit_checks_prune_the_window_and_drop_empty_buckets() {
    let mut failed = HashMap::from([
        (
            "login:source:one".to_string(),
            VecDeque::from([99, 100, 101, 102]),
        ),
        ("login:global".to_string(), VecDeque::from([102, 103])),
    ]);
    assert!(rate_limited(&mut failed, "login:source:one", 2, 100));
    assert_eq!(failed["login:source:one"], VecDeque::from([101, 102]));
    assert!(!rate_limited(&mut failed, "login:source:one", 2, 101));
    assert!(!rate_limited(&mut failed, "login:source:one", 2, 102));
    assert!(!failed.contains_key("login:source:one"));
    assert_eq!(failed["login:global"], VecDeque::from([102, 103]));
}

#[test]
fn cache_control_combines_headers_and_case_insensitive_directives() {
    let mut headers = HeaderMap::new();
    headers.append(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, MAX-AGE = \"120\""),
    );
    headers.append(
        header::CACHE_CONTROL,
        HeaderValue::from_static("max-age=90"),
    );
    assert_eq!(client_metadata_cache_ttl(&headers, 300), 90);
    headers.append(header::CACHE_CONTROL, HeaderValue::from_static("No-Store"));
    assert_eq!(client_metadata_cache_ttl(&headers, 300), 0);
}

#[test]
fn cache_control_requires_revalidation_for_no_cache() {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("max-age=120, no-cache"),
    );
    assert_eq!(client_metadata_cache_ttl(&headers, 300), 0);
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("max-age=invalid"),
    );
    assert_eq!(client_metadata_cache_ttl(&headers, 300), 300);
}

#[cfg(test)]
mod persistence_transactions {
    use super::super::{OAuthTokenRequest, memory, token};
    use crate::{
        config::{Config, OAuthConfig, ProcessConfig, Profile, ToolConfig, TrustProxy},
        state::{AppState, AuthorizationCode, OAuthClient, OAuthClientKind},
        util::{now_seconds, pkce_challenge},
    };
    use axum::extract::{Form, State};
    use std::{path::Path, time::Duration};
    use tempfile::tempdir;

    fn state(directory: &Path) -> AppState {
        AppState::new(Config {
            profile: Profile::ServerSecure,
            host: "127.0.0.1".into(),
            port: 0,
            workdir: directory.to_path_buf(),
            backend_url: "http://127.0.0.1:9".into(),
            backend_response_limit: 16_384,
            max_sessions_per_principal: 2,
            browser_script: directory.join("unused.cjs"),
            node_path: None,
            trust_proxy: TrustProxy::None,
            state_file: Some(directory.join("state.json")),
            tokens: vec![],
            allow_unauthenticated: false,
            tools: ToolConfig {
                shell: false,
                browser: false,
            },
            process: ProcessConfig {
                shell_timeout: Duration::from_secs(1),
                browser_timeout: Duration::from_secs(1),
                stdout_limit: 4_096,
                stderr_limit: 4_096,
                shell_concurrency: 1,
                browser_concurrency: 1,
                child_env_allowlist: Default::default(),
            },
            oauth: OAuthConfig {
                public_url: Some("https://bridge.example".into()),
                username: "test-user".into(),
                password: Some("test-password".into()),
                access_token_ttl: 3_600,
                refresh_token_ttl: 86_400,
                code_ttl: 300,
                max_failed_logins: 6,
                max_login_buckets: 16,
                max_authorization_codes: 16,
                max_clients: 16,
                max_access_tokens: 16,
                max_refresh_tokens: 16,
                dcr_client_ttl: 86_400,
                client_metadata_timeout: Duration::from_secs(2),
                client_metadata_max_bytes: 4_096,
                client_metadata_cache_ttl: 300,
                login_window: Duration::from_secs(60),
            },
        })
        .unwrap()
    }

    #[tokio::test]
    async fn unchanged_cleanup_does_not_create_a_state_file() {
        let dir = tempdir().unwrap();
        let state = state(dir.path());
        memory::cleanup_expired(&state).await;
        assert!(!dir.path().join("state.json").exists());
    }

    #[tokio::test]
    async fn failed_session_save_is_reported_and_does_not_publish_ownership() {
        let dir = tempdir().unwrap();
        let state = state(dir.path());
        let path = dir.path().join("state.json");
        std::fs::create_dir(&path).unwrap();
        assert!(state.remember_session("user", "session").await.is_err());
        assert!(!state.owns_session("user", "session").await);
        std::fs::remove_dir(&path).unwrap();
        state.remember_session("user", "session").await.unwrap();
        assert!(state.owns_session("user", "session").await);
        let persisted: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(persisted["sessions"]["user"][0], "session");
    }

    #[test]
    fn cancelled_token_request_finishes_durable_publication() {
        use tokio::{runtime::Builder, sync::oneshot, time::timeout};
        let runtime = Builder::new_multi_thread()
            .worker_threads(1)
            .max_blocking_threads(1)
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let dir = tempdir().unwrap();
            let state = state(dir.path());
            let verifier = "v".repeat(43);
            state.oauth_codes.write().await.insert(
                "test-code".into(),
                AuthorizationCode {
                    client_id: "test-client".into(),
                    redirect_uri: "https://client.example/callback".into(),
                    resource: "https://bridge.example/mcp".into(),
                    scope: "mcp:tools".into(),
                    code_challenge: pkce_challenge(&verifier),
                    expires_at: now_seconds() + 300,
                    principal: "test-user".into(),
                },
            );
            let (ready_tx, ready_rx) = oneshot::channel();
            let (release_tx, release_rx) = std::sync::mpsc::channel();
            let blocker = tokio::task::spawn_blocking(move || {
                ready_tx.send(()).unwrap();
                let _ = release_rx.recv();
            });
            ready_rx.await.unwrap();
            let request = tokio::spawn(token(
                State(state.clone()),
                Form(OAuthTokenRequest {
                    grant_type: Some("authorization_code".into()),
                    code: Some("test-code".into()),
                    redirect_uri: Some("https://client.example/callback".into()),
                    client_id: Some("test-client".into()),
                    code_verifier: Some(verifier),
                    ..Default::default()
                }),
            ));
            timeout(Duration::from_secs(2), async {
                while state.durable_mutations.try_lock().is_ok() {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            request.abort();
            assert!(request.await.unwrap_err().is_cancelled());
            assert!(state.oauth_codes.read().await.contains_key("test-code"));
            assert!(state.oauth_access_tokens.read().await.is_empty());
            assert!(state.durable_mutations.try_lock().is_err());
            memory::cache_client(
                &state,
                "cached-client".into(),
                OAuthClient {
                    client_name: "Concurrent metadata".into(),
                    redirect_uris: vec![],
                    grant_types: vec![],
                    response_types: vec![],
                    token_endpoint_auth_methods: vec![],
                    kind: OAuthClientKind::ClientMetadataDocument,
                    expires_at: now_seconds() + 300,
                },
            )
            .await;
            release_tx.send(()).unwrap();
            blocker.await.unwrap();
            let finished = timeout(Duration::from_secs(5), state.durable_mutations.lock())
                .await
                .unwrap();
            drop(finished);
            assert!(!state.oauth_codes.read().await.contains_key("test-code"));
            assert_eq!(state.oauth_access_tokens.read().await.len(), 1);
            assert_eq!(state.oauth_refresh_tokens.read().await.len(), 1);
            assert!(
                state
                    .oauth_clients
                    .read()
                    .await
                    .contains_key("cached-client")
            );
            let persisted: serde_json::Value =
                serde_json::from_slice(&std::fs::read(dir.path().join("state.json")).unwrap())
                    .unwrap();
            assert_eq!(persisted["access_tokens"].as_object().unwrap().len(), 1);
            assert_eq!(persisted["refresh_tokens"].as_object().unwrap().len(), 1);
        });
    }
}
