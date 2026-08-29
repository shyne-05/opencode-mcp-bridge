#!/usr/bin/env bash
set -euo pipefail

umask 077

usage() {
  cat <<'EOF'
Usage:
  scripts/bootstrap-oauth.sh <https://public-origin>
  scripts/bootstrap-oauth.sh --show
  scripts/bootstrap-oauth.sh --rotate <https://public-origin>

Creates or updates the native user-service environment file with a safe OAuth
login. The default username is "admin". Passwords are generated locally and
are never stored in this repository.

Environment overrides:
  MCP_BRIDGE_ENV_FILE   Target EnvironmentFile (default: ~/.config/mcp-bridge/env)
  MCP_OAUTH_USERNAME    Login username (default: admin)
EOF
}

ENV_FILE="${MCP_BRIDGE_ENV_FILE:-${HOME:?HOME is required}/.config/mcp-bridge/env}"
USERNAME="${MCP_OAUTH_USERNAME:-admin}"
MODE="init"
PUBLIC_URL=""

case "${1:-}" in
  -h|--help)
    usage
    exit 0
    ;;
  --show)
    MODE="show"
    ;;
  --rotate)
    MODE="rotate"
    PUBLIC_URL="${2:-${MCP_PUBLIC_URL:-}}"
    ;;
  "")
    PUBLIC_URL="${MCP_PUBLIC_URL:-}"
    ;;
  *)
    PUBLIC_URL="$1"
    ;;
esac

show_credentials() {
  if [[ ! -f "$ENV_FILE" ]]; then
    printf 'OAuth environment file does not exist: %s\n' "$ENV_FILE" >&2
    exit 1
  fi
  local username password
  username="$(sed -n 's/^MCP_OAUTH_USERNAME=//p' "$ENV_FILE" | tail -n 1)"
  password="$(sed -n 's/^MCP_OAUTH_PASSWORD=//p' "$ENV_FILE" | tail -n 1)"
  if [[ -z "$username" || -z "$password" ]]; then
    printf 'OAuth credentials are incomplete in %s\n' "$ENV_FILE" >&2
    exit 1
  fi
  printf 'Username: %s\nPassword: %s\n' "$username" "$password"
}

if [[ "$MODE" == "show" ]]; then
  show_credentials
  exit 0
fi

if [[ -z "$PUBLIC_URL" ]]; then
  printf 'A public OAuth origin is required.\n\n' >&2
  usage >&2
  exit 2
fi

if [[ ! "$PUBLIC_URL" =~ ^https://[^/]+(:[0-9]+)?$ ]] \
  && [[ ! "$PUBLIC_URL" =~ ^http://(127\.0\.0\.1|localhost|\[::1\])(:[0-9]+)?$ ]]; then
  printf 'Invalid public origin: %s\nUse an HTTPS origin without a path, or loopback HTTP for local testing.\n' "$PUBLIC_URL" >&2
  exit 2
fi

ENV_DIR="$(dirname "$ENV_FILE")"
mkdir -p "$ENV_DIR"
chmod 700 "$ENV_DIR" 2>/dev/null || true

existing_password_line=""
if [[ -f "$ENV_FILE" && "$MODE" != "rotate" ]]; then
  existing_password_line="$(grep -E '^MCP_OAUTH_PASSWORD=' "$ENV_FILE" | tail -n 1 || true)"
fi

if [[ -n "$existing_password_line" ]]; then
  password_line="$existing_password_line"
else
  if command -v openssl >/dev/null 2>&1; then
    password="$(openssl rand -hex 24)"
  else
    password="$(od -An -N24 -tx1 /dev/urandom | tr -d ' \n')"
  fi
  if [[ ${#password} -lt 24 ]]; then
    printf 'Failed to generate a sufficiently strong OAuth password.\n' >&2
    exit 1
  fi
  password_line="MCP_OAUTH_PASSWORD=$password"
fi

TMP_FILE="$(mktemp "$ENV_DIR/.env.tmp.XXXXXX")"
cleanup() {
  rm -f "$TMP_FILE"
}
trap cleanup EXIT

if [[ -f "$ENV_FILE" ]]; then
  awk '!/^(MCP_PUBLIC_URL|MCP_OAUTH_USERNAME|MCP_OAUTH_PASSWORD|MCP_OAUTH_ALLOW_INSECURE_HTTP)=/' "$ENV_FILE" > "$TMP_FILE"
fi

{
  printf 'MCP_PUBLIC_URL=%s\n' "$PUBLIC_URL"
  printf 'MCP_OAUTH_USERNAME=%s\n' "$USERNAME"
  printf '%s\n' "$password_line"
  if [[ "$PUBLIC_URL" =~ ^http:// ]]; then
    printf 'MCP_OAUTH_ALLOW_INSECURE_HTTP=true\n'
  fi
} >> "$TMP_FILE"

chmod 600 "$TMP_FILE"
mv -f "$TMP_FILE" "$ENV_FILE"
trap - EXIT
chmod 600 "$ENV_FILE"

printf 'OAuth bootstrap configured safely.\n'
printf 'Username: %s\n' "$USERNAME"
printf 'Credential file: %s (mode 600)\n' "$ENV_FILE"
printf 'The password is stored locally and is not printed by default.\n'
printf 'To view it on this machine: scripts/bootstrap-oauth.sh --show\n'
if [[ "$MODE" == "rotate" ]]; then
  printf 'Password rotated. Restart the bridge service before reconnecting OAuth clients.\n'
fi
