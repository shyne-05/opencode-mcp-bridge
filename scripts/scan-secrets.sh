#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

if ! git rev-parse --git-dir >/dev/null 2>&1; then
  echo "secret scan requires a Git worktree" >&2
  exit 2
fi

# High-confidence credential signatures. Keep this intentionally conservative:
# false positives weaken the gate and encourage unsafe blanket allowlisting.
secret_regex='(gh[pousr]_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,}|sk-[A-Za-z0-9_-]{20,}|sk-ant-[A-Za-z0-9_-]{20,}|AKIA[0-9A-Z]{16}|AIza[0-9A-Za-z_-]{35}|xox[baprs]-[0-9A-Za-z-]{20,}|glpat-[0-9A-Za-z_-]{20,}|hf_[0-9A-Za-z]{20,}|-----BEGIN (RSA |EC |OPENSSH )?PRIVATE KEY-----|(MCP_OAUTH_PASSWORD|MCP_TOKEN|CLOUDFLARE_API_TOKEN|OPENAI_API_KEY|ANTHROPIC_API_KEY)=[A-Za-z0-9_./+=-]{20,})'

# This regression fixture deliberately contains fake secret-shaped canaries to
# prove they never reach logs. It contains no usable credential.
is_allowed_fixture() {
  [[ "$1" == "tests/oauth_browser.cjs" ]]
}

is_sensitive_filename() {
  local path="$1"
  case "$path" in
    .env.example|*/.env.example)
      return 1
      ;;
    .env|.env.*|*/.env|*/.env.*|*.pem|*.key|*.p12|*.pfx|*credentials*.json|*credential*.json|*oauth-password*|*oauth_password*)
      return 0
      ;;
    *)
      return 1
      ;;
  esac
}

found=0
scan_revision() {
  local revision="$1"
  local short_revision
  short_revision="$(git rev-parse --short=12 "$revision")"

  local path
  while IFS= read -r path; do
    [[ -n "$path" ]] || continue
    # `git grep <revision>` prefixes matching filenames with `<revision>:`.
    # Strip only the exact revision prefix so allowlists and reports operate on
    # repository-relative paths without weakening matching for unusual names.
    path="${path#${revision}:}"
    if is_allowed_fixture "$path"; then
      continue
    fi
    printf 'potential credential signature: commit=%s file=%s\n' "$short_revision" "$path" >&2
    found=1
  done < <(git grep -IlE "$secret_regex" "$revision" -- 2>/dev/null || true)

  while IFS= read -r path; do
    [[ -n "$path" ]] || continue
    if is_sensitive_filename "$path"; then
      printf 'sensitive filename tracked in history: commit=%s file=%s\n' "$short_revision" "$path" >&2
      found=1
    fi
  done < <(git ls-tree -r --name-only "$revision")
}

if [[ "${1:-}" == "--current" ]]; then
  scan_revision HEAD
else
  # Scan every commit reachable from the checked-out main history. CI uses a
  # full checkout so credentials removed in a later commit are still detected.
  while IFS= read -r revision; do
    scan_revision "$revision"
  done < <(git rev-list HEAD)
fi

if (( found != 0 )); then
  cat >&2 <<'EOF'
Secret scan failed. No secret values were printed.
Rotate any real exposed credential before rewriting history, then remove it
from every reachable commit and run this scan again.
EOF
  exit 1
fi

echo "Secret scan passed: no high-confidence credential signatures or tracked credential files found."
