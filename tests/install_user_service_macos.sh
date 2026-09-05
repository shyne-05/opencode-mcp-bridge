#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
fixture_root="$(mktemp -d "${TMPDIR:-/tmp}/mcp-bridge-macos-test.XXXXXX")"
trap 'rm -rf "$fixture_root"' EXIT
test_project="$fixture_root/repo & <code> \"quoted\" 'single'"
# Native macOS TMPDIR can end in a slash. Keep redundant separators here to
# verify equivalent log paths without changing their configured spelling.
test_user_dir="$fixture_root//user & <home> \"quoted\" 'single'"
# Exercise logical paths on every platform, including macOS /var aliases.
mkdir -p "$fixture_root/project-target/scripts" "$fixture_root/bin" "$test_user_dir"
ln -s "$fixture_root/project-target" "$test_project"

# Keep the real HOME untouched. Only the temporary installer's two user paths
# resolve through a fixture variable; all commands and plist generation remain.
sed 's/\$HOME/\$MCP_BRIDGE_TEST_HOME/g' \
  "$repo_root/scripts/install-user-service-macos.sh" \
  > "$test_project/scripts/install-user-service-macos.sh"
cat > "$test_project/scripts/package-release.sh" <<'STUB'
#!/usr/bin/env bash
exit 0
STUB
cat > "$fixture_root/bin/uname" <<'STUB'
#!/usr/bin/env bash
printf 'Darwin\n'
STUB
cat > "$fixture_root/bin/launchctl" <<'STUB'
#!/usr/bin/env bash
set -euo pipefail
if [[ "$1" == kickstart ]]; then [[ "$2" == -k ]]; fi
printf '%s\n' "$1" >> "${MCP_BRIDGE_TEST_CALLS:?}"
STUB
cat > "$fixture_root/bin/plutil" <<'STUB'
#!/usr/bin/env bash
set -euo pipefail
[[ "$1" == '-lint' ]]
python3 - "$2" <<'PY'
import os
import plistlib
import sys
from pathlib import Path

with open(sys.argv[1], "rb") as stream:
    plist = plistlib.load(stream)
# The installer intentionally preserves logical paths from Bash pwd. Resolving
# symlinks here would incorrectly reject valid /var versus /private/var paths.
root = Path(os.environ["MCP_BRIDGE_TEST_ROOT"]).absolute()
user_dir = Path(os.environ["MCP_BRIDGE_TEST_HOME"])
assert plist["Label"] == os.environ["MCP_BRIDGE_LAUNCHD_LABEL"]
assert plist["ProgramArguments"] == ["/bin/bash", str(root / "scripts/run-with-env.sh")]
assert plist["WorkingDirectory"] == str(root)
environment = plist["EnvironmentVariables"]
assert set(environment) == {"PATH", "MCP_BRIDGE_ENV_FILE"}
assert environment["PATH"] == os.environ["PATH"]
assert Path(environment["MCP_BRIDGE_ENV_FILE"]) == Path(os.environ["MCP_BRIDGE_TEST_ENV_PATH"])
assert "never-persist-fixture-secret" not in Path(sys.argv[1]).read_text()
assert Path(plist["StandardOutPath"]) == user_dir / "Library/Logs/mcp-bridge/stdout.log"
assert Path(plist["StandardErrorPath"]) == user_dir / "Library/Logs/mcp-bridge/stderr.log"
assert plist["RunAtLoad"] is True
assert plist["KeepAlive"] is True
assert plist["ProcessType"] == "Interactive"
PY
STUB
chmod +x "$fixture_root/bin/"*

PATH="$fixture_root/bin:$fixture_root/tool & <bin>:$PATH" \
  MCP_BRIDGE_TEST_HOME="$test_user_dir" \
  MCP_BRIDGE_TEST_ROOT="$test_project" \
  MCP_BRIDGE_TEST_CALLS="$fixture_root/launchctl-calls" \
  MCP_BRIDGE_TEST_ENV_PATH="$test_user_dir/.config/mcp-bridge/env" \
  MCP_BRIDGE_ENV_FILE='' \
  MCP_OAUTH_PASSWORD='never-persist-fixture-secret' \
  MCP_BRIDGE_LAUNCHD_LABEL='com.example.bridge&<test>' \
  "$BASH" "$test_project/scripts/install-user-service-macos.sh" \
  > "$fixture_root/install.log"

[[ "$(cat "$fixture_root/launchctl-calls")" == $'bootout\nbootstrap\nenable\nkickstart' ]]
# A relative config override is resolved against the caller, before the
# installer changes to the project directory. No config file is read.
(
  cd "$fixture_root"
  PATH="$fixture_root/bin:$PATH" \
    MCP_BRIDGE_TEST_HOME="$test_user_dir" \
    MCP_BRIDGE_TEST_ROOT="$test_project" \
    MCP_BRIDGE_TEST_CALLS="$fixture_root/custom-launchctl-calls" \
    MCP_BRIDGE_TEST_ENV_PATH="$fixture_root/config & <custom>/bridge env" \
    MCP_BRIDGE_ENV_FILE='config & <custom>/bridge env' \
    MCP_BRIDGE_LAUNCHD_LABEL='com.example.custom' \
    "$BASH" "$test_project/scripts/install-user-service-macos.sh" > "$fixture_root/custom-install.log"
)
[[ "$(cat "$fixture_root/custom-launchctl-calls")" == $'bootout\nbootstrap\nenable\nkickstart' ]]

# Labels cannot redirect plist writes out of LaunchAgents.
for invalid_label in '../outside' $'invalid\nlabel' $'invalid\rlabel'; do
  status=0
  PATH="$fixture_root/bin:$PATH" \
    MCP_BRIDGE_TEST_HOME="$test_user_dir" \
    MCP_BRIDGE_TEST_CALLS="$fixture_root/invalid-launchctl-calls" \
    MCP_BRIDGE_LAUNCHD_LABEL="$invalid_label" \
    "$BASH" "$test_project/scripts/install-user-service-macos.sh" > "$fixture_root/invalid.log" 2>&1 || status="$?"
  [[ "$status" == 2 ]]
  [[ ! -e "$fixture_root/invalid-launchctl-calls" ]]
done
printf 'macOS installer plist, environment, and label regressions passed.\n'
