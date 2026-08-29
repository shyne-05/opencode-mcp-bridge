mod common;

use common::spawn_bridge;
use reqwest::{Client, StatusCode, header};
use serde_json::{Value, json};

const TOKEN: &str = "integration-test-token";

fn discover_body(id: u64) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "server/discover",
        "params": {
            "_meta": {
                "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                "io.modelcontextprotocol/clientInfo": {"name": "integration-test", "version": "1.0"},
                "io.modelcontextprotocol/clientCapabilities": {}
            }
        }
    })
}

fn first_sse_json(body: &str) -> Value {
    body.lines()
        .find_map(|line| line.strip_prefix("data: {").map(|rest| format!("{{{rest}")))
        .and_then(|line| serde_json::from_str(&line).ok())
        .expect("SSE body should contain a JSON data event")
}

#[tokio::test]
async fn authenticates_and_supports_current_and_legacy_mcp_clients() {
    let bridge = spawn_bridge(|command, _port| {
        command
            .env("MCP_PROFILE", "server-secure")
            .env("MCP_TOKEN", TOKEN);
    })
    .await;
    let client = Client::new();
    let mcp = format!("{}/mcp", bridge.base_url);

    let unauthorized = client
        .post(&mcp)
        .header(header::CONTENT_TYPE, "application/json")
        .json(&json!({"jsonrpc":"2.0","id":1,"method":"ping"}))
        .send()
        .await
        .expect("unauthorized request should complete");
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

    let discover = client
        .post(&mcp)
        .bearer_auth(TOKEN)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json, text/event-stream")
        .header("MCP-Protocol-Version", "2026-07-28")
        .header("Mcp-Method", "server/discover")
        .json(&discover_body(2))
        .send()
        .await
        .expect("discover request should complete");
    assert_eq!(discover.status(), StatusCode::OK);
    let discover: Value = discover.json().await.expect("discover should return JSON");
    assert!(
        discover["result"]["supportedVersions"]
            .as_array()
            .expect("supportedVersions should be an array")
            .iter()
            .any(|version| version == "2026-07-28")
    );

    let path_auth = client
        .post(format!("{mcp}/{TOKEN}"))
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json, text/event-stream")
        .header("MCP-Protocol-Version", "2026-07-28")
        .header("Mcp-Method", "server/discover")
        .json(&discover_body(3))
        .send()
        .await
        .expect("path-token discover should complete");
    assert_eq!(path_auth.status(), StatusCode::OK);

    let initialize = client
        .post(&mcp)
        .bearer_auth(TOKEN)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json, text/event-stream")
        .json(&json!({
            "jsonrpc":"2.0",
            "id":4,
            "method":"initialize",
            "params":{
                "protocolVersion":"2025-03-26",
                "capabilities":{},
                "clientInfo":{"name":"legacy-integration-test","version":"1.0"}
            }
        }))
        .send()
        .await
        .expect("initialize should complete");
    assert_eq!(initialize.status(), StatusCode::OK);
    let session_id = initialize
        .headers()
        .get("mcp-session-id")
        .expect("initialize should return a session id")
        .to_str()
        .expect("session id should be valid text")
        .to_string();
    let initialize_body = initialize
        .text()
        .await
        .expect("initialize body should read");
    let initialize_json = first_sse_json(&initialize_body);
    assert_eq!(initialize_json["result"]["protocolVersion"], "2025-03-26");

    let initialized = client
        .post(&mcp)
        .bearer_auth(TOKEN)
        .header("Mcp-Session-Id", &session_id)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json, text/event-stream")
        .json(&json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
        .send()
        .await
        .expect("initialized notification should complete");
    assert_eq!(initialized.status(), StatusCode::ACCEPTED);

    let tools = client
        .post(&mcp)
        .bearer_auth(TOKEN)
        .header("Mcp-Session-Id", &session_id)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json, text/event-stream")
        .json(&json!({"jsonrpc":"2.0","id":5,"method":"tools/list","params":{}}))
        .send()
        .await
        .expect("tools/list should complete");
    assert_eq!(tools.status(), StatusCode::OK);
    let tools_json = first_sse_json(&tools.text().await.expect("tools body should read"));
    let names = tools_json["result"]["tools"]
        .as_array()
        .expect("tools should be an array")
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect::<Vec<_>>();
    assert!(names.contains(&"bridge_prompt"));
    assert!(names.contains(&"bridge_read_file"));
    assert!(!names.contains(&"shell"));
    assert!(!names.contains(&"browser"));
    assert!(!names.contains(&"desktop_open_app"));
    assert!(!names.contains(&"audio_get_volume"));
    assert!(!names.contains(&"audio_set_volume"));
}
