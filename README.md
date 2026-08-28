# MCP Bridge

MCP Bridge is a small Rust server that exposes a configurable agent backend through Streamable HTTP MCP. It can also provide optional host shell, command-line agent, and Chrome automation tools.

The public tool names and configuration use the project name `mcp-bridge`. The backend adapter currently speaks the local HTTP API provided by the OpenCode-compatible agent service described below.

## Features

Core tools are available after the backend is configured:

| Tool | Purpose | Requirement |
| --- | --- | --- |
| `bridge_prompt` | Send a prompt and wait for the response | Agent backend |
| `bridge_prompt_async` | Send a prompt without waiting | Agent backend |
| `bridge_session_messages` | Read a bridge-owned session | Agent backend |
| `bridge_session_status` | Read a bridge-owned session status | Agent backend |
| `bridge_list_sessions` | List sessions owned by the current token | Agent backend |
| `bridge_read_file` | Read a file through the backend | Agent backend |
| `bridge_search` | Search the backend workspace | Agent backend |
| `shell` | Run a host `bash` command | `MCP_ENABLE_HOST_TOOLS=true` |
| `bridge_codex_prompt` | Run the installed command-line agent | `MCP_ENABLE_HOST_TOOLS=true` and Codex CLI |
| `browser` | Control a local Chrome debugging session | `MCP_ENABLE_HOST_TOOLS=true`, Chrome, Node.js, and Playwright |

Host tools are hidden and disabled by default because they can access local files, processes, browser cookies, and desktop applications.

## Requirements

Install only what you need:

- Rust 1.88 or newer: <https://rustup.rs>
- Bash for the optional `shell` tool
- A compatible local agent service for the seven `bridge_*` backend tools. The current adapter uses the service API started by the [OpenCode CLI](https://opencode.ai).
- Node.js 22 or newer and the Codex CLI for `bridge_codex_prompt`:

  ```bash
  npm install --global @openai/codex
  ```

- Google Chrome or Chromium, Node.js, and Playwright for `browser`:

  ```bash
  npm install --global playwright
  ```

- `cloudflared` only when the bridge must be reachable from an external MCP client through a tunnel.

Check the tools you installed:

```bash
rustc --version
cargo --version
node --version
npm --version
bash --version
```

## Install

Clone and build the project:

```bash
git clone https://github.com/shyne-05/opencode-mcp-bridge.git mcp-bridge
cd mcp-bridge
cargo build --release
```

The binary is `target/release/mcp-bridge` on Linux and macOS, or `target/release/mcp-bridge.exe` on Windows.

## Quick start

The following starts the bridge securely on the local machine. Keep this shell open while using the server:

```bash
export BRIDGE_WORKDIR="$PWD"
export BRIDGE_BACKEND_URL="http://127.0.0.1:4097"
export MCP_TOKEN="$(openssl rand -hex 32)"
./target/release/mcp-bridge
```

In another terminal, start the compatible backend service on `127.0.0.1:4097` using its documented command. For the current adapter, that is:

```bash
opencode serve --hostname 127.0.0.1 --port 4097
```

The bridge itself defaults to `127.0.0.1:3000`. It refuses to start without `MCP_TOKEN` or `MCP_TOKENS`, unless the explicit local-development override is set.

## Enable host tools

Only enable these tools when the MCP client and network are trusted:

```bash
export MCP_ENABLE_HOST_TOOLS=true
./target/release/mcp-bridge
```

With host tools enabled:

- `shell` runs `bash -c` on the host.
- `bridge_codex_prompt` runs the Codex CLI inside the requested sandbox mode.
- `browser` can read and modify the Chrome session connected to CDP port `9222`.

The requested working directory must exist inside `BRIDGE_WORKDIR`. Start with the narrowest project directory possible.

## Chrome setup

Start Chrome with a separate profile and CDP enabled. Do not use a profile containing accounts or sensitive cookies unless the MCP client is fully trusted.

Linux:

```bash
google-chrome \
  --remote-debugging-port=9222 \
  --user-data-dir="$HOME/.config/mcp-bridge-chrome"
```

macOS:

```bash
/Applications/Google\ Chrome.app/Contents/MacOS/Google\ Chrome \
  --remote-debugging-port=9222 \
  --user-data-dir="$HOME/.config/mcp-bridge-chrome"
```

Windows PowerShell:

```powershell
& "C:\Program Files\Google\Chrome\Application\chrome.exe" `
  --remote-debugging-port=9222 `
  --user-data-dir="$env:USERPROFILE\.config\mcp-bridge-chrome"
```

Verify CDP before calling `browser`:

```bash
curl --fail http://127.0.0.1:9222/json/list
```

When running a built binary outside the repository directory, set the helper path explicitly:

```bash
export MCP_BROWSER_SCRIPT="$PWD/scripts/browser.cjs"
```

## Authentication

Use one token:

```bash
export MCP_TOKEN="$(openssl rand -hex 32)"
```

Or use named tokens for multiple users:

```bash
export MCP_TOKENS="alice=$(openssl rand -hex 32),bob=$(openssl rand -hex 32)"
```

The preferred request format is a bearer header:

```bash
curl -sS -X POST http://127.0.0.1:3000/mcp \
  -H "Authorization: Bearer $MCP_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}'
```

Clients that cannot set a header may use the token path:

```text
http://127.0.0.1:3000/mcp/<token>
```

Do not put tokens in query strings. A path token can appear in proxy history, browser history, and access logs, so use the bearer header whenever possible.

## Connect an MCP client

For a local client, use:

```text
http://127.0.0.1:3000/mcp
```

Configure the client to send `Authorization: Bearer <your-token>`. If the client only accepts a URL and cannot send headers, use `/mcp/<your-token>` and select no additional authentication in that client.

## Public access through Cloudflare Tunnel

Keep the bridge bound to loopback and let the tunnel be the public edge:

```bash
cloudflared tunnel create mcp-bridge
cloudflared tunnel route dns mcp-bridge your-domain.example.com
```

Create `~/.cloudflared/config.yml`:

```yaml
tunnel: <tunnel-id>
credentials-file: /path/to/<tunnel-id>.json
ingress:
  - hostname: your-domain.example.com
    service: http://127.0.0.1:3000
  - service: http_status:404
```

Then run the named tunnel:

```bash
cloudflared tunnel run mcp-bridge
```

Use HTTPS at the public domain and connect the MCP client to:

```text
https://your-domain.example.com/mcp
```

Do not publish port `9222`, the backend port, or an unauthenticated bridge endpoint.

## Configuration

| Variable | Default | Description |
| --- | --- | --- |
| `MCP_HOST` | `127.0.0.1` | Listen address. Use `0.0.0.0` only inside a protected container/network. |
| `MCP_PORT` | `3000` | Listen port. |
| `MCP_TOKEN` | unset | One bearer token. Required unless local unauthenticated mode is explicitly enabled. |
| `MCP_TOKENS` | unset | Comma-separated tokens, optionally named as `name=token`. Overrides `MCP_TOKEN`. |
| `MCP_ENABLE_HOST_TOOLS` | `false` | Exposes shell, browser, and command-line agent tools. |
| `MCP_ALLOW_UNAUTHENTICATED` | `false` | Local-development escape hatch. Never use for a public service. |
| `BRIDGE_BACKEND_URL` | `http://127.0.0.1:4097` | URL of the compatible agent backend. |
| `BRIDGE_WORKDIR` | `.` | Allowed working-directory root for backend and host commands. |
| `MCP_BROWSER_SCRIPT` | `scripts/browser.cjs` | Path to the tracked Playwright helper. |
| `NODE_PATH` | automatic | Node module search path when Playwright is installed globally. |

## Verify the installation

Check the bridge health and MCP handshake:

```bash
curl -sS http://127.0.0.1:3000/health

curl -sS -X POST http://127.0.0.1:3000/mcp \
  -H "Authorization: Bearer $MCP_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}'

curl -sS -X POST http://127.0.0.1:3000/mcp \
  -H "Authorization: Bearer $MCP_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}'
```

The tools list contains seven tools by default and ten when `MCP_ENABLE_HOST_TOOLS=true`. A request without a valid token must return HTTP `401`.

If the backend is unavailable, `/health` returns HTTP `503`; the bridge can still start, but backend tools will not work until the backend is running.

## Docker

Build and run the bridge container:

```bash
docker build -t mcp-bridge .
docker run --rm \
  -p 3000:3000 \
  -e MCP_TOKEN="$(openssl rand -hex 32)" \
  -e BRIDGE_WORKDIR=/work \
  -v "$PWD:/work:ro" \
  mcp-bridge
```

The image includes Node.js and the Playwright package for the browser helper. Run the compatible backend and Chrome separately, then set `BRIDGE_BACKEND_URL` to a reachable backend URL. The Codex CLI is not bundled; add it to a derived image if `bridge_codex_prompt` is required. Do not enable host tools in a container unless the container isolation, network, and mounted files are understood.

## Security notes

- Authentication is required by default and token comparison is constant-time.
- The default listener is loopback-only and permissive CORS is not enabled.
- Host tools are opt-in and intentionally powerful. Use a dedicated OS user, a separate browser profile, a narrow `BRIDGE_WORKDIR`, HTTPS, and a private network.
- Session IDs are tracked per authenticated token for the lifetime of one bridge process. Restarting the bridge clears this in-memory ownership map.
- This project does not implement OAuth, TLS termination, rate limiting, or account management. Put it behind a trusted HTTPS edge when it is not local-only.
- Never commit `.env` files, tokens, cookies, private keys, or tunnel credentials.

## License

MIT
