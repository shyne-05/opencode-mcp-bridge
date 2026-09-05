# OAuth setup

MCP Bridge intentionally does **not** ship a shared default password. A password committed to a public repository would give every installation the same credential and would stop being a secret immediately.

For an easy first-run experience, the repository includes a local bootstrap helper. It uses `admin` as the normal username and generates a unique high-entropy password on the machine where the bridge runs.

## First setup

Run from the repository root.

Linux/macOS:

```bash
bash scripts/bootstrap-oauth.sh https://your-domain.example.com
```

Windows PowerShell:

```powershell
.\scripts\bootstrap-oauth.ps1 https://your-domain.example.com
```

The default environment file is user-specific:

| Platform | Configuration file |
| --- | --- |
| Linux/macOS | `~/.config/mcp-bridge/env` |
| Windows | the current user's `LocalApplicationData\mcp-bridge\env` |

Set `MCP_BRIDGE_ENV_FILE` before running the helper to select another file. Linux/macOS restrict the directory to its owner and write the file with mode `600`; on Windows, keep the selected directory protected by the user's file permissions.

The resulting OAuth values are equivalent to:

```text
MCP_PUBLIC_URL=https://your-domain.example.com
MCP_OAUTH_USERNAME=admin
MCP_OAUTH_PASSWORD=<unique locally generated secret>
```

No real password is committed to GitHub and the bootstrap helper does not print it into normal command output.

## Set your own password manually

If you prefer to choose the OAuth password yourself, use the placeholder shown in `.env.example` and replace it with your own password before starting the bridge:

```text
MCP_PUBLIC_URL=https://your-domain.example.com
MCP_OAUTH_USERNAME=admin
MCP_OAUTH_PASSWORD=YOUR_PASSWORD_HERE
```

`YOUR_PASSWORD_HERE` is documentation only. Replace it with a unique password for that installation. Do not commit the real value back to GitHub. OAuth passwords must be at least 12 characters.

## View the local login

When you need to enter the OAuth login on this machine:

```bash
# Linux/macOS
bash scripts/bootstrap-oauth.sh --show
```

```powershell
# Windows
.\scripts\bootstrap-oauth.ps1 -Show
```

This intentionally reveals the username/password only in the local terminal. Treat the output as a credential and do not paste it into issues, logs, screenshots, or commits.

## Rotate the password

```bash
# Linux/macOS
bash scripts/bootstrap-oauth.sh --rotate https://your-domain.example.com
```

```powershell
# Windows
.\scripts\bootstrap-oauth.ps1 -Rotate https://your-domain.example.com
```

Restart the installed bridge service or task when ready to load the new login password. Changing the login password does not revoke already issued OAuth tokens.

## Existing password behavior

Running the normal bootstrap command again preserves an existing nonblank `MCP_OAUTH_PASSWORD`. An empty or whitespace-only entry is replaced with a generated password. Use `--rotate` when you explicitly want a new password.

## Custom username

`admin` is the bootstrap default. To choose another username for one setup:

```bash
MCP_OAUTH_USERNAME=my-user \
  bash scripts/bootstrap-oauth.sh https://your-domain.example.com
```

Windows PowerShell:

```powershell
$env:MCP_OAUTH_USERNAME = 'my-user'
.\scripts\bootstrap-oauth.ps1 https://your-domain.example.com
```

## Loopback development

For local OAuth testing only, loopback HTTP is accepted by the helper:

```bash
bash scripts/bootstrap-oauth.sh http://127.0.0.1:3000
```

On Windows, pass the same origin to `scripts/bootstrap-oauth.ps1`. The helpers accept `localhost`, `127.0.0.1`, and `::1` for HTTP testing and add `MCP_OAUTH_ALLOW_INSECURE_HTTP=true` only for those loopback origins. Public deployments should use HTTPS.

## Security rules

- Never add a real `MCP_OAUTH_PASSWORD`, `MCP_TOKEN`, API key, cookie, tunnel token, private key, or credential file to Git.
- Keep the selected environment file private to the bridge account.
- Use a separate credential per installation.
- Rotate credentials if they are accidentally shared.
- Keep the MCP bridge behind authenticated HTTPS when it is reachable remotely.
- Keep the backend and Chrome CDP listeners private.
