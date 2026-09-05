#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
fixture_root="$(mktemp -d "${TMPDIR:-/tmp}/mcp-bridge-bootstrap-test.XXXXXX")"
trap 'rm -rf "$fixture_root"' EXIT
mkdir -p "$fixture_root/bin"

# Never inspect the caller's environment file or produce real credentials.
export MCP_BRIDGE_ENV_FILE="$fixture_root/config with spaces/env"
export MCP_OAUTH_USERNAME='Fixture Admin'
export MCP_PUBLIC_URL=''
export BRIDGE_TEST_RANDOM='0123456789abcdef0123456789abcdef0123456789abcdef'
export BRIDGE_TEST_RANDOM_LOG="$fixture_root/random.log"
export PATH="$fixture_root/bin:$PATH"
cat > "$fixture_root/bin/openssl" <<'MOCK'
#!/bin/sh
[ "$#" -eq 3 ] && [ "$1" = rand ] && [ "$2" = -hex ] && [ "$3" = 24 ] || exit 90
printf 'called\n' >> "$BRIDGE_TEST_RANDOM_LOG"
printf '%s\n' "$BRIDGE_TEST_RANDOM"
MOCK
chmod +x "$fixture_root/bin/openssl"

run_bootstrap() (
  cd "$fixture_root"
  "$BASH" "$repo_root/scripts/bootstrap-oauth.sh" "$@" > "$fixture_root/output.log" 2>&1
)

assert_private_file() {
  [[ "$(find "$MCP_BRIDGE_ENV_FILE" -prune -perm 600 -print)" == "$MCP_BRIDGE_ENV_FILE" ]]
}

# Initialization supports paths and usernames with spaces, and keeps credentials
# out of its default output while creating private directory/file permissions.
run_bootstrap https://example.test
printf 'MCP_PUBLIC_URL=https://example.test\nMCP_OAUTH_USERNAME=Fixture Admin\nMCP_OAUTH_PASSWORD=%s\n' \
  "$BRIDGE_TEST_RANDOM" > "$fixture_root/expected.env"
cmp -s "$MCP_BRIDGE_ENV_FILE" "$fixture_root/expected.env"
[[ "$(cat "$fixture_root/output.log")" != *"$BRIDGE_TEST_RANDOM"* ]]
[[ "$(cat "$BRIDGE_TEST_RANDOM_LOG")" == called ]]
[[ "$(find "$fixture_root/config with spaces" -prune -perm 700 -print)" == "$fixture_root/config with spaces" ]]
assert_private_file

# A second initialization reuses the password without invoking randomness.
run_bootstrap https://example.test
cmp -s "$MCP_BRIDGE_ENV_FILE" "$fixture_root/expected.env"
[[ "$(cat "$BRIDGE_TEST_RANDOM_LOG")" == called ]]

# CRLF input, duplicate assignments, shell metacharacters, and an unterminated
# final entry are literal data. The final password assignment is authoritative.
fixture_password='fixture-password=$(touch should-not-exist)`touch also-should-not-exist`=value'
while IFS= read -r fixture_line; do
  printf '%s\r\n' "$fixture_line"
done > "$MCP_BRIDGE_ENV_FILE" <<'ENV'
# Preserve this comment
BRIDGE_LITERAL=$(touch should-not-exist) `touch also-should-not-exist` $HOME \path
MCP_PUBLIC_URL=http://localhost:1234
MCP_OAUTH_USERNAME=Old User
MCP_OAUTH_PASSWORD=obsolete-test-value
MCP_OAUTH_ALLOW_INSECURE_HTTP=true
ENV
printf 'MCP_OAUTH_PASSWORD=%s\r\n' "$fixture_password" >> "$MCP_BRIDGE_ENV_FILE"
printf 'UNTERMINATED=kept' >> "$MCP_BRIDGE_ENV_FILE"

# --show is explicitly requested only for synthetic credentials and captured.
run_bootstrap --show
[[ "$(cat "$fixture_root/output.log")" == "$(printf 'Username: Old User\nPassword: %s\n' "$fixture_password")" ]]
run_bootstrap https://example.test:8443
cat > "$fixture_root/expected.env" <<'ENV'
# Preserve this comment
BRIDGE_LITERAL=$(touch should-not-exist) `touch also-should-not-exist` $HOME \path
UNTERMINATED=kept
MCP_PUBLIC_URL=https://example.test:8443
MCP_OAUTH_USERNAME=Fixture Admin
ENV
printf 'MCP_OAUTH_PASSWORD=%s\n' "$fixture_password" >> "$fixture_root/expected.env"
cmp -s "$MCP_BRIDGE_ENV_FILE" "$fixture_root/expected.env"
[[ "$(cat "$BRIDGE_TEST_RANDOM_LOG")" == called ]]
[[ ! -e "$fixture_root/should-not-exist" && ! -e "$fixture_root/also-should-not-exist" ]]
[[ "$(cat "$fixture_root/output.log")" != *"$fixture_password"* ]]
assert_private_file

# An empty or whitespace-only final password is incomplete, including CRLF.
for empty_password in '' '   ' $'\t'; do
  printf 'MCP_OAUTH_USERNAME=Fixture Admin\r\nMCP_OAUTH_PASSWORD=old-test-value\r\nMCP_OAUTH_PASSWORD=%s\r\n' \
    "$empty_password" > "$MCP_BRIDGE_ENV_FILE"
  status=0
  run_bootstrap --show || status="$?"
  [[ "$status" == 1 ]]
  [[ "$(cat "$fixture_root/output.log")" != *old-test-value* ]]
  : > "$BRIDGE_TEST_RANDOM_LOG"
  run_bootstrap https://example.test
  printf 'MCP_PUBLIC_URL=https://example.test\nMCP_OAUTH_USERNAME=Fixture Admin\nMCP_OAUTH_PASSWORD=%s\n' \
    "$BRIDGE_TEST_RANDOM" > "$fixture_root/expected.env"
  cmp -s "$MCP_BRIDGE_ENV_FILE" "$fixture_root/expected.env"
  [[ "$(cat "$BRIDGE_TEST_RANDOM_LOG")" == called ]]
done

# Rotation always creates a new password and enables loopback HTTP explicitly.
export BRIDGE_TEST_RANDOM='abcdef0123456789abcdef0123456789abcdef0123456789'
: > "$BRIDGE_TEST_RANDOM_LOG"
run_bootstrap --rotate 'http://[::1]:8080'
printf 'MCP_PUBLIC_URL=http://[::1]:8080\nMCP_OAUTH_USERNAME=Fixture Admin\nMCP_OAUTH_PASSWORD=%s\nMCP_OAUTH_ALLOW_INSECURE_HTTP=true\n' \
  "$BRIDGE_TEST_RANDOM" > "$fixture_root/expected.env"
cmp -s "$MCP_BRIDGE_ENV_FILE" "$fixture_root/expected.env"
[[ "$(cat "$BRIDGE_TEST_RANDOM_LOG")" == called ]]
[[ "$(cat "$fixture_root/output.log")" != *"$BRIDGE_TEST_RANDOM"* ]]
assert_private_file

for origin in 'https://[2001:db8::1]:443' 'https://example.test' 'http://localhost' 'http://127.0.0.1:8080'; do
  run_bootstrap "$origin"
  if [[ "$origin" == http://* ]]; then
    [[ "$(awk '/^MCP_OAUTH_ALLOW_INSECURE_HTTP=/' "$MCP_BRIDGE_ENV_FILE")" == MCP_OAUTH_ALLOW_INSECURE_HTTP=true ]]
  else
    [[ -z "$(awk '/^MCP_OAUTH_ALLOW_INSECURE_HTTP=/' "$MCP_BRIDGE_ENV_FILE")" ]]
  fi
done

# Invalid inputs fail before modifying the file, without reflecting a supplied
# URL password or allowing new environment assignments through a line break.
cp "$MCP_BRIDGE_ENV_FILE" "$fixture_root/before.env"
for origin in 'https://example.test/path' 'https://example.test?query' 'https://example.test#fragment' \
  'https://user:secret-canary@example.test' 'https://example.test:bad' 'https://example.test\path' \
  'https://example.test with space' $'https://example.test\nMCP_TOKEN=secret-canary' \
  $'https://example.test\rMCP_TOKEN=secret-canary' 'http://remote.example.test'; do
  status=0
  run_bootstrap "$origin" || status="$?"
  [[ "$status" == 2 ]]
  cmp -s "$MCP_BRIDGE_ENV_FILE" "$fixture_root/before.env"
  [[ "$(cat "$fixture_root/output.log")" != *secret-canary* ]]
done
for username in '   ' $'\t' $'Fixture\nMCP_TOKEN=secret-canary' $'Fixture\rMCP_TOKEN=secret-canary'; do
  status=0
  MCP_OAUTH_USERNAME="$username" run_bootstrap https://example.test || status="$?"
  [[ "$status" == 2 ]]
  cmp -s "$MCP_BRIDGE_ENV_FILE" "$fixture_root/before.env"
  [[ "$(cat "$fixture_root/output.log")" != *secret-canary* ]]
done

# Reject a broken random source instead of writing a malformed password. Keep
# the previous file intact and leave no temporary environment file behind.
for invalid_random in 'short' 'xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx' \
  $'0123456789abcdef0123456789abcdef\nMCP_TOKEN=secret-canary'; do
  status=0
  BRIDGE_TEST_RANDOM="$invalid_random" run_bootstrap --rotate https://example.test || status="$?"
  [[ "$status" == 1 ]]
  cmp -s "$MCP_BRIDGE_ENV_FILE" "$fixture_root/before.env"
  [[ "$(cat "$fixture_root/output.log")" != *secret-canary* ]]
  [[ -z "$(find "$fixture_root/config with spaces" -name '.env.tmp.*' -print)" ]]
done

printf 'OAuth bootstrap regression tests passed.\n'
