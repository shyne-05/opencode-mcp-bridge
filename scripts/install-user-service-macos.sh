#!/usr/bin/env bash
set -euo pipefail

[[ "$(uname -s)" == "Darwin" ]] || {
  printf 'This installer is for macOS only.\n' >&2
  exit 2
}

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
LABEL="${MCP_BRIDGE_LAUNCHD_LABEL:-io.mcpbridge.agent}"
if [[ "$LABEL" == */* || "$LABEL" == *$'\n'* || "$LABEL" == *$'\r'* ]]; then
  printf 'The LaunchAgent label must not contain slashes or line breaks.\n' >&2
  exit 2
fi
# launchd does not inherit the installer's shell environment. Preserve the
# executable search path (including Homebrew/nvm) and the selected config path.
SERVICE_PATH="${PATH:-/usr/bin:/bin:/usr/sbin:/sbin}"
ENV_FILE="${MCP_BRIDGE_ENV_FILE:-$HOME/.config/mcp-bridge/env}"
[[ "$ENV_FILE" == /* ]] || ENV_FILE="$PWD/$ENV_FILE"
PLIST="$HOME/Library/LaunchAgents/$LABEL.plist"
LOG_DIR="$HOME/Library/Logs/mcp-bridge"

cd "$ROOT"
bash scripts/package-release.sh
mkdir -p "$(dirname "$PLIST")" "$LOG_DIR"

xml_escape() {
  # sed replacement escaping is consistent on macOS Bash 3.2 and newer Bash.
  printf '%s' "$1" | sed -e 's/&/\&amp;/g' -e 's/</\&lt;/g' -e 's/>/\&gt;/g'
}

LABEL_XML="$(xml_escape "$LABEL")"
ROOT_XML="$(xml_escape "$ROOT")"
LOG_DIR_XML="$(xml_escape "$LOG_DIR")"
SERVICE_PATH_XML="$(xml_escape "$SERVICE_PATH")"
ENV_FILE_XML="$(xml_escape "$ENV_FILE")"

cat > "$PLIST" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>$LABEL_XML</string>
  <key>ProgramArguments</key>
  <array>
    <string>/bin/bash</string>
    <string>$ROOT_XML/scripts/run-with-env.sh</string>
  </array>
  <key>WorkingDirectory</key><string>$ROOT_XML</string>
  <key>EnvironmentVariables</key>
  <dict>
    <key>PATH</key><string>$SERVICE_PATH_XML</string>
    <key>MCP_BRIDGE_ENV_FILE</key><string>$ENV_FILE_XML</string>
  </dict>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>ProcessType</key><string>Interactive</string>
  <key>StandardOutPath</key><string>$LOG_DIR_XML/stdout.log</string>
  <key>StandardErrorPath</key><string>$LOG_DIR_XML/stderr.log</string>
</dict>
</plist>
PLIST

plutil -lint "$PLIST" >/dev/null
launchctl bootout "gui/$UID/$LABEL" >/dev/null 2>&1 || true
launchctl bootstrap "gui/$UID" "$PLIST"
launchctl enable "gui/$UID/$LABEL"
launchctl kickstart -k "gui/$UID/$LABEL"
printf 'Installed and started macOS LaunchAgent %s\n' "$LABEL"
printf 'Plist: %s\n' "$PLIST"
