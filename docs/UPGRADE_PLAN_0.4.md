# MCP Bridge 0.4 Upgrade Plan (superseded)

This historical plan is retained for context. The current 0.5.2 release is the source of truth for the public API and configuration. The active MCP surface is `bridge_prompt`, `bridge_read_file`, `bridge_search`, and the opt-in `shell` and `browser` tools; the removed session wrappers, agent wrapper, and `MCP_AGENT_*` settings must not be reintroduced.

## Objective

Turn MCP Bridge into a production-quality personal desktop automation gateway while preserving the capabilities that make it useful on the owner's workstation: unrestricted host shell access when explicitly enabled, desktop workflows through the shell, browser automation, backend-agent sessions, and local file/project workflows.

The upgrade must improve security and maintainability **without weakening the personal-desktop use case**.

## Compatibility guarantees

- Keep the public project and binary name `mcp-bridge`.
- Preserve existing backend tools and their public names.
- Preserve `shell` and `browser` as opt-in host tools.
- Preserve bearer token, named bearer token, OAuth, and URL-path token compatibility.
- Preserve `BRIDGE_WORKDIR` confinement semantics for working directories.
- Keep the running 0.3.0 process untouched until the 0.4.0 source passes all verification gates.
- Do not commit or print credentials.

## Phase 1 — Architecture and protocol

1. Replace the hand-maintained MCP JSON-RPC implementation with the official Rust MCP SDK (`rmcp`).
2. Support current MCP protocol negotiation while remaining compatible with older clients supported by the SDK.
3. Split the 1,600+ line monolith into focused modules:
   - configuration
   - authentication / OAuth
   - shared application state
   - backend client
   - process execution
   - browser bridge
   - MCP tool definitions / dispatch
   - HTTP server/bootstrap
4. Keep HTTP authentication outside MCP tool logic and inject the authenticated principal through request extensions.

## Phase 2 — Host execution hardening

1. Keep shell access unrestricted when enabled; do not introduce a command allowlist.
2. Stop child commands from inheriting MCP/OAuth/tunnel secrets.
3. Build an explicit safe environment for child processes, including desktop-session variables required for Fedora/Wayland/DBus/PipeWire workflows.
4. Allow explicitly configured extra environment variable names for advanced workflows.
5. Put spawned commands in their own Unix process group and terminate the complete group on timeout.
6. Bound captured stdout/stderr while the process is running instead of truncating only after completion.
7. Add per-tool concurrency limits to prevent accidental process storms.
8. Make execution timeout and output limits configurable with safe bounds.

## Phase 3 — Capability profiles

1. Add a `personal-desktop` profile optimized for the owner's workstation.
2. Add a `server-secure` profile with host tools disabled by default.
3. Split the host-tool master switch into independent shell/browser switches while retaining the legacy master switch for compatibility.
4. Keep desktop variables available in `personal-desktop` mode without exposing bridge credentials.

## Phase 4 — Backend and file safety

1. Centralize backend HTTP request/error handling.
2. Validate backend response status before consuming bodies.
3. Keep backend-owned session isolation per authenticated principal.
4. Resolve backend file paths against `BRIDGE_WORKDIR` and reject traversal/symlink escapes before calling the backend.
5. Normalize limits and error formatting across tools.

## Phase 5 — OAuth hardening

1. Keep PKCE S256, resource binding, redirect validation, high-entropy tokens, and constant-time comparisons.
2. Add bounded in-memory OAuth state and periodic expired-token cleanup.
3. Rotate refresh tokens rather than reusing the same refresh token indefinitely.
4. Add login attempt throttling/backoff.
5. Add security headers to OAuth HTML responses.
6. Validate OAuth configuration at startup and reject obviously unsafe values.
7. Implement standards-based Client ID Metadata Documents (CIMD), with SSRF-resistant remote metadata fetching and exact redirect binding.
8. Keep Dynamic Client Registration (DCR) as a rate-limited backwards-compatible fallback.
9. Advertise `offline_access` and verify the full ChatGPT-style authorization/refresh flow.

## Phase 6 — Desktop ergonomics

1. Keep generic `shell` for advanced workflows, including application and audio control.
2. Keep browser automation as a separate stateful CDP capability.
3. Avoid duplicating shell functionality with separate native desktop/audio MCP wrappers.

## Phase 7 — Tests and verification

Add tests for at least:

- constant-time authentication checks
- bearer/path token authentication
- OAuth redirect/client validation
- PKCE verification
- expired OAuth state cleanup
- refresh-token rotation
- login throttling
- `BRIDGE_WORKDIR` traversal rejection
- symlink escape rejection
- secret environment stripping
- allowed desktop environment inheritance
- bounded process output
- process timeout behavior
- host-tool profile/feature selection
- browser URL validation
- backend session ownership
- MCP tool discovery
- authenticated MCP calls
- unauthenticated MCP rejection

Verification gates:

1. `cargo fmt --all -- --check`
2. `cargo check --locked --all-targets --all-features`
3. `cargo test --locked --all-features`
4. `cargo clippy --locked --all-targets --all-features -- -D warnings`
5. `node --check scripts/browser.cjs`
6. release build
7. smoke test a temporary 0.4.0 instance on a different port
8. verify OAuth metadata and MCP tool discovery
9. verify Spotify/application launch and audio control compatibility without exposing secrets
10. final dead-code/duplication/repository hygiene audit

## Phase 8 — Cleanup and documentation

1. Remove superseded protocol/auth/utility code.
2. Remove duplicate helpers and unused dependencies.
3. Keep `main.rs` bootstrap-only.
4. Update README, SECURITY, Dockerfile, CI, and configuration reference.
5. Ensure the working tree contains only intentional source/documentation changes and no generated secrets or runtime artifacts.

## Definition of done

The upgrade is complete only when the repository is modular, current-protocol capable, passes all automated checks, survives integration smoke tests, retains personal desktop automation behavior, and has no known duplicated/dead implementation left from 0.3.0.

## Implementation status — completed 2026-08-29

All planned phases were implemented in the 0.4.0 source tree and the active release has since been cleaned up and hardened as 0.5.2.

Verified outcomes:

- official RMCP protocol layer with current and legacy negotiation
- 5 tools in the trusted `personal-desktop` profile when host tools are enabled
- 3 core tools in `server-secure` by default
- sanitized child environments retain required desktop session variables without exposing `MCP_*` credentials
- bounded process output, process-group timeout termination, and tool concurrency limits
- canonical backend read/search confinement with traversal and symlink escape rejection, plus persistent session ownership and real execution status
- OAuth authorization-code + PKCE flow, CIMD client discovery, DCR fallback, access-token authentication, offline access, durable hashed refresh-token rotation, expired-state cleanup, bounded token/client state, and trusted-peer throttling
- static bearer-token and URL-path-token compatibility
- trusted shell workflows retain Spotify/application launch and PipeWire audio control
- current Playwright browser snapshot support
- formatter, locked check, unit tests, clippy `-D warnings`, Node syntax validation, locked release build, and RustSec audit gates

The original 0.3.0 migration was completed only after the exact release artifact passed all gates; the current deployment helper applies the same discipline to 0.5.2 and keeps a rollback package outside the repository.

## Completion status

For current deployment and verification instructions, use the native user-service section in `README.md` and the controls in `SECURITY.md`.
