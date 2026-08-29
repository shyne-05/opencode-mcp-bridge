#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

SERVICE="${MCP_BRIDGE_SYSTEMD_SERVICE:-mcp-bridge.service}"
STATE_DIR="${MCP_BRIDGE_DEPLOY_STATE_DIR:-${XDG_STATE_HOME:-${HOME:-/tmp}/.local/state}/mcp-bridge}"
ROLLBACK_EXE="$STATE_DIR/rollback-mcp-bridge"
ROLLBACK_BROWSER="$STATE_DIR/rollback-browser.cjs"
LOCK_FILE="$STATE_DIR/deploy.lock"

fail() {
  printf 'deploy verification failed: %s\n' "$*" >&2
  exit 1
}

require_command() {
  command -v "$1" >/dev/null || fail "$1 is required"
}

require_runtime_tools() {
  for command in systemctl curl ss python3 install cp mv chmod mkdir flock; do
    require_command "$command"
  done
}

acquire_deploy_lock() {
  mkdir -p "$STATE_DIR"
  chmod 700 "$STATE_DIR"
  [[ ! -L "$STATE_DIR" ]] || fail "deployment state directory must not be a symlink"
  exec {DEPLOY_LOCK_FD}>"$LOCK_FILE"
  chmod 600 "$LOCK_FILE"
  flock -n "$DEPLOY_LOCK_FD" || fail "another deployment is already running"
}

service_pid() {
  systemctl --user show "$SERVICE" --property=MainPID --value
}

require_active_service() {
  systemctl --user is-active --quiet "$SERVICE" || fail "$SERVICE must be active before deployment"
  local pid
  pid="$(service_pid)"
  [[ "$pid" =~ ^[1-9][0-9]*$ ]] || fail "$SERVICE has no running MainPID"
  [[ -r "/proc/$pid/exe" ]] || fail "cannot access the current bridge executable"
}

prepare_rollback() {
  local expected_exe="$1"
  local expected_browser="$2"
  local pid source tmp

  require_active_service
  local service_exec
  service_exec="$(systemctl --user show "$SERVICE" --property=ExecStart --value)"
  [[ "$service_exec" == *"path=$expected_exe"* ]] ||
    fail "$SERVICE ExecStart does not point to the packaged release binary"

  pid="$(service_pid)"
  source="/proc/$pid/exe"
  tmp="$STATE_DIR/.rollback-mcp-bridge.$$"
  rm -f "$tmp"
  if ! cp --dereference --preserve=mode,timestamps "$source" "$tmp"; then
    rm -f "$tmp"
    fail "could not copy the current bridge executable for rollback"
  fi
  chmod 700 "$tmp"
  mv -f "$tmp" "$ROLLBACK_EXE"

  ROLLBACK_BROWSER_PRESENT=no
  if [[ -f "$expected_browser" ]]; then
    tmp="$STATE_DIR/.rollback-browser.cjs.$$"
    rm -f "$tmp"
    if ! cp --preserve=mode,timestamps "$expected_browser" "$tmp"; then
      rm -f "$tmp"
      fail "could not copy the current browser helper for rollback"
    fi
    chmod 700 "$tmp"
    mv -f "$tmp" "$ROLLBACK_BROWSER"
    ROLLBACK_BROWSER_PRESENT=yes
  else
    rm -f "$ROLLBACK_BROWSER"
  fi
}

wait_for_pid_gone() {
  local pid="$1"
  for _ in {1..40}; do
    if ! kill -0 "$pid" 2>/dev/null; then
      return 0
    fi
    sleep 0.25
  done
  return 1
}

listener_addresses_for_pid() {
  local pid="$1"
  ss -H -ltnp 2>/dev/null | awk -v needle="pid=$pid," 'index($0, needle) { print $4 }'
}

base_url_for_listener() {
  local listen_addr="$1"
  local port="${listen_addr##*:}"
  local host="${listen_addr%:*}"
  case "$host" in
    '*'|'0.0.0.0') host='127.0.0.1' ;;
    '[::]'|'::') host='[::1]' ;;
  esac
  if [[ "$host" == *:* && "$host" != \[*\] ]]; then
    host="[$host]"
  fi
  printf 'http://%s:%s\n' "$host" "$port"
}

http_status() {
  curl --silent --output /dev/null --write-out '%{http_code}' \
    --connect-timeout 1 --max-time 2 "$1" 2>/dev/null || true
}

wait_for_http_200() {
  local url="$1"
  for _ in {1..40}; do
    [[ "$(http_status "$url")" == 200 ]] && return 0
    sleep 0.25
  done
  return 1
}

verify_rollback() {
  local expected_exe="$1"
  systemctl --user is-active --quiet "$SERVICE" || return 1

  local pid running_exe
  pid="$(service_pid)"
  [[ "$pid" =~ ^[1-9][0-9]*$ ]] || return 1
  running_exe="$(readlink "/proc/$pid/exe" 2>/dev/null || true)"
  [[ "$running_exe" == "$expected_exe" ]] || return 1
  [[ "$running_exe" != *' (deleted)' ]] || return 1

  local listener_addresses=()
  mapfile -t listener_addresses < <(listener_addresses_for_pid "$pid")
  [[ "${#listener_addresses[@]}" -eq 1 ]] || return 1
  local base_url
  base_url="$(base_url_for_listener "${listener_addresses[0]}")"
  wait_for_http_200 "$base_url/live"
}

restore_rollback() {
  local expected_exe="$1"
  local expected_browser="$2"
  local browser_present="$3"

  printf 'attempting rollback of %s\n' "$SERVICE" >&2
  systemctl --user stop "$SERVICE" >/dev/null 2>&1 || true
  if ! install -m 0755 "$ROLLBACK_EXE" "$expected_exe"; then
    printf 'rollback failed: could not restore the previous bridge executable\n' >&2
    return 1
  fi
  if [[ "$browser_present" == yes ]]; then
    if ! install -m 0755 "$ROLLBACK_BROWSER" "$expected_browser"; then
      printf 'rollback failed: could not restore the previous browser helper\n' >&2
      return 1
    fi
  fi
  if ! systemctl --user start "$SERVICE" >/dev/null 2>&1; then
    printf 'rollback failed: could not start %s\n' "$SERVICE" >&2
    return 1
  fi
  if ! verify_rollback "$expected_exe"; then
    printf 'rollback failed: the previous service did not pass the liveness check\n' >&2
    return 1
  fi
  printf 'rollback completed; the previous %s service is live\n' "$SERVICE" >&2
}

restart_and_verify() {
  local expected_commit="$1"
  local expected_version="$2"
  local expected_exe="$3"
  local previous_pid="$4"

  systemctl --user restart "$SERVICE" || {
    printf 'could not restart %s\n' "$SERVICE" >&2
    return 1
  }
  wait_for_pid_gone "$previous_pid" || {
    printf 'previous bridge PID %s did not exit after restart\n' "$previous_pid" >&2
    return 1
  }
  systemctl --user is-active --quiet "$SERVICE" || {
    printf '%s is not active after restart\n' "$SERVICE" >&2
    return 1
  }

  local pid running_exe
  pid="$(service_pid)"
  [[ "$pid" =~ ^[1-9][0-9]*$ ]] || {
    printf '%s has no running MainPID\n' "$SERVICE" >&2
    return 1
  }
  running_exe="$(readlink "/proc/$pid/exe" 2>/dev/null || true)"
  [[ "$running_exe" == "$expected_exe" ]] || {
    printf '%s is not running the freshly built release binary\n' "$SERVICE" >&2
    return 1
  }
  [[ "$running_exe" != *' (deleted)' ]] || {
    printf '%s is running a deleted executable image\n' "$SERVICE" >&2
    return 1
  }

  local listener_addresses=()
  mapfile -t listener_addresses < <(listener_addresses_for_pid "$pid")
  [[ "${#listener_addresses[@]}" -eq 1 ]] || {
    printf 'expected exactly one bridge listener for PID %s, found %s\n' "$pid" "${#listener_addresses[@]}" >&2
    return 1
  }
  local base_url
  base_url="$(base_url_for_listener "${listener_addresses[0]}")"
  wait_for_http_200 "$base_url/live" || {
    printf '/live is not healthy at %s\n' "$base_url" >&2
    return 1
  }
  wait_for_http_200 "$base_url/ready" || {
    printf '/ready is not healthy at %s\n' "$base_url" >&2
    return 1
  }

  local root_json
  root_json="$(curl --silent --show-error --fail --max-time 2 "$base_url/")" || {
    printf 'root provenance endpoint is unavailable at %s\n' "$base_url" >&2
    return 1
  }
  ROOT_JSON="$root_json" EXPECTED_COMMIT="$expected_commit" EXPECTED_VERSION="$expected_version" python3 - <<'PY'
import json
import os
import sys

try:
    payload = json.loads(os.environ["ROOT_JSON"])
    build = payload["build"]
    assert payload["name"] == "mcp-bridge"
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
}

run_worker() {
  local expected_commit="$1"
  local expected_version="$2"
  local expected_exe="$3"
  local previous_pid="$4"
  local rollback_exe="$5"
  local rollback_browser="$6"
  local browser_present="$7"

  [[ "$previous_pid" =~ ^[1-9][0-9]*$ ]] || fail "worker received an invalid previous PID"
  [[ "$browser_present" == yes || "$browser_present" == no ]] || fail "worker received invalid rollback metadata"
  ROLLBACK_EXE="$rollback_exe"
  ROLLBACK_BROWSER="$rollback_browser"
  STATE_DIR="$(dirname "$ROLLBACK_EXE")"
  LOCK_FILE="$STATE_DIR/deploy.lock"
  require_runtime_tools
  acquire_deploy_lock

  if ! restart_and_verify "$expected_commit" "$expected_version" "$expected_exe" "$previous_pid"; then
    restore_rollback "$expected_exe" "$ROOT/target/release/browser.cjs" "$browser_present" || true
    return 1
  fi
}

if [[ "${1:-}" == "--worker" ]]; then
  [[ $# -eq 8 ]] || fail "worker requires expected commit, version, executable, previous PID, rollback executable, rollback browser, and browser metadata"
  run_worker "$2" "$3" "$4" "$5" "$6" "$7" "$8"
  exit $?
fi

require_command git
require_command systemd-run
require_runtime_tools
acquire_deploy_lock

[[ -z "$(git status --porcelain)" ]] || fail "working tree must be clean before deployment"
[[ "$(git branch --show-current)" == "main" ]] || fail "deployment must run from the main branch"

git fetch --quiet origin main
git merge --ff-only origin/main >/dev/null
[[ -z "$(git status --porcelain)" ]] || fail "working tree became dirty during synchronization"

local_head="$(git rev-parse HEAD)"
remote_head="$(git rev-parse origin/main)"
[[ "$local_head" == "$remote_head" ]] || fail "local HEAD does not match origin/main"

expected_commit="$(git rev-parse --short=12 HEAD)"
expected_version="$(awk -F '"' '$1 ~ /^version = / { print $2; exit }' Cargo.toml)"
expected_exe="$ROOT/target/release/mcp-bridge"
expected_browser="$ROOT/target/release/browser.cjs"
[[ -n "$expected_version" ]] || fail "could not determine package version"

previous_pid="$(service_pid)"
prepare_rollback "$expected_exe" "$expected_browser"
env -u MCP_BUILD_COMMIT_OVERRIDE -u MCP_BUILD_DIRTY_OVERRIDE scripts/package-release.sh

if grep -Fq "/$SERVICE" "/proc/$$/cgroup" 2>/dev/null; then
  unit="mcp-bridge-deploy-$(date +%s)-$$"
  systemd-run --user \
    --unit="$unit" \
    --on-active=1s \
    --collect \
    --quiet \
    bash "$ROOT/scripts/deploy-user-service.sh" \
      --worker "$expected_commit" "$expected_version" "$expected_exe" "$previous_pid" \
      "$ROLLBACK_EXE" "$ROLLBACK_BROWSER" "$ROLLBACK_BROWSER_PRESENT"
  printf 'Deployment handed off to %s so %s can restart without killing the deploy worker.\n' \
    "$unit.service" "$SERVICE"
  exit 0
fi

if ! restart_and_verify "$expected_commit" "$expected_version" "$expected_exe" "$previous_pid"; then
  restore_rollback "$expected_exe" "$expected_browser" "$ROLLBACK_BROWSER_PRESENT" || true
  exit 1
fi
