#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

SERVICE="${MCP_BRIDGE_SYSTEMD_SERVICE:-mcp-bridge.service}"

fail() {
  printf 'deploy verification failed: %s\n' "$*" >&2
  exit 1
}

command -v git >/dev/null || fail "git is required"
command -v systemctl >/dev/null || fail "systemctl is required"
command -v curl >/dev/null || fail "curl is required"
command -v ss >/dev/null || fail "ss is required"
command -v python3 >/dev/null || fail "python3 is required"

[[ -z "$(git status --porcelain)" ]] || fail "working tree must be clean before deployment"

git fetch --quiet origin main
local_head="$(git rev-parse HEAD)"
remote_head="$(git rev-parse origin/main)"
[[ "$local_head" == "$remote_head" ]] || fail "local HEAD does not match origin/main"

scripts/package-release.sh
expected_commit="$(git rev-parse --short=12 HEAD)"
expected_version="$(awk -F '"' '$1 ~ /^version = / { print $2; exit }' Cargo.toml)"
expected_exe="$ROOT/target/release/mcp-bridge"

systemctl --user restart "$SERVICE"
systemctl --user is-active --quiet "$SERVICE" || fail "$SERVICE is not active after restart"

pid="$(systemctl --user show "$SERVICE" --property=MainPID --value)"
[[ "$pid" =~ ^[1-9][0-9]*$ ]] || fail "$SERVICE has no running MainPID"

running_exe="$(readlink "/proc/$pid/exe" 2>/dev/null || true)"
[[ "$running_exe" == "$expected_exe" ]] || fail "service is not running the freshly built release binary"

listen_addr="$(ss -H -ltnp 2>/dev/null | awk -v needle="pid=$pid," 'index($0, needle) { print $4; exit }')"
[[ -n "$listen_addr" ]] || fail "could not identify the bridge listening address for PID $pid"

port="${listen_addr##*:}"
host="${listen_addr%:*}"
case "$host" in
  '*'|'0.0.0.0'|'[::]') host='127.0.0.1' ;;
esac
if [[ "$host" == *:* && "$host" != \[*\] ]]; then
  host="[$host]"
fi
base_url="http://$host:$port"

for _ in {1..40}; do
  if curl --fail --silent --show-error --max-time 2 "$base_url/live" >/dev/null 2>&1; then
    break
  fi
  sleep 0.25
done
curl --fail --silent --show-error --max-time 2 "$base_url/live" >/dev/null || fail "/live is not healthy"

for _ in {1..40}; do
  if curl --fail --silent --show-error --max-time 2 "$base_url/ready" >/dev/null 2>&1; then
    break
  fi
  sleep 0.25
done
curl --fail --silent --show-error --max-time 2 "$base_url/ready" >/dev/null || fail "/ready is not healthy"

root_json="$(curl --fail --silent --show-error --max-time 2 "$base_url/")"
ROOT_JSON="$root_json" EXPECTED_COMMIT="$expected_commit" EXPECTED_VERSION="$expected_version" python3 - <<'PY'
import json
import os
import sys

try:
    payload = json.loads(os.environ["ROOT_JSON"])
    build = payload["build"]
    assert payload["version"] == os.environ["EXPECTED_VERSION"]
    assert build["commit"] == os.environ["EXPECTED_COMMIT"]
    assert build["dirty"] is False
    assert build["browser_helper_protocol"] == "mcp-browser-helper/2"
except (AssertionError, KeyError, TypeError, json.JSONDecodeError) as exc:
    print(f"invalid build provenance: {exc}", file=sys.stderr)
    raise SystemExit(1)
PY

printf 'MCP Bridge %s deployed at %s (PID %s, commit %s, dirty=false, helper=%s)\n' \
  "$expected_version" "$base_url" "$pid" "$expected_commit" 'mcp-browser-helper/2'
