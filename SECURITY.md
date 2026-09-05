# Security Policy

## Reporting a vulnerability

Report security issues privately through GitHub Security Advisories for this repository. Do not disclose tokens, cookies, private keys, tunnel credentials, OAuth codes, or exploit details in a public issue.

## Threat model

MCP Bridge has two intentionally different roles:

- a local/backend MCP gateway
- an optional personal-computer automation gateway

When host tools are enabled, authenticated clients may execute host commands, interact with the desktop session, or access browser state. This is intentional capability, not a sandbox escape.

The primary security boundary is therefore **authentication + deployment trust**, followed by defense-in-depth controls that reduce accidental privilege/secret exposure.

## Security controls

### Authentication

- Static bearer tokens and named bearer tokens are supported.
- OAuth uses authorization code + PKCE S256, resource binding, issuer (`iss`) responses, refresh-token rotation, and `offline_access` support.
- Modern MCP clients are resolved through OAuth Client ID Metadata Documents (CIMD); redirect URIs must exactly match verified metadata.
- CIMD fetching is SSRF-hardened: HTTPS DNS hostnames only, private/reserved address rejection, DNS pinning for the request, proxy/redirect disabling, bounded response size, and short fetch timeouts.
- Dynamic Client Registration is available only as a backwards-compatible fallback and is rate-limited/bounded.
- Authorization codes are short-lived, process-local, and bounded. Unexpired access-token metadata, refresh-token metadata, and DCR registrations are durable across normal restarts so an active OAuth authorization is not discarded merely because the bridge process restarts.
- Access-token and refresh-token values are never persisted in plaintext. Durable token records use SHA-256 fingerprints as lookup keys and restrictive atomic state-file writes.
- Refresh tokens rotate; a consumed refresh token cannot be reused. Token exchanges persist their result before consuming the old credential, so a failed state-file write leaves the exchange retryable.
- OAuth login throttling combines source-IP, username-fingerprint, and global buckets. Forwarded IP headers are ignored unless `MCP_TRUST_PROXY=cloudflare` is explicitly enabled and the actual TCP peer is a loopback proxy.
- Token/password comparisons avoid ordinary early-exit string comparison.

### HTTP and deployment guards

- `MCP_ALLOW_UNAUTHENTICATED=true` is rejected at startup unless `MCP_HOST` is loopback.
- MCP/HTTP request bodies are capped before Streamable HTTP/RMCP parsing and oversized requests return HTTP 413.
- `/live` reports process liveness; `/ready` and `/health` report backend readiness and return HTTP 503 when unavailable.
- The root endpoint exposes version/build/helper-protocol provenance to make mixed-version deployments observable.
- The Linux guarded user-service deployment helper requires a clean `main`, safely fast-forwards it to `origin/main`, builds the release package, restarts the service, and verifies process identity, health, readiness, build commit, `dirty=false`, and browser-helper protocol.
- The Linux guarded deployment helper captures the running executable/helper outside the repository before packaging, and restores them if restart or verification fails. When deployment is initiated from the bridge process itself, it hands restart work to a transient user-systemd unit before restarting `mcp-bridge.service`, preventing the service from killing the process that still needs to complete deployment verification.

### Durable state

By default durable state is stored under the user's XDG state directory (or `~/.local/state/mcp-bridge/state.json`). Use `MCP_STATE_FILE` to choose another path or `:memory:` for ephemeral tests. The store contains OAuth access-token fingerprints/metadata, refresh-token fingerprints/metadata, DCR client registrations, and principal-to-backend-session ownership. It must be protected like authentication state and must never be committed.

### Child processes

- Child processes do not inherit the complete bridge environment.
- A small runtime/desktop environment allowlist is reconstructed explicitly.
- Secret-looking environment variable names cannot be added through `MCP_CHILD_ENV_ALLOW`.
- Captured stdout/stderr is bounded while streaming.
- Shell, browser, and backend work have bounded running and waiting capacity. Queued calls expire after five seconds; backend health checks remain independent.
- On Unix, spawned commands run in a separate process group; timeout and cancellation cleanup terminates that group.

These controls reduce accidental credential leakage and denial-of-service risk. They do **not** constrain what an explicitly enabled unrestricted shell command can do using the host account's normal permissions.

### Files and backend sessions

- Requested directories are canonicalized and must remain inside `BRIDGE_WORKDIR`.
- Backend file reads are canonicalized before they are sent to the backend.
- Symlink/path traversal outside `BRIDGE_WORKDIR` is rejected.
- Backend sessions created through the bridge are tracked per authenticated principal and ownership is persisted across bridge restarts.
- Bridge-owned session history is bounded per principal, search is forced into canonical `BRIDGE_WORKDIR` and filtered for path/symlink escapes, regular-file reads reject directories, and backend response bodies are capped while streaming.

### Browser

- Chrome CDP is expected on loopback (`127.0.0.1:9222`).
- Browser URLs are limited to `http://`, `https://`, and `about:blank`.
- The helper does not call `browser.close()` on a user-owned CDP browser. Helper protocol compatibility is checked before scripted actions, and nonexistent close targets fail explicitly.
- Browser HTTP bodies are capped at 1 MiB, and each action has one deadline covering startup and execution. Cancelled or invalid workers are discarded before another call can use them.
- Browser cookies/page data remain sensitive because browser control is intentionally powerful.

## Deployment guidance

### Personal workstation

`MCP_PROFILE=personal-desktop` is appropriate when the bridge is intended to open applications, control audio, use the desktop bus/session, and run local development commands.

For an installed `mcp-bridge.service`, prefer the guarded deployment helper:

```bash
bash scripts/deploy-user-service.sh
```

Recommended controls:

- keep the bridge listener private when possible
- use HTTPS + OAuth for remote clients
- keep Chrome CDP and the local backend on loopback/private interfaces
- use a separate Chrome automation profile when practical
- keep `BRIDGE_WORKDIR` as narrow as the workflow allows
- enable only the host tool groups you actually use
- protect the operating-system account itself with normal workstation security

Running as the normal desktop user is a deliberate choice for personal automation because Wayland, DBus, PipeWire, application launchers, and user files belong to that session.

### Server/VPS

Use:

```text
MCP_PROFILE=server-secure
```

Host/desktop tools default off. Avoid enabling unrestricted shell unless the authenticated MCP client and the complete network path are trusted.

### Containers

The provided image runs as a non-root user. Container boundaries can be useful for server deployments but do not automatically make host-integrated desktop automation safe. Avoid mounting Docker sockets, broad host filesystems, or credential directories into the bridge container.

## Secrets

Never commit:

- `MCP_TOKEN` / `MCP_TOKENS`
- `MCP_OAUTH_PASSWORD`
- Cloudflare tunnel credentials/tokens
- API keys
- browser cookies/profiles
- private keys

Prefer a service manager or secret manager over storing secrets in repository files.

Before committing, run `bash scripts/scan-secrets.sh --worktree`, stage the intended files, then run `bash scripts/scan-secrets.sh --staged`. The staged check reads the exact Git index; the worktree check also covers new, nonignored files. The default command scans committed history. Reports identify files without printing credential values.
