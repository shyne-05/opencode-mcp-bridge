# MCP Bridge

MCP Bridge is a Rust gateway that connects MCP clients such as ChatGPT to a local agent backend and, when explicitly enabled, to the owner's desktop environment.

Current package version: **0.5.2**.

The project is intentionally built around a small public MCP surface, strong authentication boundaries, restart-safe OAuth state, and a guarded self-deployment path for personal workstations.

## Highlights

- **Five-tool public surface** — three backend tools plus optional `shell` and `browser`; no public async/session-management wrappers.
- **Trusted desktop automation** — authenticated clients can use unrestricted host Bash and a local Chrome/Chromium CDP session when explicitly enabled.
- **Restart-safe OAuth** — PKCE, CIMD, DCR fallback, rotating refresh tokens, durable token fingerprints/metadata, and standards-based invalid-token recovery.
- **Bounded execution** — child environments are sanitized, output is capped, concurrency is limited, and timed-out process trees are terminated.
- **Deployment provenance** — `/` reports version, build commit, dirty state, and browser-helper protocol; `/live` and `/ready` expose liveness/readiness.
- **Rollback-safe self-update** — `scripts/deploy-user-service.sh` synchronizes `main`, packages the release, restarts the user service, verifies the live build, and rolls back on failure.

The MCP protocol layer uses the official Rust MCP SDK (`rmcp`) and negotiates the protocol revisions supported by the pinned SDK, including current and legacy MCP clients used by this project.

## MCP tools

The intended public surface is deliberately small:

| Tool | Purpose | Availability |
| --- | --- | --- |
| `bridge_prompt` | Send a prompt to the configured local agent backend and wait for the result | Backend configured |
| `bridge_read_file` | Read an existing file inside `BRIDGE_WORKDIR` through the backend | Backend configured |
| `bridge_search` | Search the configured backend workspace | Backend configured |
| `shell` | Run unrestricted host Bash commands with a sanitized environment, bounded output, timeout, process-tree termination, and concurrency limiting | `MCP_ENABLE_SHELL=true` |
| `browser` | Control a local Chrome/Chromium CDP session | `MCP_ENABLE_BROWSER=true` |

`MCP_ENABLE_HOST_TOOLS=true` remains supported for 0.3 compatibility. In the `personal-desktop` profile it enables the optional shell and browser groups unless an individual switch overrides the default.

The project intentionally does **not** expose public async/session-management or agent-wrapper tools. Backend session ownership is internal to the bridge.

## Requirements

Install only what your deployment uses:

- Rust **1.88** or newer
- Bash for `shell`
- A compatible backend service for `bridge_prompt`, `bridge_read_file`, and `bridge_search`
- Google Chrome/Chromium, Node.js, and Playwright for `browser`
- `systemd --user`, `curl`, `ss`, Python 3, and `flock` for the guarded native deployment helper
- `cloudflared` only when an external MCP client must reach the bridge through a tunnel

The backend adapter expects these local HTTP endpoints:

- `/global/health`
- `/session`
- `/session/{id}/message`
- `/find`
- `/file/content`

## Build

```bash
git clone https://github.com/shyne-05/opencode-mcp-bridge.git mcp-bridge
cd mcp-bridge
cargo build --release
```

The native release binary is:

```text
target/release/mcp-bridge
```

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

## Safe self-update / deployment

For a native installation managed by `mcp-bridge.service`, the **official update path** is the checked-in guarded deployment helper:

```bash
bash scripts/deploy-user-service.sh
```

Do not manually rebuild a new binary underneath the running service when this workflow is available.

### What the deployment helper does

The helper performs the complete deployment transaction:

1. Requires a clean working tree on `main`.
2. Fetches `origin/main` and allows only a fast-forward synchronization.
3. Refuses to deploy if local `HEAD` does not equal `origin/main` after synchronization.
4. Captures the currently running bridge executable and browser helper for rollback.
5. Builds/packages the release through `scripts/package-release.sh`.
6. Restarts `mcp-bridge.service`.
7. Confirms the previous PID exited and the service is active with a new live process.
8. Confirms systemd is running the freshly built executable rather than a stale/deleted image.
9. Confirms exactly one TCP listener belongs to the bridge process.
10. Verifies `/live` and `/ready` return HTTP 200.
11. Verifies root build provenance: package version, expected Git commit, `dirty=false`, and browser helper protocol `mcp-browser-helper/2`.
12. If restart or verification fails, restores the previous packaged executable/helper and attempts to bring the previous service back to liveness.

The deployment state directory defaults to:

```text
${XDG_STATE_HOME:-$HOME/.local/state}/mcp-bridge
```

It is created with restrictive permissions and contains deployment lock/rollback material rather than application secrets.

### Updating through MCP itself

The existing `shell` tool is intentionally the maintenance entry point; there is no separate public reset/restart/update MCP tool.

An authenticated client can run:

```bash
bash scripts/deploy-user-service.sh
```

If that command is executing **inside the bridge's own systemd service cgroup**, the script does not restart the service from the same process that must report the tool result. Instead, it hands the restart/verification phase to a detached transient `systemd --user` unit and returns first. This prevents a self-update from killing its own deploy worker halfway through the transaction.

After the MCP connection is re-established, verify the service if desired:

```bash
systemctl --user is-active mcp-bridge.service
curl --fail http://127.0.0.1:3000/live
curl --fail http://127.0.0.1:3000/ready
curl --fail http://127.0.0.1:3000/
```

If your service listens on a different address, use that listener instead of `127.0.0.1:3000`.

### systemd service layout

The repository includes `deploy/mcp-bridge.service` as the reference native user service. The checked-in unit currently assumes:

```text
Repository:   ~/opencode-mcp-bridge-rust
Environment:  ~/.config/mcp-bridge/env
Binary:       ~/opencode-mcp-bridge-rust/target/release/mcp-bridge
```

If you cloned the repository somewhere else, adjust `WorkingDirectory` and `ExecStart` in your installed user unit before using the deployment helper. The helper itself discovers its repository root from its own path, but it deliberately verifies that the running service points at that repository's packaged release binary.

Typical user-service management commands are:

```bash
systemctl --user daemon-reload
systemctl --user enable --now mcp-bridge.service
systemctl --user status mcp-bridge.service
```

### MCP tool-schema changes

Normal implementation updates, OAuth fixes, rebuilds, and service restarts do **not** require a new MCP tool definition.

Keep `shell` stable and use it for maintenance. If a release actually adds/removes/renames tools or changes their input schemas, MCP clients that cache tool definitions may need their app/tool snapshot refreshed or reconnected after the new server is live.

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
```

### `server-secure`

Designed for server/VPS use. Optional host tools default off even if the legacy desktop-oriented master switch is present; enable individual capabilities deliberately.

```bash
export MCP_PROFILE=server-secure
```

## Child-process security model

The bridge itself may contain authentication secrets such as `MCP_OAUTH_PASSWORD`. Shell and browser helper child processes **do not inherit the full bridge environment**.

Instead, the bridge clears each child environment and restores only an allowlist of ordinary runtime variables. Personal-desktop mode includes the desktop/session variables needed for Wayland, DBus, PipeWire, and host-shell/browser workflows.

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
| `MCP_BROWSER_TIMEOUT_SECONDS` | `30` | Browser/helper timeout, 1–300 seconds |
| `MCP_STDOUT_LIMIT_BYTES` | `1048576` | Captured stdout cap |
| `MCP_STDERR_LIMIT_BYTES` | `262144` | Captured stderr cap |
| `MCP_SHELL_CONCURRENCY` | `2` | Concurrent shell calls |
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
bash scripts/bootstrap-oauth.sh https://your-domain.example.com
```

The bootstrap helper uses `admin` as the normal username, generates a unique high-entropy password locally, and stores it only in `~/.config/mcp-bridge/env` with mode `600`. No shared default password is committed to this repository. To view the local credential when the OAuth login page opens:

```bash
bash scripts/bootstrap-oauth.sh --show
```

Use `--rotate` to replace the password deliberately. See [`docs/OAUTH_SETUP.md`](docs/OAUTH_SETUP.md) for the complete onboarding and rotation workflow. You can still set `MCP_OAUTH_USERNAME` and `MCP_OAUTH_PASSWORD` manually; explicit values override the bootstrap convention, and passwords must be at least 12 characters.

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
- restart-safe access authorization through durable SHA-256 token fingerprints/metadata
- standards-based `invalid_token` Bearer challenges so capable clients can refresh and retry
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

OAuth authorization codes remain short-lived and in memory. Unexpired access-token metadata, rotating refresh-token metadata, Dynamic Client Registration records, and bridge-owned backend-session ownership are stored in the durable state file so normal bridge restarts preserve active OAuth authorization and do not orphan resumable backend sessions.

Access and refresh token values are never written to durable state in plaintext. Durable token records are keyed by SHA-256 fingerprints and written atomically with restrictive permissions on Unix.

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

## Health and build provenance

- `/live` — process liveness only; HTTP 200 while the bridge is running.
- `/ready` — bridge + backend readiness; HTTP 503 when the backend is unavailable.
- `/health` — compatibility alias for `/ready`.

The root `/` response includes package version, build commit/dirty state, and browser-helper protocol so deployment tooling can detect version skew.

A healthy production/native deployment should report the expected Git commit, `dirty=false`, and browser helper protocol `mcp-browser-helper/2`.

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
| `MCP_BROWSER_SCRIPT` | auto-detected | Browser helper path; protocol compatibility is checked before use |
| `MCP_CHILD_ENV_ALLOW` | unset | Extra comma-separated non-secret child environment names |
| `MCP_STATE_FILE` | XDG/user state path | Durable OAuth token fingerprints/metadata, DCR clients, and session ownership; `:memory:` disables persistence |
| `MCP_TRUST_PROXY` | `none` | `none` or `cloudflare`; forwarded IPs are trusted only from a loopback proxy |
| `MCP_PUBLIC_URL` | unset | Public HTTPS OAuth origin |
| `MCP_OAUTH_USERNAME` | `admin` | Built-in OAuth login username; bootstrap helper writes this explicitly |
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
| `MCP_OAUTH_MAX_ACCESS_TOKENS` | `1024` | Maximum active OAuth access-token records |
| `MCP_OAUTH_MAX_REFRESH_TOKENS` | `1024` | Maximum active OAuth refresh-token records |
| `MCP_SHELL_TIMEOUT_SECONDS` | `30` | Shell timeout, bounded to 1–600 seconds |
| `MCP_BROWSER_TIMEOUT_SECONDS` | `30` | Browser helper timeout, bounded to 1–300 seconds |
| `MCP_STDOUT_LIMIT_BYTES` | `1048576` | Per-process captured stdout limit |
| `MCP_STDERR_LIMIT_BYTES` | `262144` | Per-process captured stderr limit |
| `MCP_SHELL_CONCURRENCY` | `2` | Maximum concurrent shell executions |
| `MCP_BROWSER_CONCURRENCY` | `1` | Maximum concurrent browser helper operations |

MCP/HTTP request bodies are hard-limited to 1 MiB before RMCP parsing. Oversized requests return HTTP `413 Payload Too Large`.

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

The image runs as an unprivileged `bridge` user with `MCP_PROFILE=server-secure`, `/work` as the default confined work directory, and `/state/state.json` as durable state. Mount `/state` persistently if OAuth/session continuity must survive container recreation.

Desktop/CDP tools are primarily intended for native personal-workstation use; containerized desktop control needs explicit host integration and should not be enabled casually.

## Verification

Local quality gates:

```bash
cargo fmt --all -- --check
cargo check --locked --all-targets --all-features
cargo test --locked --all-features
NODE_PATH="$(npm root -g)" node tests/oauth_browser.cjs
cargo clippy --locked --all-targets --all-features -- -D warnings
node --check scripts/browser.cjs
bash -n scripts/package-release.sh
bash -n scripts/deploy-user-service.sh
scripts/package-release.sh
cargo audit
```

The repository CI runs the Rust/Node checks, end-to-end MCP/OAuth integration tests included in `cargo test`, browser OAuth regression, release packaging checks, shell-script syntax checks, Clippy with warnings denied, and a RustSec dependency audit.

## Security model

MCP Bridge is intentionally powerful. Enabling host tools gives an authenticated MCP client meaningful control over the host account.

For a personal workstation:

- use a strong bearer token or OAuth password
- expose the bridge only through authenticated HTTPS when remote access is required
- keep backend and Chrome CDP ports private
- use a dedicated Chrome automation profile when practical
- set `BRIDGE_WORKDIR` no broader than necessary
- enable only the host tool groups you actually need
- never treat unrestricted shell as a sandbox
- keep the durable state directory private to the bridge user

See [SECURITY.md](SECURITY.md) for the detailed threat model and reporting process.
