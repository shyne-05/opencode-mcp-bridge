use reqwest::Client;
use std::{
    fs,
    net::TcpListener,
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::Duration,
};

pub struct BridgeProcess {
    child: Child,
    pub base_url: String,
    cleanup_state_file: PathBuf,
}

impl Drop for BridgeProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_file(&self.cleanup_state_file);
    }
}

pub fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("test port should bind");
    listener.local_addr().expect("test port address").port()
}

pub async fn spawn_bridge<F>(configure: F) -> BridgeProcess
where
    F: FnOnce(&mut Command, u16),
{
    spawn_bridge_at_port(free_port(), configure).await
}

pub async fn spawn_bridge_at_port<F>(port: u16, configure: F) -> BridgeProcess
where
    F: FnOnce(&mut Command, u16),
{
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let cleanup_state_file = manifest_dir
        .join("target")
        .join(format!("test-state-{port}.json"));
    let _ = fs::remove_file(&cleanup_state_file);
    let mut command = Command::new(env!("CARGO_BIN_EXE_mcp-bridge"));
    command
        .env("MCP_HOST", "127.0.0.1")
        .env("MCP_PORT", port.to_string())
        .env("BRIDGE_WORKDIR", &manifest_dir)
        .env("BRIDGE_BACKEND_URL", "http://127.0.0.1:9")
        .env("MCP_STATE_FILE", &cleanup_state_file)
        .env("RUST_LOG", "error")
        .env_remove("MCP_PROFILE")
        .env_remove("MCP_TOKEN")
        .env_remove("MCP_TOKENS")
        .env_remove("MCP_PUBLIC_URL")
        .env_remove("MCP_OAUTH_USERNAME")
        .env_remove("MCP_OAUTH_PASSWORD")
        .env_remove("MCP_OAUTH_ALLOW_INSECURE_HTTP")
        .env_remove("MCP_OAUTH_MAX_FAILED_LOGINS")
        .env_remove("MCP_ALLOW_UNAUTHENTICATED")
        .env_remove("MCP_ENABLE_HOST_TOOLS")
        .env_remove("MCP_ENABLE_SHELL")
        .env_remove("MCP_ENABLE_BROWSER")
        .env_remove("MCP_TRUST_PROXY")
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    configure(&mut command, port);

    let child = command.spawn().expect("bridge test process should start");
    let bridge = BridgeProcess {
        child,
        base_url: format!("http://127.0.0.1:{port}"),
        cleanup_state_file,
    };
    let client = Client::new();
    for _ in 0..80 {
        if client
            .get(format!("{}/", bridge.base_url))
            .send()
            .await
            .is_ok_and(|response| response.status().is_success())
        {
            return bridge;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    drop(bridge);
    panic!("bridge test process did not become ready");
}
