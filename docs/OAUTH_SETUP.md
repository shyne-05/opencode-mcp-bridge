# OAuth setup

MCP Bridge intentionally does **not** ship a shared default password. A password committed to a public repository would give every installation the same credential and would stop being a secret immediately.

For an easy first-run experience, the repository includes a local bootstrap helper. It uses `admin` as the normal username and generates a unique high-entropy password on the machine where the bridge runs.

## First setup

Run from the repository root:

```bash
bash scripts/bootstrap-oauth.sh https://your-domain.example.com
```

The helper updates the native user-service environment file at:

```text
~/.config/mcp-bridge/env
```

The directory is restricted to the owner and the file is written with mode `600`.

The resulting OAuth values are equivalent to:

```text
MCP_PUBLIC_URL=https://your-domain.example.com
MCP_OAUTH_USERNAME=admin
MCP_OAUTH_PASSWORD=<unique locally generated secret>
```

No real password is committed to GitHub and the bootstrap helper does not print it into normal command output.

## View the local login

When you need to enter the OAuth login on this machine:

```bash
bash scripts/bootstrap-oauth.sh --show
```

This intentionally reveals the username/password only in the local terminal. Treat the output as a credential and do not paste it into issues, logs, screenshots, or commits.

## Rotate the password

```bash
bash scripts/bootstrap-oauth.sh --rotate https://your-domain.example.com
systemctl --user restart mcp-bridge.service
```

After rotation, existing OAuth clients may need to reconnect if their authorization can no longer be refreshed.

## Existing password behavior

Running the normal bootstrap command again preserves an existing `MCP_OAUTH_PASSWORD` entry. Use `--rotate` when you explicitly want a new password.

## Custom username

`admin` is the bootstrap default. To choose another username for one setup:

```bash
MCP_OAUTH_USERNAME=my-user \
  bash scripts/bootstrap-oauth.sh https://your-domain.example.com
```

## Loopback development

For local OAuth testing only, loopback HTTP is accepted by the helper:

```bash
bash scripts/bootstrap-oauth.sh http://127.0.0.1:3000
```

The helper adds `MCP_OAUTH_ALLOW_INSECURE_HTTP=true` only for that loopback HTTP configuration. Public deployments should use HTTPS.

## Security rules

- Never add a real `MCP_OAUTH_PASSWORD`, `MCP_TOKEN`, API key, cookie, tunnel token, private key, or credential file to Git.
- Keep `~/.config/mcp-bridge/env` private to the bridge account.
- Use a separate credential per installation.
- Rotate credentials if they are accidentally shared.
- Keep the MCP bridge behind authenticated HTTPS when it is reachable remotely.
- Keep the backend and Chrome CDP listeners private.
