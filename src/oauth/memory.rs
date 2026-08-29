use crate::{
    config::TrustProxy,
    state::{AppState, OAuthClient},
    util::{now_seconds, token_fingerprint},
};
use axum::http::HeaderMap;
use std::{
    collections::HashMap,
    hash::Hash,
    net::{IpAddr, SocketAddr},
    time::Duration,
};

pub(super) async fn cache_client(state: &AppState, client_id: String, client: OAuthClient) {
    let now = now_seconds();
    let mut clients = state.oauth_clients.write().await;
    clients.retain(|_, existing| existing.expires_at > now);
    if clients.contains_key(&client_id) || clients.len() < state.config.oauth.max_clients {
        clients.insert(client_id, client);
    }
}

pub(super) async fn store_registered_client(
    state: &AppState,
    client_id: String,
    client: OAuthClient,
) -> Result<(), String> {
    let now = now_seconds();
    {
        let mut clients = state.oauth_clients.write().await;
        clients.retain(|_, existing| existing.expires_at > now);
        if clients.len() >= state.config.oauth.max_clients {
            return Err("dynamic client registration capacity is temporarily full".to_string());
        }
        clients.insert(client_id, client);
    }
    state.persist_durable().await
}

pub(super) fn insert_bounded<K, V, F>(
    map: &mut HashMap<K, V>,
    key: K,
    value: V,
    max_entries: usize,
    expires_at: F,
) where
    K: Clone + Eq + Hash,
    F: Fn(&V) -> u64,
{
    let now = now_seconds();
    map.retain(|_, existing| expires_at(existing) > now);
    if map.len() >= max_entries {
        let remove_count = map.len().saturating_add(1).saturating_sub(max_entries);
        let mut oldest = map
            .iter()
            .map(|(existing_key, existing)| (existing_key.clone(), expires_at(existing)))
            .collect::<Vec<_>>();
        oldest.sort_unstable_by_key(|(_, expiry)| *expiry);
        for (oldest_key, _) in oldest.into_iter().take(remove_count) {
            map.remove(&oldest_key);
        }
    }
    map.insert(key, value);
}

pub(crate) fn spawn_cleanup_task(state: AppState) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(60));
        loop {
            interval.tick().await;
            cleanup_expired(&state).await;
        }
    });
}

pub(super) async fn cleanup_expired(state: &AppState) {
    let now = now_seconds();
    state
        .oauth_codes
        .write()
        .await
        .retain(|_, value| value.expires_at > now);
    state
        .oauth_clients
        .write()
        .await
        .retain(|_, value| value.expires_at > now);
    state
        .oauth_access_tokens
        .write()
        .await
        .retain(|_, value| value.expires_at > now);
    state
        .oauth_refresh_tokens
        .write()
        .await
        .retain(|_, value| value.expires_at > now);

    let cutoff = now.saturating_sub(state.config.oauth.login_window.as_secs());
    let mut failed = state.failed_logins.lock().await;
    failed.retain(|_, attempts| {
        while attempts
            .front()
            .is_some_and(|timestamp| *timestamp <= cutoff)
        {
            attempts.pop_front();
        }
        !attempts.is_empty()
    });
    drop(failed);
    if let Err(error) = state.persist_durable().await {
        tracing::warn!(%error, "failed to persist OAuth cleanup");
    }
}

pub(super) fn effective_client_ip(
    state: &AppState,
    peer: SocketAddr,
    headers: &HeaderMap,
) -> IpAddr {
    effective_client_ip_for(state.config.trust_proxy, peer, headers)
}

pub(super) fn effective_client_ip_for(
    trust_proxy: TrustProxy,
    peer: SocketAddr,
    headers: &HeaderMap,
) -> IpAddr {
    if trust_proxy == TrustProxy::Cloudflare
        && peer.ip().is_loopback()
        && let Some(ip) = headers
            .get("cf-connecting-ip")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.trim().parse::<IpAddr>().ok())
    {
        return ip;
    }
    peer.ip()
}

pub(super) fn login_rate_limit_keys(
    state: &AppState,
    peer: SocketAddr,
    headers: &HeaderMap,
    username: &str,
) -> Vec<(String, usize)> {
    let ip = effective_client_ip(state, peer, headers);
    let base = state.config.oauth.max_failed_logins;
    vec![
        (format!("login:source:{ip}"), base),
        (
            format!("login:user:{}", token_fingerprint(username)),
            base.saturating_mul(2),
        ),
        ("login:global".to_string(), base.saturating_mul(16)),
    ]
}

pub(super) fn registration_rate_limit_keys(
    state: &AppState,
    peer: SocketAddr,
    headers: &HeaderMap,
) -> Vec<(String, usize)> {
    let ip = effective_client_ip(state, peer, headers);
    let base = state.config.oauth.max_failed_logins;
    vec![
        (format!("dcr:source:{ip}"), base),
        ("dcr:global".to_string(), base.saturating_mul(16)),
    ]
}

pub(super) async fn any_rate_limited(state: &AppState, keys: &[(String, usize)]) -> bool {
    for (key, limit) in keys {
        if rate_limited(state, key, *limit).await {
            return true;
        }
    }
    false
}

async fn rate_limited(state: &AppState, key: &str, limit: usize) -> bool {
    let now = now_seconds();
    let cutoff = now.saturating_sub(state.config.oauth.login_window.as_secs());
    let mut failed = state.failed_logins.lock().await;
    let attempts = failed.entry(key.to_string()).or_default();
    while attempts
        .front()
        .is_some_and(|timestamp| *timestamp <= cutoff)
    {
        attempts.pop_front();
    }
    attempts.len() >= limit
}

pub(super) async fn record_rate_event(state: &AppState, keys: &[(String, usize)]) {
    let mut failed = state.failed_logins.lock().await;
    for (key, _) in keys {
        if !failed.contains_key(key) && failed.len() >= state.config.oauth.max_login_buckets {
            let oldest_key = failed
                .iter()
                .min_by_key(|(_, attempts)| attempts.front().copied().unwrap_or_default())
                .map(|(existing_key, _)| existing_key.clone());
            if let Some(oldest_key) = oldest_key {
                failed.remove(&oldest_key);
            }
        }
        failed
            .entry(key.clone())
            .or_default()
            .push_back(now_seconds());
    }
}

pub(super) async fn clear_login_success(state: &AppState, keys: &[(String, usize)]) {
    let mut failed = state.failed_logins.lock().await;
    // Do not clear the global bucket on one successful login.
    for (key, _) in keys.iter().filter(|(key, _)| !key.ends_with(":global")) {
        failed.remove(key);
    }
}

pub(super) fn refresh_token_key(token: &str) -> String {
    token_fingerprint(token)
}
