use crate::{
    config::Config,
    durable::{DurableSnapshot, DurableStore},
    limits::ToolGate,
    util::token_fingerprint,
};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::BufReader,
    process::{Child, ChildStdin, ChildStdout},
    sync::{Mutex, OnceCell, RwLock},
};

const ACCESS_TOKEN_FINGERPRINT_PREFIX: &str = "sha256:";

#[derive(Debug, Clone)]
pub struct AuthorizationCode {
    pub client_id: String,
    pub redirect_uri: String,
    pub resource: String,
    pub scope: String,
    pub code_challenge: String,
    pub expires_at: u64,
    pub principal: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OAuthClientKind {
    ClientMetadataDocument,
    DynamicRegistration,
    LegacyPreRegistered,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthClient {
    pub client_name: String,
    pub redirect_uris: Vec<String>,
    pub grant_types: Vec<String>,
    pub response_types: Vec<String>,
    pub token_endpoint_auth_methods: Vec<String>,
    pub kind: OAuthClientKind,
    pub expires_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthAccessToken {
    pub principal: String,
    pub resource: String,
    pub expires_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthRefreshToken {
    pub client_id: String,
    pub principal: String,
    pub resource: String,
    pub scope: String,
    pub expires_at: u64,
}

#[derive(Debug, Clone)]
pub struct Principal(pub String);

pub struct BrowserWorker {
    pub(crate) child: Child,
    pub(crate) stdin: ChildStdin,
    pub(crate) stdout: BufReader<ChildStdout>,
    pub(crate) next_id: u64,
}

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub http: Client,
    pub sessions: Arc<RwLock<HashMap<String, VecDeque<String>>>>,
    pub oauth_codes: Arc<RwLock<HashMap<String, AuthorizationCode>>>,
    pub oauth_clients: Arc<RwLock<HashMap<String, OAuthClient>>>,
    /// Access tokens use SHA-256 fingerprint keys in memory and on disk.
    pub oauth_access_tokens: Arc<RwLock<HashMap<String, OAuthAccessToken>>>,
    /// Refresh tokens are keyed by SHA-256 fingerprint, never by plaintext token.
    pub oauth_refresh_tokens: Arc<RwLock<HashMap<String, OAuthRefreshToken>>>,
    pub failed_logins: Arc<Mutex<HashMap<String, VecDeque<u64>>>>,
    pub shell_slots: ToolGate,
    pub browser_slots: ToolGate,
    pub backend_slots: ToolGate,
    pub node_path: Arc<OnceCell<Option<String>>>,
    pub browser_worker: Arc<Mutex<Option<BrowserWorker>>>,
    /// Serializes durable mutations through both persistence and publication.
    pub(crate) durable_mutations: Arc<Mutex<()>>,
    durable: Arc<DurableStore>,
}

impl AppState {
    pub fn new(config: Config) -> Result<Self, String> {
        let http = Client::builder()
            .timeout(Duration::from_secs(120))
            .pool_idle_timeout(Duration::from_secs(90))
            .pool_max_idle_per_host(32)
            .tcp_keepalive(Duration::from_secs(30))
            .tcp_nodelay(true)
            .build()
            .map_err(|error| format!("failed to build HTTP client: {error}"))?;
        let shell_slots = ToolGate::new(config.process.shell_concurrency);
        let browser_slots = ToolGate::new(config.process.browser_concurrency);
        let durable = Arc::new(DurableStore::new(config.state_file.clone()));
        let persisted = durable.load()?;

        let mut sessions = persisted.sessions;
        for owned in sessions.values_mut() {
            while owned.len() > config.max_sessions_per_principal {
                owned.pop_front();
            }
        }

        Ok(Self {
            config: Arc::new(config),
            http,
            sessions: Arc::new(RwLock::new(sessions)),
            oauth_codes: Default::default(),
            oauth_clients: Arc::new(RwLock::new(persisted.dcr_clients)),
            oauth_access_tokens: Arc::new(RwLock::new(persisted.access_tokens)),
            oauth_refresh_tokens: Arc::new(RwLock::new(persisted.refresh_tokens)),
            failed_logins: Default::default(),
            shell_slots,
            browser_slots,
            backend_slots: ToolGate::new(4),
            node_path: Arc::new(OnceCell::new()),
            browser_worker: Default::default(),
            durable_mutations: Default::default(),
            durable,
        })
    }

    pub async fn remember_session(&self, principal: &str, session_id: &str) -> Result<(), String> {
        let guard = self.durable_mutations.clone().lock_owned().await;
        if self.config.state_file.is_none() {
            let mut sessions = self.sessions.write().await;
            let owned = sessions.entry(principal.to_string()).or_default();
            remember_bounded(owned, session_id, self.config.max_sessions_per_principal);
            return Ok(());
        }
        let state = self.clone();
        let principal = principal.to_string();
        let session_id = session_id.to_string();
        // Once persistence starts, cancellation must not interrupt publication.
        tokio::spawn(async move {
            let _guard = guard;
            let mut snapshot = state.durable_snapshot().await;
            let owned = snapshot.sessions.entry(principal).or_default();
            remember_bounded(owned, &session_id, state.config.max_sessions_per_principal);
            let snapshot = state.persist_snapshot(snapshot).await?;
            *state.sessions.write().await = snapshot.sessions;
            Ok(())
        })
        .await
        .map_err(|error| format!("session persistence task failed: {error}"))?
    }

    pub async fn owns_session(&self, principal: &str, session_id: &str) -> bool {
        self.sessions
            .read()
            .await
            .get(principal)
            .is_some_and(|sessions| sessions.iter().any(|existing| existing == session_id))
    }

    /// Call while holding durable_mutations; prepare changes in this snapshot.
    pub(crate) async fn durable_snapshot(&self) -> DurableSnapshot {
        DurableSnapshot {
            version: 1,
            sessions: self.sessions.read().await.clone(),
            access_tokens: self.oauth_access_tokens.read().await.clone(),
            refresh_tokens: self.oauth_refresh_tokens.read().await.clone(),
            dcr_clients: self
                .oauth_clients
                .read()
                .await
                .iter()
                .filter(|(_, client)| client.kind == OAuthClientKind::DynamicRegistration)
                .map(|(id, client)| (id.clone(), client.clone()))
                .collect(),
        }
    }

    /// Publish the changed maps only after this succeeds, with the mutation gate held.
    pub(crate) async fn persist_snapshot(
        &self,
        snapshot: DurableSnapshot,
    ) -> Result<DurableSnapshot, String> {
        self.durable.save_owned(snapshot).await
    }
}

pub(crate) fn access_token_lookup_key(token: &str) -> String {
    format!(
        "{ACCESS_TOKEN_FINGERPRINT_PREFIX}{}",
        token_fingerprint(token)
    )
}

fn remember_bounded(owned: &mut VecDeque<String>, session_id: &str, limit: usize) {
    if let Some(index) = owned.iter().position(|existing| existing == session_id) {
        owned.remove(index);
    }
    owned.push_back(session_id.to_string());
    while owned.len() > limit {
        owned.pop_front();
    }
}

#[cfg(test)]
mod tests {
    use super::{access_token_lookup_key, remember_bounded};
    use std::collections::VecDeque;

    #[test]
    fn session_history_is_bounded_and_deduplicated() {
        let mut sessions = VecDeque::new();
        remember_bounded(&mut sessions, "one", 2);
        remember_bounded(&mut sessions, "two", 2);
        remember_bounded(&mut sessions, "three", 2);
        assert_eq!(
            sessions,
            VecDeque::from(["two".to_string(), "three".to_string()])
        );
        remember_bounded(&mut sessions, "two", 2);
        assert_eq!(
            sessions,
            VecDeque::from(["three".to_string(), "two".to_string()])
        );
    }

    #[test]
    fn persisted_access_token_keys_are_one_way_fingerprints() {
        let token = "mcp_access_secret-canary";
        let key = access_token_lookup_key(token);
        assert!(key.starts_with("sha256:"));
        assert!(!key.contains(token));
    }
}
