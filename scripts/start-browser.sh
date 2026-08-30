#!/usr/bin/env bash
set -euo pipefail

PORT="${MCP_BROWSER_CDP_PORT:-9222}"
case "$(uname -s)" in
  Darwin)
    PROFILE="${MCP_BROWSER_PROFILE_DIR:-$HOME/Library/Application Support/mcp-bridge/chrome-profile}"
    candidates=(
      "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
      "/Applications/Chromium.app/Contents/MacOS/Chromium"
      "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge"
    )
    ;;
  Linux)
    PROFILE="${MCP_BROWSER_PROFILE_DIR:-${XDG_CONFIG_HOME:-$HOME/.config}/mcp-bridge-chrome}"
    candidates=(google-chrome-stable google-chrome chromium chromium-browser microsoft-edge-stable microsoft-edge)
    ;;
  *)
    printf 'Unsupported Unix platform: %s\n' "$(uname -s)" >&2
    exit 2
    ;;
esac

browser=""
for candidate in "${candidates[@]}"; do
  if [[ "$candidate" == /* ]]; then
    [[ -x "$candidate" ]] && browser="$candidate" && break
  elif command -v "$candidate" >/dev/null 2>&1; then
    browser="$(command -v "$candidate")"
    break
  fi
done
[[ -n "$browser" ]] || {
  printf 'No supported Chromium-based browser was found.\n' >&2
  exit 1
}

mkdir -p "$PROFILE"
"$browser" --remote-debugging-address=127.0.0.1 --remote-debugging-port="$PORT" --user-data-dir="$PROFILE" >/dev/null 2>&1 &
printf 'Started %s with CDP on 127.0.0.1:%s\n' "$browser" "$PORT"
