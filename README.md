# MCP Bridge

MCP Bridge is a Rust gateway that connects MCP clients such as ChatGPT to a local agent backend and, when explicitly enabled, to the owner's desktop environment.

Version 0.4 is designed around two goals:

1. **Personal desktop automation**: keep powerful local capabilities such as unrestricted shell access, browser control, application launching, audio control, and coding-agent workflows.
2. **Safe boundaries**: do not leak bridge credentials into child processes, constrain filesystem access to a configured workspace, bound process output/concurrency, terminate timed-out process trees, and keep authentication/session ownership explicit.

The MCP protocol layer uses the official Rust MCP SDK (`rmcp`) and currently negotiates every protocol revision supported by that SDK, including `2026-07-28` and legacy `2025-03-26` clients.

## Tools

Core backend tools are always available when the backend is configured:

| Tool | Purpose |
| --- | --- |
| `bridge_prompt` | Send a prompt to the configured local agent backend and wait for the result |
| `bridge_prompt_async` | Send a prompt without waiting for completion |
| `bridge_session_messages` | Read messages from a bridge-owned backend session |
| `bridge_session_status` | Read status from a bridge-owned backend session |
| `bridge_list_sessions` | List backend sessions owned by the authenticated principal |
| `bridge_read_file` | Read an existing file inside `BRIDGE_WORKDIR` through the backend |
| `bridge_search` | Search the configured backend workspace |

Optional host/desktop tools:

| Tool | Purpose | Switch |
| --- | --- | --- |
| `shell` | Run unrestricted host Bash commands with a sanitized environment, bounded output, timeout, process-tree termination, and concurrency limiting | `MCP_ENABLE_SHELL=true` |
| `bridge_agent_prompt` | Run the configured command-line coding agent | `MCP_ENABLE_AGENT=true` |
| `browser` | Control a local Chrome/Chromium CDP session | `MCP_ENABLE_BROWSER=true` |
| `desktop_open_app` | Launch Flatpak, desktop, or executable applications without shell-string interpolation | `MCP_ENABLE_DESKTOP=true` |
| `audio_get_volume` | Read the default PipeWire/WirePlumber sink volume | `MCP_ENABLE_DESKTOP=true` |
| `audio_set_volume` | Set the default sink volume and optionally unmute it | `MCP_ENABLE_DESKTOP=true` |

`MCP_ENABLE_HOST_TOOLS=true` remains supported for 0.3 compatibility. In the `personal-desktop` profile it enables all optional host/desktop tool groups unless an individual switch overrides the default.

## Requirements

Only install what your deployment uses:

- Rust 1.88 or newer
- Bash for `shell`
- A compatible backend service for the seven `bridge_*` tools
- A command-line agent for `bridge_agent_prompt`
- Google Chrome/Chromium, Node.js, and Playwright for `browser`
- PipeWire/WirePlumber (`wpctl`) for native audio tools
- Flatpak and/or `gtk-launch` for broad application launching support
- `cloudflared` only when an external MCP client must reach the bridge through a tunnel

The backend adapter expects these local HTTP endpoints:

- `/global/health`
- `/session`
- `/session/{id}`
- `/session/{id}/message`
- `/session/{id}/prompt_async`
- `/find`
- `/file/content`

## Build

```bash
git clone https://github.com/shyne-05/opencode-mcp-bridge.git mcp-bridge
cd mcp-bridge
cargo build --release
```

The binary is `target/release/mcp-bridge` on Linux/macOS.

## Quick start: personal desktop

For a trusted personal workstation:

```bash
export MCP_PROFILE=personal-desktop
export BRIDGE_WORKDIR="$HOME"
export BRIDGE_BACKEND_URL="http://127.0.0.1:4097"
export MCP_TOKEN="$(openssl rand -hex 32)"
export MCP_ENABLE_HOST_TOOLS=true
./target/release/mcp-bridge
```

The default listener is `127.0.0.1:3000`.

`BRIDGE_WORKDIR="$HOME"` is appropriate when the goal is to let the assistant work across projects in the user's home directory. Use a narrower project directory when broad access is unnecessary.

## Profiles

### `personal-desktop`

Designed for the owner's workstation. Desktop session variables such as `DISPLAY`, `WAYLAND_DISPLAY`, `XDG_RUNTIME_DIR`, and `DBUS_SESSION_BUS_ADDRESS` may be passed to child processes when present.

Host tools are still opt-in. Use either the legacy master switch:

```bash
export MCP_ENABLE_HOST_TOOLS=true
```

or individual switches:

```bash
export MCP_ENABLE_SHELL=true
export MCP_ENABLE_BROWSER=true
export MCP_ENABLE_AGENT=true
export MCP_ENABLE_DESKTOP=true
```

### `server-secure`

Designed for server/VPS use. Optional host tools default off even if the legacy desktop-oriented master switch is present; enable individual capabilities deliberately.

```bash
export MCP_PROFILE=server-secure
```

## Child-process security model

The bridge itself may contain authentication secrets such as `MCP_OAUTH_PASSWORD`. Shell, browser, agent, and desktop child processes **do not inherit the full bridge environment**.

Instead, the bridge clears each child environment and restores only an allowlist of ordinary runtime variables. Personal-desktop mode includes the desktop/session variables needed to open applications and control audio.

Additional non-secret variables can be explicitly allowed:

```bash
export MCP_CHILD_ENV_ALLOW="MY_TOOL_CONFIG,ANOTHER_SAFE_VARIABLE"
```

Names that look like credentials (`TOKEN`, `SECRET`, `PASSWORD`, `API_KEY`, `AUTH`, `COOKIE`, all `MCP_*`, and `CLOUDFLARE_*`) are rejected from this extension list.

This does not make unrestricted shell harmless: shell commands still run with the operating-system permissions of the bridge user. The boundary prevents accidental credential inheritance; it is not a sandbox.

## Process controls

Default limits:

| Variable | Default | Range / purpose |
| --- | ---: | --- |
| `MCP_SHELL_TIMEOUT_SECONDS` | `30` | Shell timeout, 1–600 seconds |
| `MCP_AGENT_TIMEOUT_SECONDS` | `180` | Agent timeout, 1–1800 seconds |
| `MCP_BROWSER_TIMEOUT_SECONDS` | `30` | Browser/helper timeout, 1–300 seconds |
| `MCP_STDOUT_LIMIT_BYTES` | `1048576` | Captured stdout cap |
| `MCP_STDERR_LIMIT_BYTES` | `262144` | Captured stderr cap |
| `MCP_SHELL_CONCURRENCY` | `2` | Concurrent shell calls |
| `MCP_AGENT_CONCURRENCY` | `2` | Concurrent agent calls |
| `MCP_BROWSER_CONCURRENCY` | `1` | Concurrent browser calls |

Output is bounded while it is being read, so a noisy process cannot allocate unbounded bridge memory. On Unix, timed-out commands are placed in their own process group and the complete group is terminated.

## Browser setup

Install Playwright:

```bash
npm install --global playwright
```

Start Chrome with a dedicated CDP profile:

```bash
google-chrome \
  --remote-debugging-port=9222 \
  --user-data-dir="$HOME/.config/mcp-bridge-chrome"
```

Verify CDP:

```bash
curl --fail http://127.0.0.1:9222/json/list
```

The browser helper supports:

- `tabs`
- `new`
- `navigate`
- `close`
- `snapshot`
- `click`
- `fill`
- `evaluate`

`targetId` can be supplied for page-specific Playwright actions. If omitted, the most recent Playwright page is used. `snapshot` uses the current Playwright ARIA snapshot API and falls back to body text if unavailable. `evaluate` preserves structured JSON values, missing close targets are errors, and the Rust bridge verifies `mcp-browser-helper/2` before invoking the helper. The helper also understands the 0.3 argument layout to reduce mixed-version failure during an upgrade.

Do not expose port `9222` publicly.

## Command-line agent

Set the executable/path and adapter type, not a shell command string:

```bash
# Codex
export MCP_AGENT_COMMAND=codex
export MCP_AGENT_KIND=codex

# or OpenCode
export MCP_AGENT_COMMAND="$HOME/.opencode/bin/opencode"
export MCP_AGENT_KIND=opencode
```

Codex uses `exec --json -C ... --sandbox <mode>`. OpenCode uses `run --format json --dir ...`; `danger-full-access` maps to OpenCode `--auto`, while the other modes keep OpenCode's native permission system. Non-zero exits and timeouts are reported as MCP tool errors. If `MCP_AGENT_KIND` is omitted, the bridge can infer `codex` or `opencode` from an unambiguous executable basename.

## Authentication

### Static bearer token

```bash
export MCP_TOKEN="$(openssl rand -hex 32)"
```

Connect to:

```text
http://127.0.0.1:3000/mcp
```

and send:

```text
Authorization: Bearer <token>
```

### Named tokens

```bash
export MCP_TOKENS="alice=$(openssl rand -hex 32),bob=$(openssl rand -hex 32)"
```

Backend sessions are tracked per authenticated principal.

### URL-path token compatibility

Clients that cannot send authorization headers may still use:

```text
http://127.0.0.1:3000/mcp/<token>
```

Prefer bearer headers. URL tokens can appear in proxy/access history.

### Development-only unauthenticated mode

```bash
export MCP_ALLOW_UNAUTHENTICATED=true
```

The bridge enforces this as loopback-only: startup fails if unauthenticated mode is combined with a non-loopback `MCP_HOST`. Host tools may still be used for local development, but cannot be exposed unauthenticated over the network.

## ChatGPT OAuth

For an external ChatGPT MCP connection, expose the bridge through HTTPS and configure an OAuth origin:

```bash
export MCP_PUBLIC_URL="https://your-domain.example.com"
export MCP_OAUTH_USERNAME="bridge-user"
read -r -s -p "MCP OAuth password: " MCP_OAUTH_PASSWORD
export MCP_OAUTH_PASSWORD
printf '\n'
```

The OAuth password must be at least 12 characters.

Connect ChatGPT to:

```text
https://your-domain.example.com/mcp
```

The built-in OAuth flow provides:

- authorization code flow
- PKCE S256
- resource binding
- standards-based OAuth client discovery with Client ID Metadata Documents (CIMD)
- Dynamic Client Registration (DCR) fallback for older MCP clients
- exact redirect URI validation against verified client metadata
- SSRF-hardened CIMD fetching: public HTTPS DNS only, pinned resolved addresses, no redirects/proxies, bounded size and timeout
- high-entropy authorization/access/refresh tokens
- one-time authorization codes
- refresh-token rotation and `offline_access` scope support
- expired-state cleanup
- failed-login throttling
- no-store OAuth responses and restrictive login-page security headers

Configuration:

| Variable | Default |
| --- | ---: |
| `MCP_OAUTH_ACCESS_TOKEN_TTL` | `3600` seconds |
| `MCP_OAUTH_REFRESH_TOKEN_TTL` | `2592000` seconds |
| `MCP_OAUTH_CODE_TTL` | `300` seconds |
| `MCP_OAUTH_MAX_FAILED_LOGINS` | `6` |
| `MCP_OAUTH_LOGIN_WINDOW_SECONDS` | `60` seconds |
| `MCP_OAUTH_MAX_CLIENTS` | `1024` |
| `MCP_OAUTH_DCR_CLIENT_TTL` | `2592000` seconds |
| `MCP_OAUTH_CLIENT_METADATA_TIMEOUT_SECONDS` | `10` seconds |
| `MCP_OAUTH_CLIENT_METADATA_MAX_BYTES` | `65536` bytes |
| `MCP_OAUTH_CLIENT_METADATA_CACHE_TTL` | `300` seconds |

Current MCP clients can use CIMD without pre-registration: the bridge fetches the HTTPS `client_id` metadata document, verifies that its embedded `client_id` exactly matches the URL, and accepts only redirect URIs declared by that document. Older clients may use the advertised `/oauth/register` Dynamic Client Registration endpoint.

OAuth authorization codes and short-lived access tokens remain in memory. Rotating refresh-token metadata, Dynamic Client Registration records, and bridge-owned backend-session ownership are stored in the durable state file so normal bridge restarts do not force a full OAuth login or orphan resumable backend sessions. Refresh token values are never written to disk; the durable store keys them by SHA-256 fingerprint and writes atomically with restrictive permissions on Unix.

## Cloudflare Tunnel

Keep the bridge on a private/local listener and publish only the bridge HTTP endpoint through the tunnel.

Do **not** publish:

- Chrome CDP port `9222`
- backend port `4097`
- an unauthenticated MCP endpoint

When Cloudflare Tunnel terminates locally, forwarded client IP headers are **not trusted by default**. Set `MCP_TRUST_PROXY=cloudflare` only when the bridge receives traffic from a trusted loopback Cloudflare proxy. Otherwise throttling uses the actual TCP peer address, so a client cannot bypass limits by spoofing `CF-Connecting-IP`.

Example ingress:

```yaml
tunnel: <tunnel-id>
credentials-file: /path/to/<tunnel-id>.json
ingress:
  - hostname: your-domain.example.com
    service: http://127.0.0.1:3000
  - service: http_status:404
```

## Configuration reference

| Variable | Default | Description |
| --- | --- | --- |
| `MCP_PROFILE` | `personal-desktop` | `personal-desktop` or `server-secure` |
| `MCP_HOST` | `127.0.0.1` | Listener address |
| `MCP_PORT` | `3000` | Listener port |
| `BRIDGE_WORKDIR` | `.` | Canonical filesystem root for backend/file/working-directory access |
| `BRIDGE_BACKEND_URL` | `http://127.0.0.1:4097` | Local backend origin |
| `MCP_BACKEND_RESPONSE_LIMIT_BYTES` | `1048576` | Maximum backend response bytes buffered per request |
| `MCP_MAX_SESSIONS_PER_PRINCIPAL` | `256` | Maximum bridge-owned backend session IDs retained per authenticated principal |
| `MCP_TOKEN` | unset | Single bearer token |
| `MCP_TOKENS` | unset | Named/comma-separated bearer tokens |
| `MCP_ALLOW_UNAUTHENTICATED` | `false` | Development-only authentication bypass; enforced loopback-only |
| `MCP_ENABLE_HOST_TOOLS` | `false` | Legacy personal-desktop master switch |
| `MCP_ENABLE_SHELL` | profile/master default | Enable `shell` |
| `MCP_ENABLE_BROWSER` | profile/master default | Enable `browser` |
| `MCP_ENABLE_AGENT` | profile/master default | Enable command-line agent |
| `MCP_ENABLE_DESKTOP` | profile/master default | Enable app/audio helpers |
| `MCP_AGENT_COMMAND` | unset | Codex/OpenCode executable path |
| `MCP_AGENT_KIND` | inferred | `codex` or `opencode`; required if executable name is ambiguous |
| `MCP_BROWSER_SCRIPT` | auto-detected | Browser helper path; protocol compatibility is checked before use |
| `MCP_CHILD_ENV_ALLOW` | unset | Extra comma-separated non-secret child environment names |
| `MCP_STATE_FILE` | XDG/user state path | Durable refresh-token fingerprints, DCR clients, and session ownership; `:memory:` disables persistence |
| `MCP_TRUST_PROXY` | `none` | `none` or `cloudflare`; forwarded IPs are trusted only from a loopback proxy |
| `MCP_PUBLIC_URL` | unset | Public HTTPS OAuth origin |
| `MCP_OAUTH_USERNAME` | `user` | Built-in OAuth login username |
| `MCP_OAUTH_PASSWORD` | unset | Built-in OAuth login password; minimum 12 characters when OAuth is enabled |
| `MCP_OAUTH_ACCESS_TOKEN_TTL` | `3600` | OAuth access-token lifetime in seconds |
| `MCP_OAUTH_REFRESH_TOKEN_TTL` | `2592000` | OAuth refresh-token lifetime in seconds |
| `MCP_OAUTH_CODE_TTL` | `300` | OAuth authorization-code lifetime in seconds |
| `MCP_OAUTH_MAX_FAILED_LOGINS` | `6` | Failed logins allowed per client bucket/window |
| `MCP_OAUTH_LOGIN_WINDOW_SECONDS` | `60` | Failed-login rolling window |
| `MCP_OAUTH_MAX_LOGIN_BUCKETS` | `1024` | Maximum in-memory failed-login client buckets |
| `MCP_OAUTH_MAX_CODES` | `256` | Maximum in-memory authorization codes |
| `MCP_OAUTH_MAX_CLIENTS` | `1024` | Maximum cached CIMD + legacy DCR client registrations |
| `MCP_OAUTH_DCR_CLIENT_TTL` | `2592000` | Legacy DCR client lifetime in seconds |
| `MCP_OAUTH_CLIENT_METADATA_TIMEOUT_SECONDS` | `10` | DNS/fetch timeout for CIMD metadata |
| `MCP_OAUTH_CLIENT_METADATA_MAX_BYTES` | `65536` | Maximum CIMD response size |
| `MCP_OAUTH_CLIENT_METADATA_CACHE_TTL` | `300` | Default/maximum CIMD cache lifetime when cache headers permit |
| `MCP_OAUTH_MAX_ACCESS_TOKENS` | `1024` | Maximum in-memory OAuth access tokens |
| `MCP_OAUTH_MAX_REFRESH_TOKENS` | `1024` | Maximum in-memory OAuth refresh tokens |
| `MCP_SHELL_TIMEOUT_SECONDS` | `30` | Shell timeout, bounded to 1–600 seconds |
| `MCP_AGENT_TIMEOUT_SECONDS` | `180` | Agent timeout, bounded to 1–1800 seconds |
| `MCP_BROWSER_TIMEOUT_SECONDS` | `30` | Browser helper timeout, bounded to 1–300 seconds |
| `MCP_STDOUT_LIMIT_BYTES` | `1048576` | Per-process captured stdout limit |
| `MCP_STDERR_LIMIT_BYTES` | `262144` | Per-process captured stderr limit |
| `MCP_SHELL_CONCURRENCY` | `2` | Maximum concurrent shell executions |
| `MCP_AGENT_CONCURRENCY` | `2` | Maximum concurrent agent executions |
| `MCP_BROWSER_CONCURRENCY` | `1` | Maximum concurrent browser helper operations |

MCP/HTTP request bodies are hard-limited to 1 MiB before RMCP parsing. Oversized requests return HTTP `413 Payload Too Large`.

## Health endpoints

- `/live` — process liveness only, HTTP 200 while the bridge is running.
- `/ready` — bridge + backend readiness, HTTP 503 when the backend is unavailable.
- `/health` — compatibility alias for `/ready`.

The root `/` response includes package version, build commit/dirty state, and browser-helper protocol so deployments can detect version skew.

## Docker

```bash
docker build \
  --build-arg BUILD_COMMIT="$(git rev-parse --short=12 HEAD)" \
  --build-arg BUILD_DIRTY="$(test -z "$(git status --porcelain)" && echo false || echo true)" \
  -t mcp-bridge .
docker run --rm \
  -p 127.0.0.1:3000:3000 \
  -e MCP_TOKEN="$(openssl rand -hex 32)" \
  -e BRIDGE_BACKEND_URL=http://host.docker.internal:4097 \
  -e BRIDGE_WORKDIR=/work \
  -v "$PWD:/work:ro" \
  -v mcp-bridge-state:/state \
  mcp-bridge
```

The image runs as an unprivileged `bridge` user with `MCP_PROFILE=server-secure`, `/work` as the default confined work directory, and `/state/state.json` as durable state. Mount `/state` persistently if OAuth refresh/session continuity must survive container recreation. Desktop/CDP tools are primarily intended for native personal-workstation use; containerized desktop control needs explicit host integration and should not be enabled casually.

## Verification

Local quality gates:

```bash
cargo fmt --all -- --check
cargo check --locked --all-targets --all-features
cargo test --locked --all-features
cargo clippy --locked --all-targets --all-features -- -D warnings
node --check scripts/browser.cjs
bash -n scripts/package-release.sh
scripts/package-release.sh
cargo audit
```

The repository CI runs the same Rust/Node checks plus the end-to-end MCP/OAuth integration tests included in `cargo test`, a release build, and a RustSec dependency audit.

## Security model

MCP Bridge is intentionally powerful. Enabling host tools gives an authenticated MCP client meaningful control over the host account.

For a personal workstation:

- use a strong token or OAuth password
- expose only through authenticated HTTPS
- keep backend/CDP ports private
- use a dedicated Chrome automation profile when practical
- set `BRIDGE_WORKDIR` no broader than necessary
- review which host tool groups are enabled
- never treat unrestricted shell as a sandbox

See [SECURITY.md](SECURITY.md) for the detailed threat model and reporting process.
