#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
fixture_root="$(mktemp -d "${TMPDIR:-/tmp}/mcp-bridge-env-test.XXXXXX")"
trap 'rm -rf "$fixture_root"' EXIT
mkdir -p "$fixture_root/scripts" "$fixture_root/target/release"
cp "$repo_root/scripts/run-with-env.sh" "$fixture_root/scripts/run-with-env.sh"

# An isolated fake binary inspects only these fixture values and arguments.
cat > "$fixture_root/target/release/mcp-bridge" <<'BINARY'
#!/usr/bin/env bash
set -euo pipefail
[[ "$#" == 4 ]]
[[ "$1" == 'one two' ]]
[[ "$2" == '--check' ]]
[[ -z "$3" ]]
[[ "$4" == '$literal' ]]
[[ "$BRIDGE_TEST_PLAIN" == 'hello world' ]]
[[ "$BRIDGE_TEST_EQUALS" == 'one=two=' ]]
[[ "$BRIDGE_TEST_SPACES" == ' leading and trailing ' ]]
[[ "$BRIDGE_TEST_QUOTED" == '"$(touch should-not-exist) `touch also-should-not-exist` $HOME \path"' ]]
[[ "$BRIDGE_TEST_EMPTY" == '' ]]
[[ "$BRIDGE_TEST_LAST" == 'last=value' ]]
printf 'called\n' > "${BRIDGE_TEST_CALL_FILE:?}"
BINARY
chmod +x "$fixture_root/target/release/mcp-bridge"

# Every complete line uses CRLF; the final assignment has no line ending.
while IFS= read -r fixture_line; do
  printf '%s\r\n' "$fixture_line"
done > "$fixture_root/env" <<'ENV'
# Ignored comment with $(touch comment-should-not-exist)

BRIDGE_TEST_PLAIN=hello world
BRIDGE_TEST_EQUALS=one=two=
BRIDGE_TEST_QUOTED="$(touch should-not-exist) `touch also-should-not-exist` $HOME \path"
BRIDGE_TEST_EMPTY=
ENV
printf '%s\r\n' 'BRIDGE_TEST_SPACES= leading and trailing ' >> "$fixture_root/env"
printf '%s' 'BRIDGE_TEST_LAST=last=value' >> "$fixture_root/env"

(
  cd "$fixture_root"
  MCP_BRIDGE_ENV_FILE="$fixture_root/env" \
    BRIDGE_TEST_CALL_FILE="$fixture_root/called" \
    "$BASH" scripts/run-with-env.sh 'one two' --check '' '$literal'
)
[[ -f "$fixture_root/called" ]]
[[ ! -e "$fixture_root/should-not-exist" ]]
[[ ! -e "$fixture_root/also-should-not-exist" ]]
[[ ! -e "$fixture_root/comment-should-not-exist" ]]

# Malformed unterminated entries must fail before executing the binary, and
# diagnostics must never reproduce their values or malformed secret contents.
for invalid_entry in 'INVALID-NAME=secret-canary' '=secret-canary' 'invalid-secret-canary!'; do
  printf '%s' "$invalid_entry" > "$fixture_root/env"
  status=0
  MCP_BRIDGE_ENV_FILE="$fixture_root/env" \
    BRIDGE_TEST_CALL_FILE="$fixture_root/invalid-called" \
    "$BASH" "$fixture_root/scripts/run-with-env.sh" \
    > "$fixture_root/error.log" 2>&1 || status="$?"
  [[ "$status" == 2 ]]
  [[ ! -f "$fixture_root/invalid-called" ]]
  [[ "$(cat "$fixture_root/error.log")" != *secret-canary* ]]
done

printf 'Environment runner regression tests passed.\n'
