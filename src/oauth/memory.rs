use crate::{
    config::TrustProxy,
    state::{AppState, OAuthClient, OAuthClientKind},
    util::{now_seconds, token_fingerprint},
};
use axum::http::HeaderMap;
use std::{
    collections::{HashMap, VecDeque},
    hash::Hash,
    net::{IpAddr, SocketAddr},
    time::Duration,
};

pub(super) async fn cache_client(state: &AppState, client_id: String, client: OAuthClient) {
    let now = now_seconds();
    let mut clients = state.oauth_clients.write().await;
    // Durable clients are removed by the serialized cleanup transaction.
    clients.retain(|_, existing| {
        existing.kind == OAuthClientKind::DynamicRegistration || existing.expires_at > now
    });
    if clients.contains_key(&client_id) || clients.len() < state.config.oauth.max_clients {
        clients.insert(client_id, client);
    }
}

pub(super) async fn store_registered_client(
    state: &AppState,
    client_id: String,
    client: OAuthClient,
) -> Result<(), String> {
    let guard = state.durable_mutations.clone().lock_owned().await;
    let state = state.clone();
    tokio::spawn(async move {
        let _guard = guard;
        let now = now_seconds();
        let mut snapshot = if state.config.state_file.is_none() {
            Default::default()
        } else {
            state.durable_snapshot().await
        };
        let mut clients = state.oauth_clients.write().await;
        if clients
            .values()
            .filter(|client| client.expires_at > now)
            .count()
            >= state.config.oauth.max_clients
        {
            return Err("dynamic client registration capacity is temporarily full".to_string());
        }
        snapshot
            .dcr_clients
            .retain(|_, existing| existing.expires_at > now);
        snapshot
            .dcr_clients
            .insert(client_id.clone(), client.clone());
        state.persist_snapshot(snapshot).await?;
        clients.retain(|_, existing| existing.expires_at > now);
        clients.insert(client_id, client);
        Ok(())
    })
    .await
    .map_err(|error| format!("client registration task failed: {error}"))?
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
    if max_entries == 0 {
        map.clear();
        return;
    }
    let now = now_seconds();
    map.retain(|_, existing| expires_at(existing) > now);
    // A replacement needs no extra capacity and must not evict another live entry.
    map.remove(&key);
    if map.len() >= max_entries {
        let remove_count = map.len() - max_entries + 1;
        if remove_count == 1 {
            // Normal insertion at capacity: scan once and clone only the evicted key.
            let oldest_key = map
                .iter()
                .min_by_key(|(_, existing)| expires_at(existing))
                .map(|(existing_key, _)| existing_key.clone());
            if let Some(oldest_key) = oldest_key {
                map.remove(&oldest_key);
            }
        } else {
            // Also support shrinking an already populated map without sorting every entry.
            let mut oldest = map
                .iter()
                .map(|(existing_key, existing)| (existing_key, expires_at(existing)))
                .collect::<Vec<_>>();
            if remove_count < oldest.len() {
                oldest.select_nth_unstable_by_key(remove_count, |(_, expiry)| *expiry);
            }
            let keys = oldest
                .into_iter()
                .take(remove_count)
                .map(|(existing_key, _)| existing_key.clone())
                .collect::<Vec<_>>();
            for oldest_key in keys {
                map.remove(&oldest_key);
            }
        }
    }
    map.insert(key, value);
}

pub(crate) fn spawn_cleanup_task(state: AppState) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(60));
        loop {
            interval.tick().await;
            cleanup_expired(&state).await;
        }
    })
}

pub(super) async fn cleanup_expired(state: &AppState) {
    let guard = state.durable_mutations.clone().lock_owned().await;
    let state = state.clone();
    if let Err(error) = tokio::spawn(async move {
        let _guard = guard;
        cleanup_transaction(&state).await;
    })
    .await
    {
        tracing::warn!(%error, "OAuth cleanup task failed");
    }
}

async fn cleanup_transaction(state: &AppState) {
    let now = now_seconds();
    state
        .oauth_codes
        .write()
        .await
        .retain(|_, value| value.expires_at > now);
    // Metadata cache and login counters are intentionally memory-only.
    state.oauth_clients.write().await.retain(|_, value| {
        value.kind == OAuthClientKind::DynamicRegistration || value.expires_at > now
    });
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

    let durable_changed = state
        .oauth_access_tokens
        .read()
        .await
        .values()
        .any(|value| value.expires_at <= now)
        || state
            .oauth_refresh_tokens
            .read()
            .await
            .values()
            .any(|value| value.expires_at <= now)
        || state.oauth_clients.read().await.values().any(|value| {
            value.kind == OAuthClientKind::DynamicRegistration && value.expires_at <= now
        });
    if !durable_changed {
        return;
    }
    if state.config.state_file.is_none() {
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
        state
            .oauth_clients
            .write()
            .await
            .retain(|_, value| value.expires_at > now);
        return;
    }
    let mut snapshot = state.durable_snapshot().await;
    snapshot
        .access_tokens
        .retain(|_, value| value.expires_at > now);
    snapshot
        .refresh_tokens
        .retain(|_, value| value.expires_at > now);
    snapshot
        .dcr_clients
        .retain(|_, value| value.expires_at > now);
    let snapshot = match state.persist_snapshot(snapshot).await {
        Ok(snapshot) => snapshot,
        Err(error) => {
            tracing::warn!(%error, "failed to persist OAuth cleanup");
            return;
        }
    };
    *state.oauth_access_tokens.write().await = snapshot.access_tokens;
    *state.oauth_refresh_tokens.write().await = snapshot.refresh_tokens;
    // Do not replace this map: concurrent metadata fetches may have added cache entries.
    state
        .oauth_clients
        .write()
        .await
        .retain(|_, value| value.expires_at > now);
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
    let cutoff = now_seconds().saturating_sub(state.config.oauth.login_window.as_secs());
    let mut failed = state.failed_logins.lock().await;
    keys.iter()
        .any(|(key, limit)| rate_limited(&mut failed, key, *limit, cutoff))
}

pub(super) fn rate_limited(
    failed: &mut HashMap<String, VecDeque<u64>>,
    key: &str,
    limit: usize,
    cutoff: u64,
) -> bool {
    // Reads must not create buckets: only recording an event enforces the bucket limit.
    let Some(attempts) = failed.get_mut(key) else {
        return limit == 0;
    };
    while attempts
        .front()
        .is_some_and(|timestamp| *timestamp <= cutoff)
    {
        attempts.pop_front();
    }
    let limited = attempts.len() >= limit;
    if attempts.is_empty() {
        failed.remove(key);
    }
    limited
}

pub(super) async fn record_rate_event(state: &AppState, keys: &[(String, usize)]) {
    let mut failed = state.failed_logins.lock().await;
    let now = now_seconds();
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
        failed.entry(key.clone()).or_default().push_back(now);
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
