#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
fixture_root="$(mktemp -d "${TMPDIR:-/tmp}/mcp-bridge-secret-test.XXXXXX")"
trap 'rm -rf "$fixture_root"' EXIT
fixture_repo="$fixture_root/repo with spaces"
mkdir -p "$fixture_repo/scripts" "$fixture_repo/tests"
cp "$repo_root/scripts/scan-secrets.sh" "$fixture_repo/scripts/scan-secrets.sh"
cp "$repo_root/.gitignore" "$fixture_repo/.gitignore"

# Every Git mutation below belongs to an isolated fixture. Disable personal Git
# configuration, signing, and hooks without changing HOME or the real worktree.
export GIT_CONFIG_NOSYSTEM=1
export GIT_CONFIG_GLOBAL=/dev/null
fixture_git() {
  git -C "$fixture_repo" -c core.hooksPath=/dev/null -c commit.gpgSign=false \
    -c user.name='Fixture Author' -c user.email='fixture@example.invalid' "$@"
}
fixture_git init -q

# Construct fake signatures at runtime so this regression source is scan-clean.
fake_token='ghp_'
fake_token+='0123456789abcdefghijklmnopqrstuv'
allowed_canary='const PRIVATE_KEY_SECRET = "-----BEGIN '
allowed_canary+='PRIVATE KEY-----browser-oauth-private-key-canary-7f4c";'
printf 'clean\n' > "$fixture_repo/tracked.txt"
printf '%s\n' "$allowed_canary" > "$fixture_repo/tests/oauth_browser.cjs"
fixture_git add .
fixture_git commit -qm 'Clean synthetic fixture'

run_scan() {
  local expected="$1" status=0
  shift
  "$BASH" "$fixture_repo/scripts/scan-secrets.sh" "$@" > "$fixture_root/report" 2>&1 || status="$?"
  if [[ "$status" != "$expected" ]]; then
    printf 'Unexpected secret scanner exit: expected=%s actual=%s mode=%s\n' "$expected" "$status" "${1:-history}" >&2
    exit 1
  fi
  if grep -F "$fake_token" "$fixture_root/report" >/dev/null; then
    printf 'Scanner leaked a synthetic credential value.\n' >&2
    exit 1
  fi
}

run_scan 0
run_scan 0 --current
run_scan 0 --staged
run_scan 0 --worktree
run_scan 2 --unknown

# The index and worktree must be inspected independently. A clean worktree
# cannot conceal a secret already staged for the next commit, or vice versa.
printf '%s\n' "$fake_token" > "$fixture_repo/tracked.txt"
fixture_git add tracked.txt
printf 'clean\n' > "$fixture_repo/tracked.txt"
run_scan 1 --staged
run_scan 0 --worktree
run_scan 0 --current
fixture_git reset -q HEAD -- tracked.txt
printf '%s\n' "$fake_token" > "$fixture_repo/tracked.txt"
run_scan 0 --staged
run_scan 1 --worktree
fixture_git checkout -- tracked.txt

# The exact canary declaration is allowed only in its one regression fixture.
# Additional keys, same-line additions, and lookalike declarations still fail.
for variant in additional same-line lookalike; do
  case "$variant" in
    additional) printf '%s\n%s\n' "$allowed_canary" "$fake_token" ;;
    same-line) printf '%s %s\n' "$allowed_canary" "$fake_token" ;;
    lookalike) printf '// %s\n' "$allowed_canary" ;;
  esac > "$fixture_repo/tests/oauth_browser.cjs"
  run_scan 1 --worktree
  fixture_git add tests/oauth_browser.cjs
  run_scan 1 --staged
  fixture_git reset -q HEAD -- tests/oauth_browser.cjs
  fixture_git checkout -- tests/oauth_browser.cjs
done
printf '%s\n' "$allowed_canary" > "$fixture_repo/other-fixture.txt"
run_scan 1 --worktree
rm "$fixture_repo/other-fixture.txt"
# The same blob cached as an allowed fixture cannot exempt another path. This
# filename sorts after tests/, so the allowed copy is scanned and cached first.
printf '%s\n' "$allowed_canary" > "$fixture_repo/zz-other-fixture.txt"
fixture_git add zz-other-fixture.txt
run_scan 1 --staged
fixture_git reset -q HEAD -- zz-other-fixture.txt
rm "$fixture_repo/zz-other-fixture.txt"

# Force-staged ignored state/credential filenames must fail before content
# scanning. Keep their contents harmless, and assert filename-only diagnostics.
for sensitive in '.mcp-bridge-state.json' '.mcp-bridge-state.tmp-42' \
  'nested/oauth-state.json' 'nested/oauth-state.tmp-42' '.env' 'private.key'; do
  mkdir -p "$(dirname "$fixture_repo/$sensitive")"
  printf 'synthetic content only\n' > "$fixture_repo/$sensitive"
  fixture_git add -f -- "$sensitive"
  run_scan 1 --staged
  grep -F 'sensitive filename:' "$fixture_root/report" >/dev/null
  if grep -F 'potential credential signature:' "$fixture_root/report" >/dev/null; then exit 1; fi
  run_scan 1 --worktree
  fixture_git reset -q HEAD -- "$sensitive"
  rm "$fixture_repo/$sensitive"
done

# Untracked crash leftovers stay ignored. Symbolic links are inspected as links,
# never followed into external data (a FIFO would block if dereferenced).
printf '%s\n' "$fake_token" > "$fixture_repo/.mcp-bridge-state.tmp-31415"
fixture_git check-ignore -q .mcp-bridge-state.tmp-31415
mkfifo "$fixture_root/external-fifo"
ln -s "$fixture_root/external-fifo" "$fixture_repo/external-link"
run_scan 0 --worktree
rm "$fixture_repo/external-link" "$fixture_repo/.mcp-bridge-state.tmp-31415"

# Newline filenames must remain one file in worktree, index, and history scans,
# and be escaped in diagnostics instead of injecting extra log lines.
strange_name=$'untracked\nfixture.txt'
printf '%s\n' "$fake_token" > "$fixture_repo/$strange_name"
run_scan 1 --worktree
escaped_name="$(printf '%q' "$strange_name")"
grep -F "file=$escaped_name line=1" "$fixture_root/report" >/dev/null
fixture_git add -- "$strange_name"
run_scan 1 --staged
grep -F "file=$escaped_name line=1" "$fixture_root/report" >/dev/null
fixture_git commit -qm 'Synthetic signature history regression'
run_scan 1 --current
grep -F "file=$escaped_name line=1" "$fixture_root/report" >/dev/null
fixture_git rm -q -- "$strange_name"
fixture_git commit -qm 'Remove synthetic signature'
run_scan 0 --current
run_scan 1

# History filename checks still reject a later-removed state file, without
# reading its content. HEAD remains clean after removal.
printf 'synthetic content only\n' > "$fixture_repo/.mcp-bridge-state.json"
fixture_git add -f .mcp-bridge-state.json
fixture_git commit -qm 'Synthetic state filename regression'
run_scan 1 --current
grep -F 'sensitive filename:' "$fixture_root/report" >/dev/null
fixture_git rm -q .mcp-bridge-state.json
fixture_git commit -qm 'Remove synthetic state file'
run_scan 0 --current
run_scan 1
grep -F 'sensitive filename:' "$fixture_root/report" >/dev/null

printf 'Secret scanner index, worktree, history, and redaction regressions passed.\n'
