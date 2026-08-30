# MCP Bridge

MCP Bridge is a cross-platform Rust gateway that connects MCP clients such as ChatGPT to a local agent backend and, when explicitly enabled, to the owner's desktop shell and Chrome/Chromium session.

Current package version: **0.6.0**.

The project keeps a deliberately small MCP contract, strong OAuth/authentication boundaries, bounded host-process execution, a persistent browser worker, and native helpers for Linux, macOS, and Windows.

## Supported platforms

| Platform | Native shell used by `shell` | Native service/install path |
| --- | --- | --- |
| Linux | Bash | systemd user service / guarded deploy helper |
| macOS | zsh | LaunchAgent |
| Windows | CMD by default | Scheduled Task |

On Windows the default is deterministic: `cmd.exe /d /s /c`. To make Windows PowerShell the default shell for MCP `shell` calls, set:

```text
MCP_WINDOWS_SHELL=powershell
```

PowerShell can also be invoked explicitly from a CMD-backed `shell` call when needed. The bridge does not select a shell based on which optional shell happens to be installed.

## Public MCP tools

The public surface is intentionally limited to five tools:

| Tool | Purpose |
| --- | --- |
| `bridge_prompt` | Send a prompt to the configured local agent backend |
| `bridge_read_file` | Read a file inside `BRIDGE_WORKDIR` through the backend |
| `bridge_search` | Search the configured backend workspace |
| `shell` | Execute a host command with bounded output, timeout, sanitized environment, and concurrency limits |
| `browser` | Control a local Chrome/Chromium CDP session through the persistent browser worker |

`MCP_ENABLE_SHELL=true` and `MCP_ENABLE_BROWSER=true` enable the optional host tools. `MCP_ENABLE_HOST_TOOLS=true` remains available for legacy personal-desktop configuration.

The bridge intentionally does not expose public async/session-management or agent-wrapper tools. Backend session ownership stays internal.

## Requirements

Install only what your deployment uses:

- Rust **1.88** or newer
- a compatible backend service for `bridge_prompt`, `bridge_read_file`, and `bridge_search`
- Node.js + Playwright + Chrome/Chromium for `browser`
- Linux guarded deployment: `systemd --user`, `curl`, `ss`, Python 3, and `flock`
- macOS service install: `launchctl`
- Windows service install: Windows PowerShell 5.1+ and Task Scheduler

The backend adapter expects:

- `/global/health`
- `/session`
- `/session/{id}/message`
- `/find`
- `/file/content`

## Clone and build

Linux/macOS:

```bash
git clone https://github.com/shyne-05/opencode-mcp-bridge.git mcp-bridge
cd mcp-bridge
cargo build --release --locked
```

Windows PowerShell:

```powershell
git clone https://github.com/shyne-05/opencode-mcp-bridge.git mcp-bridge
Set-Location mcp-bridge
cargo build --release --locked
```

Release binaries are written below `target/release/` (`mcp-bridge` on Unix, `mcp-bridge.exe` on Windows).

To package the bridge together with the browser helper:

```bash
# Linux/macOS
bash scripts/package-release.sh
```

```powershell
# Windows
.\scripts\package-release.ps1
```

## Portable configuration

Start from [`.env.example`](.env.example). Configuration must describe the machine where the bridge is actually running; do not copy another user's drive letter, username, or home-directory path.

A portable starting point is:

```text
MCP_PROFILE=personal-desktop
MCP_HOST=127.0.0.1
MCP_PORT=3000
BRIDGE_WORKDIR=.
BRIDGE_BACKEND_URL=http://127.0.0.1:4097
MCP_ENABLE_SHELL=true
MCP_ENABLE_BROWSER=true
```

`BRIDGE_WORKDIR` accepts either a relative or absolute existing directory and is canonicalized at startup. Leaving it relative avoids assumptions such as `C:\Users\name`, `D:\project`, or `/home/name`.

Native helpers derive their paths from the current user and from the repository location at runtime. Useful overrides include:

```text
MCP_BRIDGE_ENV_FILE=<custom environment-file path>
MCP_BROWSER_PROFILE_DIR=<custom browser-profile path>
BRIDGE_WORKDIR=<workspace path>
```

The default native environment-file location is user-scoped:

- Linux/macOS helpers: the current user's config location (normally `~/.config/mcp-bridge/env`, with helper/platform-specific handling)
- Windows: the current user's `LocalApplicationData\mcp-bridge\env`

## Personal desktop start

For local testing, configure authentication and run the release binary in the environment of your desktop session.

Linux/macOS example:

```bash
export MCP_PROFILE=personal-desktop
export BRIDGE_WORKDIR="."
export BRIDGE_BACKEND_URL="http://127.0.0.1:4097"
export MCP_TOKEN="$(openssl rand -hex 32)"
export MCP_ENABLE_SHELL=true
export MCP_ENABLE_BROWSER=true
./target/release/mcp-bridge
```

On Windows, set the equivalent environment variables in PowerShell or use the checked-in environment/service helpers. Do not commit real tokens or OAuth passwords.

The default listener is `127.0.0.1:3000`.

## Native service helpers

### Linux

The repository includes the reference user service at `deploy/mcp-bridge.service` and a guarded self-update/deployment path:

```bash
bash scripts/deploy-user-service.sh
```

The guarded deploy requires a clean `main`, fast-forwards from `origin/main`, packages the release, restarts the service, verifies process identity/listener/health/build provenance, and rolls back the packaged binary/helper if verification fails.

If the checked-in service paths do not match where you cloned the repository, adjust the installed user unit before using the guarded deploy helper.

### macOS

Install/update the LaunchAgent from the repository root:

```bash
bash scripts/install-user-service-macos.sh
```

The helper derives the repository path at runtime, packages the release, writes a user LaunchAgent under `~/Library/LaunchAgents`, and starts it with `launchctl`.

### Windows

Install/update the user Scheduled Task from PowerShell:

```powershell
.\scripts\install-user-service-windows.ps1
```

The helper derives the repository path at runtime, packages the release, creates/updates the current user's scheduled task, and starts it. It does not require a hard-coded drive letter or username.

## Browser automation

Install Playwright and start a dedicated CDP-enabled browser profile. The checked-in helpers perform platform-specific browser discovery and avoid requiring a copied executable path:

```bash
# Linux/macOS
bash scripts/start-browser.sh
```

```powershell
# Windows
.\scripts\start-browser.ps1
```

The `browser` tool supports:

- `tabs`
- `new`
- `navigate`
- `close`
- `snapshot`
- `click`
- `fill`
- `evaluate`

Scripted actions use a persistent Node/Playwright worker connected to Chrome CDP instead of spawning a new Node process for every action. The helper protocol is `mcp-browser-helper/2`.

Chrome CDP is expected on loopback and must not be exposed publicly.

## Authentication

### Static bearer token

Set a unique local secret in `MCP_TOKEN` (or named values in `MCP_TOKENS`) and connect to:

```text
http://127.0.0.1:3000/mcp
```

with:

```text
Authorization: Bearer <token>
```

URL-path token compatibility (`/mcp/<token>`) remains available for clients that cannot send an authorization header, but bearer headers are preferred.

`MCP_ALLOW_UNAUTHENTICATED=true` is accepted only on a loopback listener and is intended only for local development.

## ChatGPT / OAuth setup

Linux/macOS:

```bash
bash scripts/bootstrap-oauth.sh https://your-domain.example.com
```

Windows PowerShell:

```powershell
.\scripts\bootstrap-oauth.ps1 https://your-domain.example.com
```

The bootstrap helper generates a unique local password and writes the OAuth configuration to the current user's MCP Bridge environment file. Use `MCP_BRIDGE_ENV_FILE` when a different location is required.

The built-in OAuth implementation includes authorization code + PKCE S256, Client ID Metadata Documents (CIMD), bounded/rate-limited DCR fallback, refresh-token rotation, durable one-way token fingerprints, restart-safe active authorization, SSRF-hardened metadata fetching, and standards-based Bearer challenges.

See [docs/OAUTH_SETUP.md](docs/OAUTH_SETUP.md) for setup, credential viewing, rotation, and loopback development details.

## Security and resource limits

Important defaults:

| Variable / control | Default |
| --- | ---: |
| MCP/HTTP request body limit | 1 MiB |
| `MCP_SHELL_TIMEOUT_SECONDS` | `30` seconds |
| `MCP_BROWSER_TIMEOUT_SECONDS` | `30` seconds |
| `MCP_STDOUT_LIMIT_BYTES` | `1048576` |
| `MCP_STDERR_LIMIT_BYTES` | `262144` |
| `MCP_SHELL_CONCURRENCY` | `2` |
| `MCP_BROWSER_CONCURRENCY` | `1` |

Requests with a declared `Content-Length` above the MCP/HTTP limit are rejected before their bodies are read; the body-limit layer remains in place for streamed/chunked requests.

Child processes do not inherit the bridge's full environment. Only an allowlist of ordinary runtime/desktop variables is reconstructed, and secret-looking variables cannot be added through `MCP_CHILD_ENV_ALLOW`.

The unrestricted `shell` tool still executes with the operating-system permissions of the bridge user. Enable host tools only for authenticated clients you trust.

See [SECURITY.md](SECURITY.md) for the complete threat model.

## Health and provenance

- `/live` — process liveness
- `/ready` — bridge + backend readiness
- `/health` — readiness-compatible health endpoint
- `/` — package version, build commit, dirty state, OS/architecture, native shell, and browser-helper protocol

These endpoints make mixed-version or stale deployments observable.

## CI

CI validates the project on **Ubuntu, macOS, and Windows**. The matrix covers formatting, compile/check, Rust tests, browser-worker regression, OAuth browser regression, Clippy, Node syntax, and native release packaging. Additional jobs cover Rust security audit, Linux deployment/security helpers, Windows PowerShell 5.1/7 parsing, CMD availability, and Windows OAuth bootstrap behavior.

## MCP schema stability

Normal implementation updates, OAuth fixes, platform portability changes, rebuilds, and service restarts do not require a new MCP tool definition.

If a future release adds/removes/renames public tools or changes their input schemas, MCP clients that cache tool definitions may need a tool refresh or reconnect after the new server is live.

## More documentation

- [OAuth setup](docs/OAUTH_SETUP.md)
- [Security policy](SECURITY.md)
- [Changelog](CHANGELOG.md)
