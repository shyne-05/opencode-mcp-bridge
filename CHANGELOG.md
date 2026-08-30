# Changelog

All notable changes to MCP Bridge are documented here.

## Unreleased

## 0.6.0 — 2026-08-30

### Added

- Added native Linux, macOS, and Windows support while keeping the public MCP surface fixed at exactly five tools.
- Added native macOS LaunchAgent helpers and Windows Scheduled Task, OAuth bootstrap, browser-launch, environment-runner, and release-packaging PowerShell helpers.
- Added CI verification on `ubuntu-latest`, `macos-latest`, and `windows-latest`, including Rust tests, browser-worker regression, OAuth browser regression, Clippy, and native packaging.
- Added runtime OS, architecture, and native-shell provenance to the root metadata endpoint.

### Changed

- Made host shell execution platform-aware: Bash on Linux, isolated zsh on macOS, and deterministic CMD on Windows by default, with `MCP_WINDOWS_SHELL=powershell` as an explicit PowerShell mode.
- Made configuration and helper paths portable instead of assuming a drive letter, username, or home-directory layout; Windows helpers use user-scoped system locations such as LocalApplicationData.
- Added native browser discovery/launch helpers using a dedicated CDP profile and loopback-only remote debugging, compatible with current Chrome remote-debugging requirements.
- Hardened process-tree termination, durable-state replacement, canonical path confinement, and oversized-request rejection for platform-specific Windows/macOS behavior.

### Performance

- Added a persistent Node/Playwright browser worker for `navigate`, `snapshot`, `click`, `fill`, and `evaluate`, eliminating repeated Node startup, Playwright module loading, and CDP reconnection from the browser hot path.
- Prewarm the persistent browser worker in the background when browser support is enabled so the first user action avoids the Node/Playwright cold start.
- Tuned the shared Reqwest client for longer connection reuse, a larger idle pool, TCP keepalive, and TCP_NODELAY.
- Added structured `mcp_bridge::latency` tracing for total MCP tool latency plus shell/browser queue and execution latency.
- Prevent detached background applications from holding completed `shell` calls open indefinitely through inherited stdout/stderr pipes; captured output is preserved during a short drain grace and open streams are marked truncated.

### Tests

- Added a persistent browser-worker protocol regression that verifies handshake compatibility and multiple requests on one long-lived worker process.
- Added a shell regression proving detached background children do not delay an otherwise completed MCP tool response.

## 0.5.2 — 2026-08-29

### Removed

- Removed the redundant native application-launch MCP wrapper. Trusted personal-desktop deployments use the existing `shell` capability for application and audio control, reducing the exposed tool surface to the capabilities that are actually needed.
- Removed the public async/session-management MCP wrappers (`bridge_prompt_async`, `bridge_session_messages`, `bridge_session_status`, and `bridge_list_sessions`). Backend session ownership remains an internal implementation detail of synchronous `bridge_prompt`.
- Removed `bridge_agent_prompt` and the obsolete command-line-agent enablement, command, kind, timeout, and concurrency configuration. Coding CLIs remain reachable through the explicitly enabled trusted shell when needed.

### Fixed

- Hardened OAuth login CSP generation so form submission allows only `'self'` and the exact origin of an already validated registered redirect URI.
- Expanded redirect validation coverage for HTTPS callbacks, explicit ports, localhost and IPv4/IPv6 loopback callbacks, dynamic loopback ports, and rejection of custom schemes, URL credentials, and fragments.
- Preserved unexpired OAuth access authorization across normal bridge restarts by persisting only access-token fingerprints and metadata; plaintext access and refresh token values are never written to durable state.
- Strengthened the real-browser OAuth regression to verify the login POST returns HTTP 302, the registered callback is reached with preserved state and an authorization code, and sensitive credential/token canaries are absent from captured output.

### Operations

- Modernized GitHub Actions runtimes and removed stale action inputs/warnings while keeping CI minimal and locked where appropriate.
- Added the real-browser OAuth regression to CI and retained format, check, test, clippy, browser-helper syntax, release packaging, and RustSec audit gates.
- Added a guarded native user-service deployment helper that safely fast-forwards a clean local `main`, snapshots the current package for rollback, builds the release, restarts the service, and verifies process identity, the single listener, `/live`, `/ready`, build provenance, `dirty=false`, and browser-helper protocol compatibility.
- When deployment is initiated through MCP Bridge itself, restart work is handed to a transient user-systemd unit so the bridge cannot terminate the process that still needs to finish deployment verification.

## 0.5.1 — 2026-08-29

### Fixed

- Restored the safe native application launcher for reliable Flatpak and desktop-launcher resolution while keeping the redundant audio wrappers removed.

## 0.5.0 — 2026-08-29

### Removed

- Removed the redundant native desktop and audio MCP wrappers. Trusted users can continue to launch applications and manage audio through the existing `shell` tool, while the MCP surface stays focused on backend, shell, browser, and coding-agent workflows.

## 0.4.0 — 2026-08-29

### Added

- Official `rmcp`-based MCP transport and protocol negotiation, including MCP `2026-07-28` while retaining legacy client compatibility.
- `personal-desktop` and `server-secure` runtime profiles.
- Independent shell, browser, command-line agent, and desktop capability switches.
- Native `desktop_open_app`, `audio_get_volume`, and `audio_set_volume` tools.
- Per-tool concurrency limits and configurable process timeout/output limits.
- Configurable backend-response and per-principal session-history limits.
- Parser-based validation for OAuth public origins and browser navigation URLs.
- OAuth state cleanup, bounded in-memory OAuth maps, failed-login throttling, refresh-token rotation, CIMD client discovery, and legacy DCR fallback.
- Expanded unit/integration coverage for filesystem confinement, bounded backend/process I/O, session ownership/history, profiles, OAuth helpers, authentication, and browser behavior.
- RustSec audit job in CI.
- Durable state for hashed OAuth refresh tokens, DCR clients, and per-principal backend session ownership.
- Explicit `MCP_TRUST_PROXY` handling with multi-dimensional OAuth throttling.
- Codex and OpenCode command-line agent adapters via `MCP_AGENT_KIND`.
- `/live` and dependency-aware `/ready` health endpoints.
- Build provenance and browser-helper protocol reporting from `/`.
- Release packaging script and user-systemd deployment example.

### Changed

- Refactored the former monolithic `main.rs` into focused configuration, authentication, OAuth, backend, process, browser, desktop, state, tools, and utility modules.
- Child commands now start from a sanitized environment rather than inheriting the bridge process environment.
- Personal-desktop mode explicitly preserves the desktop session variables needed for Wayland, DBus, PipeWire, and application launching.
- Backend file reads are canonicalized and restricted to `BRIDGE_WORKDIR`.
- Process output is bounded while streaming instead of being truncated only after the child exits.
- Backend HTTP bodies are bounded while streaming instead of being fully buffered.
- Unix child processes run in dedicated process groups so timeouts terminate the complete command tree.
- HTTP dependencies use Rustls and RMCP disables unused default macro features to reduce the dependency surface.
- Docker defaults to the `server-secure` profile and uses locked release builds.

### Fixed

- Reject unauthenticated mode on non-loopback listeners, preventing accidental remote unauthenticated host-tool exposure.
- Constrain `bridge_search` to `BRIDGE_WORKDIR` and filter path/symlink escapes.
- Enforce the 1 MiB request limit on the actual RMCP Streamable HTTP path.
- Preserve refresh-token usability, DCR registrations, and backend-session ownership across bridge restarts.
- Make `bridge_session_status` read the backend execution-status endpoint.
- Report shell/agent non-zero exits and timeouts as MCP tool errors.
- Make browser close of a missing target an MCP error and preserve structured evaluate results.
- Make the browser-helper protocol probe dependency-free so release/CI packaging can verify compatibility before Playwright is installed.
- Reject directories in `bridge_read_file` before contacting the backend.
- Split liveness from backend readiness so failed dependencies are observable.
- Replaced brittle hard-coded ChatGPT OAuth client/redirect matching with standards-based Client ID Metadata Document validation.
- Added SSRF-safe CIMD fetching and exact metadata-bound redirect URI checks, allowing the normal “paste MCP URL → authorize → return to client” flow.
- Advertised and implemented Dynamic Client Registration fallback plus `offline_access` for long-lived ChatGPT connectivity.
- Flatpak application resolution now rejects ambiguous partial matches instead of launching the first match.
- Fixed browser snapshots on current Playwright by replacing the removed accessibility snapshot API with ARIA snapshots and a text fallback.
- Fixed browser `navigate` so it navigates a selected/current tab rather than creating a new tab.
- Fixed global Playwright resolution under the sanitized process environment by injecting only the discovered `NODE_PATH` into the browser helper.
- Prevented the CDP browser helper from closing the user's Chrome instance when the helper exits.
- Prevented malformed authorization-code exchange attempts from prematurely consuming otherwise valid authorization codes.
- Prevented refresh-token replay by rotating and consuming refresh tokens.

### Security

- MCP/OAuth/tunnel secrets are no longer inherited by shell/browser/agent/desktop child processes.
- OAuth login responses receive CSP, frame, referrer, content-type, and no-store protections.
- Path traversal and symlink escape outside `BRIDGE_WORKDIR` are rejected.
- Server-secure deployments expose only core backend tools unless host capabilities are explicitly enabled.
