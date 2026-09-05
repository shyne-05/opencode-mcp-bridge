mod common;

use axum::{
    Json, Router,
    extract::{Query, State},
    routing::get,
};
use common::{BridgeProcess, spawn_bridge};
use reqwest::{Client, StatusCode, header};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    sync::{Barrier, Semaphore, mpsc},
    task::{JoinHandle, JoinSet},
    time::timeout,
};

const TOKEN: &str = "backend-limits-test-token";

struct BackendState {
    release: Semaphore,
    active: AtomicUsize,
    peak: AtomicUsize,
    seen: Mutex<HashSet<u64>>,
    started: mpsc::UnboundedSender<()>,
    health_checks: AtomicUsize,
}

struct ActiveRequest(Arc<BackendState>);

impl Drop for ActiveRequest {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::SeqCst);
    }
}

struct MockBackend {
    state: Arc<BackendState>,
    started: mpsc::UnboundedReceiver<()>,
    task: JoinHandle<()>,
    url: String,
}

impl Drop for MockBackend {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn search(
    State(state): State<Arc<BackendState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Json<Value> {
    let id = query["pattern"].parse::<u64>().unwrap();
    let active = state.active.fetch_add(1, Ordering::SeqCst) + 1;
    state.peak.fetch_max(active, Ordering::SeqCst);
    let _active = ActiveRequest(state.clone());
    assert!(state.seen.lock().unwrap().insert(id));
    state.started.send(()).unwrap();
    state.release.acquire().await.unwrap().forget();
    Json(json!([]))
}

impl MockBackend {
    async fn start() -> Self {
        let (started, receiver) = mpsc::unbounded_channel();
        let state = Arc::new(BackendState {
            release: Semaphore::new(0),
            active: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
            seen: Mutex::new(HashSet::new()),
            started,
            health_checks: AtomicUsize::new(0),
        });
        let app = Router::new()
            .route("/find", get(search))
            .route(
                "/global/health",
                get(|State(state): State<Arc<BackendState>>| async move {
                    state.health_checks.fetch_add(1, Ordering::SeqCst);
                    Json(json!({"ok": true}))
                }),
            )
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self {
            state,
            started: receiver,
            task,
            url,
        }
    }

    async fn wait_for_four_active(&mut self) {
        timeout(Duration::from_secs(3), async {
            for _ in 0..4 {
                self.started.recv().await.unwrap();
            }
        })
        .await
        .expect("four requests should reach the backend");
        assert_eq!(self.state.active.load(Ordering::SeqCst), 4);
    }

    async fn bridge(&self) -> BridgeProcess {
        spawn_bridge(|command, _| {
            command
                .env("MCP_PROFILE", "server-secure")
                .env("MCP_TOKEN", TOKEN)
                .env("MCP_STATE_FILE", ":memory:")
                .env("BRIDGE_BACKEND_URL", &self.url);
        })
        .await
    }
}

async fn tool_call(client: Client, base: String, id: u64) -> (u64, Value) {
    let response = client
        .post(format!("{base}/mcp"))
        .bearer_auth(TOKEN)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json, text/event-stream")
        .header("MCP-Protocol-Version", "2026-07-28")
        .header("Mcp-Method", "tools/call")
        .header("Mcp-Name", "bridge_search")
        .json(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": {
                "name": "bridge_search",
                "arguments": {"pattern": id.to_string()},
                "_meta": {
                    "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                    "io.modelcontextprotocol/clientCapabilities": {}
                }
            }
        }))
        .send()
        .await
        .expect("MCP request should complete");
    assert_eq!(response.status(), StatusCode::OK);
    (
        id,
        response.json().await.expect("MCP response should be JSON"),
    )
}

fn tool_error(response: &Value) -> &str {
    assert_eq!(response["result"]["isError"], true, "{response}");
    response["result"]["content"][0]["text"]
        .as_str()
        .expect("tool error should contain text")
}

fn client() -> Client {
    Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .unwrap()
}

#[tokio::test]
async fn backend_overload_is_bounded_without_blocking_health_checks() {
    let mut backend = MockBackend::start().await;
    let bridge = backend.bridge().await;
    let client = client();
    let mut requests = JoinSet::new();
    for id in 0..4 {
        requests.spawn(tool_call(client.clone(), bridge.base_url.clone(), id));
    }
    backend.wait_for_four_active().await;

    let start = Arc::new(Barrier::new(21));
    for id in 4..24 {
        let start = start.clone();
        let client = client.clone();
        let base = bridge.base_url.clone();
        requests.spawn(async move {
            start.wait().await;
            tool_call(client, base, id).await
        });
    }
    start.wait().await;

    // Four calls are running and twelve can wait. The other eight must fail
    // immediately, before any held backend work is allowed to finish.
    let rejected = timeout(Duration::from_secs(2), async {
        let mut rejected = HashSet::new();
        for _ in 0..8 {
            let (id, response) = requests.join_next().await.unwrap().unwrap();
            assert_eq!(
                tool_error(&response),
                "backend is busy; please try again shortly"
            );
            rejected.insert(id);
        }
        rejected
    })
    .await
    .expect("excess requests should receive prompt overload responses");

    let health = timeout(
        Duration::from_secs(2),
        client.get(format!("{}/health", bridge.base_url)).send(),
    )
    .await
    .expect("health checks should bypass the saturated backend tool queue")
    .unwrap();
    assert_eq!(health.status(), StatusCode::OK);
    assert!(backend.state.health_checks.load(Ordering::SeqCst) > 0);
    assert_eq!(backend.state.active.load(Ordering::SeqCst), 4);
    assert_eq!(backend.state.seen.lock().unwrap().len(), 4);

    backend.state.release.add_permits(16);
    let accepted = timeout(Duration::from_secs(4), async {
        let mut accepted = HashSet::new();
        while let Some(result) = requests.join_next().await {
            let (id, response) = result.unwrap();
            assert_eq!(response["result"]["isError"], false, "{response}");
            accepted.insert(id);
        }
        accepted
    })
    .await
    .expect("admitted requests should finish after releasing backend work");

    assert_eq!(accepted.len(), 16);
    assert_eq!(rejected.len(), 8);
    assert_eq!(backend.state.peak.load(Ordering::SeqCst), 4);
    assert_eq!(backend.state.active.load(Ordering::SeqCst), 0);
    let seen = backend.state.seen.lock().unwrap();
    assert_eq!(*seen, accepted);
    assert!(seen.is_disjoint(&rejected));
}

#[tokio::test]
async fn expired_queued_work_never_reaches_the_backend_and_capacity_recovers() {
    let mut backend = MockBackend::start().await;
    let bridge = backend.bridge().await;
    let client = client();
    let mut running = JoinSet::new();
    for id in 0..4 {
        running.spawn(tool_call(client.clone(), bridge.base_url.clone(), id));
    }
    backend.wait_for_four_active().await;

    let mut queued = JoinSet::new();
    for id in 4..16 {
        queued.spawn(tool_call(client.clone(), bridge.base_url.clone(), id));
    }
    timeout(Duration::from_secs(8), async {
        while let Some(result) = queued.join_next().await {
            let (_, response) = result.unwrap();
            assert_eq!(
                tool_error(&response),
                "backend is busy; waiting for an available slot timed out"
            );
        }
    })
    .await
    .expect("queued requests should expire instead of waiting indefinitely");
    assert_eq!(
        *backend.state.seen.lock().unwrap(),
        HashSet::from([0, 1, 2, 3])
    );

    // Refill the queue while the original four calls remain blocked. Exactly
    // twelve fresh requests must fit again after all twelve old waiters expire.
    let mut fresh = JoinSet::new();
    for id in 16..36 {
        fresh.spawn(tool_call(client.clone(), bridge.base_url.clone(), id));
    }
    let rejected = timeout(Duration::from_secs(2), async {
        let mut rejected = HashSet::new();
        for _ in 0..8 {
            let (id, response) = fresh.join_next().await.unwrap().unwrap();
            assert_eq!(
                tool_error(&response),
                "backend is busy; please try again shortly"
            );
            rejected.insert(id);
        }
        rejected
    })
    .await
    .expect("fresh overflow should be rejected while twelve new waiters fit");
    backend.state.release.add_permits(16);
    let accepted = timeout(Duration::from_secs(4), async {
        while let Some(result) = running.join_next().await {
            assert_eq!(result.unwrap().1["result"]["isError"], false);
        }
        let mut accepted = HashSet::new();
        while let Some(result) = fresh.join_next().await {
            let (id, response) = result.unwrap();
            assert_eq!(response["result"]["isError"], false, "{response}");
            accepted.insert(id);
        }
        accepted
    })
    .await
    .expect("fresh requests should succeed after expired work releases capacity");

    let seen = backend.state.seen.lock().unwrap();
    assert_eq!(accepted.len(), 12);
    assert_eq!(seen.len(), 16);
    assert!(accepted.is_subset(&seen));
    assert!(seen.is_disjoint(&rejected));
    assert!((4..16).all(|id| !seen.contains(&id)));
    assert_eq!(backend.state.peak.load(Ordering::SeqCst), 4);
    assert_eq!(backend.state.active.load(Ordering::SeqCst), 0);
}
