#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
fixture_root="$(mktemp -d "${TMPDIR:-/tmp}/mcp-bridge-browser-test.XXXXXX")"
trap 'rm -rf "$fixture_root"' EXIT
mkdir -p "$fixture_root/bin" "$fixture_root/scripts" "$fixture_root/system applications"
test_user_dir="$fixture_root/user & <home> 'single'"
mkdir -p "$test_user_dir"
# Both system and per-user application paths stay inside the fixture. No
# installed browser, real profile, or debugging endpoint is touched.
sed -e 's/\$HOME/\$MCP_BRIDGE_TEST_HOME/g' \
  -e 's|"/Applications/|"$MCP_BRIDGE_TEST_SYSTEM_APPS/|g' \
  "$repo_root/scripts/start-browser.sh" > "$fixture_root/scripts/start-browser.sh"
cat > "$fixture_root/bin/uname" <<'STUB'
#!/usr/bin/env bash
printf '%s\n' "${MCP_BRIDGE_TEST_OS:?}"
STUB
chmod +x "$fixture_root/bin/uname"

make_browser() {
  mkdir -p "$(dirname "$1")"
  cat > "$1" <<'STUB'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$@" > "${MCP_BRIDGE_TEST_ARGUMENTS:?}"
printf 'done\n' > "${MCP_BRIDGE_TEST_DONE:?}"
STUB
  chmod +x "$1"
}

run_launcher() {
  PATH="$fixture_root/bin:$PATH" \
    MCP_BRIDGE_TEST_HOME="$test_user_dir" \
    MCP_BRIDGE_TEST_SYSTEM_APPS="$fixture_root/system applications" \
    MCP_BRIDGE_TEST_OS="$test_os" \
    MCP_BRIDGE_TEST_ARGUMENTS="$fixture_root/arguments" \
    MCP_BRIDGE_TEST_DONE="$fixture_root/done" \
    MCP_BROWSER_PROFILE_DIR="$profile_override" \
    "$BASH" "$fixture_root/scripts/start-browser.sh"
}

check_arguments() {
  local attempt
  for ((attempt=0; attempt<100; attempt++)); do
    [[ ! -e "$fixture_root/done" ]] || break
    sleep 0.05
  done
  [[ -e "$fixture_root/done" ]]
  [[ "$(cat "$fixture_root/arguments")" == "--remote-debugging-address=127.0.0.1"$'\n'"--remote-debugging-port=9222"$'\n'"--user-data-dir=$expected_profile" ]]
  [[ -d "$expected_profile" ]]
  rm "$fixture_root/done" "$fixture_root/arguments"
}

test_os='Darwin'
profile_override=''
expected_profile="$test_user_dir/Library/Application Support/mcp-bridge/chrome-profile"
user_browser="$test_user_dir/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
make_browser "$user_browser"
run_launcher > "$fixture_root/launch.log"
check_arguments
[[ "$(cat "$fixture_root/launch.log")" == "Started $user_browser with CDP on 127.0.0.1:9222" ]]

# System apps still take priority when installed, and custom profile paths
# containing spaces remain a single argument.
system_browser="$fixture_root/system applications/Google Chrome.app/Contents/MacOS/Google Chrome"
make_browser "$system_browser"
profile_override="$fixture_root/profiles/custom & spaced"
expected_profile="$profile_override"
run_launcher > "$fixture_root/launch.log"
check_arguments
[[ "$(cat "$fixture_root/launch.log")" == "Started $system_browser with CDP on 127.0.0.1:9222" ]]

rm "$user_browser" "$system_browser"
status=0
run_launcher > "$fixture_root/missing.log" 2>&1 || status="$?"
[[ "$status" == 1 ]]
[[ ! -e "$fixture_root/done" ]]

# Linux command discovery is still exercised through a fixture executable.
test_os='Linux'
make_browser "$fixture_root/bin/google-chrome-stable"
run_launcher > "$fixture_root/launch.log"
check_arguments

test_os='Unsupported'
status=0
run_launcher > "$fixture_root/unsupported.log" 2>&1 || status="$?"
[[ "$status" == 2 ]]
[[ ! -e "$fixture_root/done" ]]
printf 'Browser launcher discovery and argument regressions passed.\n'
