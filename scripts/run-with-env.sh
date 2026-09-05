#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ENV_FILE="${MCP_BRIDGE_ENV_FILE:-${HOME:?HOME is required}/.config/mcp-bridge/env}"

if [[ -f "$ENV_FILE" ]]; then
  while IFS= read -r line || [[ -n "$line" ]]; do
    line="${line%$'\r'}"
    [[ -z "$line" || "$line" == \#* ]] && continue
    name="${line%%=*}"
    value=""
    if [[ "$line" == *=* ]]; then
      value="${line#*=}"
    fi
    [[ "$name" =~ ^[A-Za-z_][A-Za-z0-9_]*$ ]] || {
      printf 'invalid environment variable name in %s\n' "$ENV_FILE" >&2
      exit 2
    }
    export "$name=$value"
  done < "$ENV_FILE"
fi

exec "$ROOT/target/release/mcp-bridge" "$@"
