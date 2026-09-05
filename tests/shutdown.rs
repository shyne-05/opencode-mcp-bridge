#![cfg(unix)]

use axum::{Router, http::StatusCode, routing::get};
use reqwest::Client;
use std::{net::SocketAddr, process::Stdio, sync::Arc, time::Duration};
use tempfile::TempDir;
use tokio::{
    io::AsyncWriteExt,
    net::{TcpListener, TcpStream},
    process::{Child, Command},
    sync::{Mutex, Notify},
};

static START_LOCK: Mutex<()> = Mutex::const_new(());

struct TestBridge {
    child: Child,
    address: SocketAddr,
    _directory: TempDir,
}

impl TestBridge {
    async fn start(backend_url: &str) -> Self {
        // Retain startup ownership until the new child actually binds its port.
        let _startup = START_LOCK.lock().await;
        let directory = tempfile::tempdir().expect("test directory should be created");
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("test port should bind");
        let address = listener.local_addr().unwrap();
        drop(listener);
        let child = Command::new(env!("CARGO_BIN_EXE_mcp-bridge"))
            .env_clear()
            .env("MCP_HOST", "127.0.0.1")
            .env("MCP_PORT", address.port().to_string())
            .env("MCP_PROFILE", "server-secure")
            .env("MCP_TOKEN", "shutdown-integration-test-token")
            .env("BRIDGE_WORKDIR", directory.path())
            .env("BRIDGE_BACKEND_URL", backend_url)
            .env("MCP_STATE_FILE", directory.path().join("state.json"))
            .env("RUST_LOG", "error")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .expect("isolated bridge should start");
        let mut bridge = Self {
            child,
            address,
            _directory: directory,
        };
        let client = Client::builder()
            .timeout(Duration::from_secs(1))
            .build()
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(status) = bridge.child.try_wait().expect("bridge status should read") {
                    panic!("isolated bridge exited before becoming live: {status}");
                }
                if client
                    .get(bridge.url("/live"))
                    .send()
                    .await
                    .is_ok_and(|response| response.status().is_success())
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("isolated bridge should become live");
        bridge
    }

    fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.address)
    }

    fn signal(&self, signal: i32) {
        let pid = self.child.id().expect("bridge should still be running");
        assert_eq!(unsafe { libc::kill(pid as i32, signal) }, 0);
    }

    async fn expect_clean_exit(&mut self, timeout: Duration) {
        let status = tokio::time::timeout(timeout, self.child.wait())
            .await
            .expect("bridge should exit before shutdown deadline")
            .expect("bridge status should be available");
        assert!(status.success(), "bridge exited with {status}");
    }
}

#[tokio::test]
async fn sigterm_drains_an_in_flight_readiness_request() {
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let app = Router::new().route(
        "/global/health",
        get({
            let started = started.clone();
            let release = release.clone();
            move || {
                let started = started.clone();
                let release = release.clone();
                async move {
                    started.notify_one();
                    release.notified().await;
                    StatusCode::OK
                }
            }
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let backend_url = format!("http://{}", listener.local_addr().unwrap());
    let backend = tokio::spawn(async move { axum::serve(listener, app).await });
    let mut bridge = TestBridge::start(&backend_url).await;
    let ready_url = bridge.url("/ready");
    let request = tokio::spawn(async move {
        Client::builder()
            .timeout(Duration::from_secs(4))
            .build()
            .unwrap()
            .get(ready_url)
            .send()
            .await
            .expect("in-flight readiness response should finish")
    });
    tokio::time::timeout(Duration::from_secs(2), started.notified())
        .await
        .expect("backend should observe the in-flight request");

    bridge.signal(libc::SIGTERM);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        bridge.child.try_wait().unwrap().is_none(),
        "bridge exited before its active request completed",
    );
    release.notify_one();
    let response = request.await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["backend"], true);
    bridge.expect_clean_exit(Duration::from_secs(3)).await;
    backend.abort();
    let _ = backend.await;
}

#[tokio::test]
async fn sigterm_bounds_drain_for_an_incomplete_request_body() {
    let mut bridge = TestBridge::start("http://127.0.0.1:9").await;
    let mut connection = TcpStream::connect(bridge.address).await.unwrap();
    connection
        .write_all(
            b"POST /oauth/token HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: 1000\r\n\r\ngrant_type=",
        )
        .await
        .unwrap();
    // Allow the connection to enter the form-body extractor before signaling.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let started = std::time::Instant::now();
    bridge.signal(libc::SIGTERM);
    bridge.expect_clean_exit(Duration::from_secs(14)).await;
    assert!(
        started.elapsed() >= Duration::from_secs(9),
        "the accepted request should receive the normal drain grace",
    );
}

#[tokio::test]
async fn sigint_exits_cleanly() {
    let mut bridge = TestBridge::start("http://127.0.0.1:9").await;
    bridge.signal(libc::SIGINT);
    bridge.expect_clean_exit(Duration::from_secs(3)).await;
}
