# Realtime / low-latency architecture

MCP Bridge keeps the public MCP surface deliberately stable while optimizing the implementation underneath it. The realtime path therefore preserves the five existing tools and focuses on reducing process startup, connection setup, queueing, and avoidable round trips.

## Current low-latency design

- RMCP Streamable HTTP remains the transport. The bridge returns JSON responses for ordinary tool calls and does not add a parallel proprietary WebSocket protocol.
- The shared Reqwest client keeps backend/CDP HTTP connections warm, enables TCP_NODELAY, and maintains a larger idle connection pool.
- Browser `navigate`, `snapshot`, `click`, `fill`, and `evaluate` calls use a persistent Node/Playwright worker. Playwright and the CDP browser connection are reused across calls instead of being loaded and reconnected for every action.
- Browser `tabs`, `new`, and `close` continue to use the Chrome DevTools HTTP endpoints directly because those operations are already lightweight.
- A dead, timed-out, or protocol-desynchronized browser worker is discarded. The next browser action creates a clean worker instead of reusing uncertain state.
- Browser helper CLI compatibility (`version` and one-shot action mode) remains available for packaging and mixed-version diagnostics.
- Shell commands continue to start in isolated Bash processes with a sanitized environment. A persistent interactive shell is intentionally avoided because it would leak cwd/environment/process state between independent MCP calls.
- Structured latency events are emitted under the `mcp_bridge::latency` tracing target for total MCP tool latency and shell/browser queue/execution latency.

## Why the public tool schema stays unchanged

The maintenance contract is that implementation, OAuth, deployment, and latency improvements should not require clients to learn new tools. This avoids unnecessary frozen/cached tool-schema churn in MCP clients such as ChatGPT.

The public tools remain:

- `bridge_prompt`
- `bridge_read_file`
- `bridge_search`
- `shell` when enabled
- `browser` when enabled

Long-running work should be expressed through the existing tools until a future MCP task/subscription implementation is justified by measured workloads and supported cleanly by the pinned RMCP SDK. Reintroducing the removed async/session wrapper tools is not part of the realtime design.

## Measurement

Run the service with a tracing filter that includes `mcp_bridge::latency`, for example:

```bash
RUST_LOG=info,mcp_bridge::latency=info
```

Useful measurements are:

- total tool latency
- shell/browser queue time
- shell execution time
- browser action time
- browser worker cold-start frequency
- backend/model inference time observed outside bridge overhead

Benchmark p50/p95/p99 rather than optimizing from individual calls. Browser actions after the first worker startup should show the largest improvement because Node startup, Playwright module loading, and CDP connection establishment are removed from the repeated hot path.
