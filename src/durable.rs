use crate::{
    state::{OAuthClient, OAuthClientKind, OAuthRefreshToken},
    util::now_seconds,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, VecDeque},
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};
use tokio::sync::Mutex;

const STATE_VERSION: u32 = 1;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DurableSnapshot {
    #[serde(default = "state_version")]
    pub version: u32,
    #[serde(default)]
    pub refresh_tokens: HashMap<String, OAuthRefreshToken>,
    #[serde(default)]
    pub sessions: HashMap<String, VecDeque<String>>,
    #[serde(default)]
    pub dcr_clients: HashMap<String, OAuthClient>,
}

fn state_version() -> u32 {
    STATE_VERSION
}

#[derive(Debug)]
pub struct DurableStore {
    path: Option<PathBuf>,
    write_lock: Mutex<()>,
}

impl DurableStore {
    pub fn new(path: Option<PathBuf>) -> Self {
        Self {
            path,
            write_lock: Mutex::new(()),
        }
    }

    pub fn load(&self) -> Result<DurableSnapshot, String> {
        let Some(path) = self.path.as_deref() else {
            return Ok(DurableSnapshot::default());
        };
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(DurableSnapshot::default());
            }
            Err(error) => {
                return Err(format!(
                    "failed to read durable state '{}': {error}",
                    path.display()
                ));
            }
        };
        let mut state: DurableSnapshot = serde_json::from_slice(&bytes)
            .map_err(|error| format!("invalid durable state '{}': {error}", path.display()))?;
        if state.version != STATE_VERSION {
            return Err(format!(
                "unsupported durable state version {} in '{}'",
                state.version,
                path.display()
            ));
        }
        let now = now_seconds();
        state
            .refresh_tokens
            .retain(|_, token| token.expires_at > now);
        state.dcr_clients.retain(|_, client| {
            client.expires_at > now && client.kind == OAuthClientKind::DynamicRegistration
        });
        Ok(state)
    }

    pub async fn save(&self, mut snapshot: DurableSnapshot) -> Result<(), String> {
        let Some(path) = self.path.clone() else {
            return Ok(());
        };
        snapshot.version = STATE_VERSION;
        let _guard = self.write_lock.lock().await;
        tokio::task::spawn_blocking(move || write_atomic(&path, &snapshot))
            .await
            .map_err(|error| format!("durable-state writer task failed: {error}"))?
    }
}

fn write_atomic(path: &Path, snapshot: &DurableSnapshot) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("durable state path '{}' has no parent", path.display()))?;
    fs::create_dir_all(parent).map_err(|error| {
        format!(
            "failed to create durable state directory '{}': {error}",
            parent.display()
        )
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(error) = fs::set_permissions(parent, fs::Permissions::from_mode(0o700))
            && error.kind() != std::io::ErrorKind::PermissionDenied
        {
            return Err(format!(
                "failed to secure durable state directory '{}': {error}",
                parent.display()
            ));
        }
    }
    let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
    let bytes = serde_json::to_vec(snapshot)
        .map_err(|error| format!("failed to encode durable state: {error}"))?;
    let mut options = OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&tmp).map_err(|error| {
        format!(
            "failed to create durable state '{}': {error}",
            tmp.display()
        )
    })?;
    file.write_all(&bytes)
        .map_err(|error| format!("failed to write durable state '{}': {error}", tmp.display()))?;
    file.sync_all()
        .map_err(|error| format!("failed to sync durable state '{}': {error}", tmp.display()))?;
    fs::rename(&tmp, path).map_err(|error| {
        format!(
            "failed to atomically replace durable state '{}': {error}",
            path.display()
        )
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::OAuthRefreshToken;
    use tempfile::tempdir;

    #[tokio::test]
    async fn durable_state_round_trips_without_plaintext_refresh_token() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("state.json");
        let store = DurableStore::new(Some(path.clone()));
        let mut snapshot = DurableSnapshot::default();
        snapshot.refresh_tokens.insert(
            "hashed-token-key".into(),
            OAuthRefreshToken {
                client_id: "client".into(),
                principal: "user".into(),
                resource: "resource".into(),
                scope: "mcp".into(),
                expires_at: u64::MAX,
            },
        );
        store.save(snapshot).await.unwrap();
        let raw = fs::read_to_string(&path).unwrap();
        assert!(!raw.contains("mcp_refresh_"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let loaded = store.load().unwrap();
        assert!(loaded.refresh_tokens.contains_key("hashed-token-key"));
    }
}
