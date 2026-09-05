use crate::{
    state::{OAuthAccessToken, OAuthClient, OAuthClientKind, OAuthRefreshToken},
    util::now_seconds,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, VecDeque},
    fs::{self, OpenOptions},
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::sync::{Mutex, OwnedMutexGuard};

const STATE_VERSION: u32 = 1;
const ACCESS_TOKEN_FINGERPRINT_PREFIX: &str = "sha256:";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DurableSnapshot {
    #[serde(default = "state_version")]
    pub version: u32,
    #[serde(default)]
    pub access_tokens: HashMap<String, OAuthAccessToken>,
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
    write_lock: Arc<Mutex<()>>,
}

impl DurableStore {
    pub fn new(path: Option<PathBuf>) -> Self {
        Self {
            path,
            write_lock: Arc::new(Mutex::new(())),
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
        state.access_tokens.retain(|key, token| {
            key.starts_with(ACCESS_TOKEN_FINGERPRINT_PREFIX) && token.expires_at > now
        });
        state
            .refresh_tokens
            .retain(|_, token| token.expires_at > now);
        state.dcr_clients.retain(|_, client| {
            client.expires_at > now && client.kind == OAuthClientKind::DynamicRegistration
        });
        Ok(state)
    }

    /// Callers serialize preparation through publication with their mutation gate.
    /// Return the saved snapshot so publication needs no second full-state clone.
    pub async fn save_owned(&self, snapshot: DurableSnapshot) -> Result<DurableSnapshot, String> {
        let Some(path) = self.path.clone() else {
            return Ok(snapshot);
        };
        let guard = self.write_lock.clone().lock_owned().await;
        write_snapshot(path, guard, snapshot).await
    }
}

async fn write_snapshot(
    path: PathBuf,
    guard: OwnedMutexGuard<()>,
    mut snapshot: DurableSnapshot,
) -> Result<DurableSnapshot, String> {
    snapshot.version = STATE_VERSION;
    tokio::task::spawn_blocking(move || {
        // Keep writer ownership until file replacement finishes, even on cancellation.
        let _guard = guard;
        write_atomic(&path, &snapshot)?;
        Ok(snapshot)
    })
    .await
    .map_err(|error| format!("durable-state writer task failed: {error}"))?
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
    let mut options = OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(&tmp).map_err(|error| {
        format!(
            "failed to create durable state '{}': {error}",
            tmp.display()
        )
    })?;
    let mut writer = BufWriter::new(file);
    serde_json::to_writer(&mut writer, snapshot).map_err(|error| {
        format!(
            "failed to encode durable state '{}': {error}",
            tmp.display()
        )
    })?;
    writer
        .flush()
        .map_err(|error| format!("failed to write durable state '{}': {error}", tmp.display()))?;
    writer
        .get_ref()
        .sync_all()
        .map_err(|error| format!("failed to sync durable state '{}': {error}", tmp.display()))?;
    drop(writer);
    replace_state_file(&tmp, path)?;
    Ok(())
}

fn replace_state_file(tmp: &Path, path: &Path) -> Result<(), String> {
    // Rust's rename replaces existing files on Unix and Windows. Keep the old
    // snapshot in place until the replacement succeeds, without a backup gap.
    fs::rename(tmp, path).map_err(|error| {
        format!(
            "failed to atomically replace durable state '{}': {error}",
            path.display()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{OAuthAccessToken, OAuthRefreshToken};
    use tempfile::tempdir;

    fn snapshot(principal: &str) -> DurableSnapshot {
        let mut snapshot = DurableSnapshot::default();
        snapshot.access_tokens.insert(
            "sha256:hashed-access-token-key".into(),
            OAuthAccessToken {
                principal: principal.into(),
                resource: "resource".into(),
                expires_at: u64::MAX,
            },
        );
        snapshot.refresh_tokens.insert(
            "hashed-refresh-token-key".into(),
            OAuthRefreshToken {
                client_id: "client".into(),
                principal: principal.into(),
                resource: "resource".into(),
                scope: "mcp".into(),
                expires_at: u64::MAX,
            },
        );
        snapshot
    }

    #[tokio::test]
    async fn durable_state_round_trips_without_plaintext_oauth_tokens() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("state.json");
        let store = DurableStore::new(Some(path.clone()));
        store.save_owned(snapshot("user")).await.unwrap();
        let raw = fs::read_to_string(&path).unwrap();
        assert!(!raw.contains("mcp_access_"));
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
        assert!(
            loaded
                .access_tokens
                .contains_key("sha256:hashed-access-token-key")
        );
        assert!(
            loaded
                .refresh_tokens
                .contains_key("hashed-refresh-token-key")
        );
    }

    #[tokio::test]
    async fn durable_state_replaces_existing_snapshot() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("state.json");
        let store = DurableStore::new(Some(path));
        store.save_owned(snapshot("first")).await.unwrap();
        store.save_owned(snapshot("second")).await.unwrap();
        let loaded = store.load().unwrap();
        assert_eq!(
            loaded
                .access_tokens
                .get("sha256:hashed-access-token-key")
                .map(|token| token.principal.as_str()),
            Some("second")
        );
    }

    #[test]
    fn failed_replacement_keeps_previous_snapshot() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("trạng thái [bridge].json");
        let missing = dir.path().join("missing.json");
        fs::write(&path, b"previous snapshot").unwrap();

        assert!(replace_state_file(&missing, &path).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"previous snapshot");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn replacement_does_not_move_existing_directory() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("state.json");
        let tmp = dir.path().join("new-state.json");
        fs::create_dir(&path).unwrap();
        fs::write(path.join("keep.txt"), b"existing content").unwrap();
        fs::write(&tmp, b"new snapshot").unwrap();

        assert!(replace_state_file(&tmp, &path).is_err());
        assert!(path.is_dir());
        assert_eq!(
            fs::read(path.join("keep.txt")).unwrap(),
            b"existing content"
        );
        assert_eq!(fs::read(&tmp).unwrap(), b"new snapshot");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 2);
    }

    #[tokio::test]
    async fn memory_only_store_returns_the_owned_snapshot_without_waiting_for_a_writer() {
        let store = DurableStore::new(None);
        let _held = store.write_lock.lock().await;
        let snapshot = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            store.save_owned(snapshot("memory-only")),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            snapshot.access_tokens["sha256:hashed-access-token-key"].principal,
            "memory-only"
        );
    }

    #[test]
    fn cancelled_save_keeps_writer_lock_until_blocking_write_finishes() {
        use tokio::{
            runtime::Builder,
            sync::oneshot,
            time::{Duration, timeout},
        };

        let runtime = Builder::new_multi_thread()
            .worker_threads(1)
            .max_blocking_threads(1)
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let dir = tempdir().unwrap();
            let store = Arc::new(DurableStore::new(Some(dir.path().join("state.json"))));
            let (ready_tx, ready_rx) = oneshot::channel();
            let (release_tx, release_rx) = std::sync::mpsc::channel();
            let blocker = tokio::task::spawn_blocking(move || {
                ready_tx.send(()).unwrap();
                let _ = release_rx.recv();
            });
            ready_rx.await.unwrap();

            let writing_store = store.clone();
            let saving = tokio::spawn(async move {
                writing_store
                    .save_owned(snapshot("cancelled-request"))
                    .await
            });
            timeout(Duration::from_secs(2), async {
                while store.write_lock.try_lock().is_ok() {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            saving.abort();
            assert!(saving.await.unwrap_err().is_cancelled());
            // The queued spawn_blocking writer still owns the lock even though
            // its request future is gone, preventing simultaneous replacements.
            assert!(store.write_lock.try_lock().is_err());
            release_tx.send(()).unwrap();
            blocker.await.unwrap();
            let finished = timeout(Duration::from_secs(5), store.write_lock.lock())
                .await
                .unwrap();
            drop(finished);
            assert_eq!(
                store.load().unwrap().access_tokens["sha256:hashed-access-token-key"].principal,
                "cancelled-request"
            );
        });
    }
}
