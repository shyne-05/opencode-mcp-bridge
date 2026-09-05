#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

if ! git rev-parse --git-dir >/dev/null 2>&1; then
  echo "secret scan requires a Git worktree" >&2
  exit 2
fi

mode="${1:---history}"
case "$mode" in
  --history|--current|--staged|--worktree) ;;
  *) printf 'Usage: scripts/scan-secrets.sh [--current|--staged|--worktree]\n' >&2; exit 2 ;;
esac
[[ "$#" -le 1 ]] || { printf 'Expected at most one scan mode.\n' >&2; exit 2; }

# High-confidence credential signatures. Keep this intentionally conservative:
# false positives weaken the gate and encourage unsafe blanket allowlisting.
secret_regex='(gh[pousr]_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,}|sk-[A-Za-z0-9_-]{20,}|sk-ant-[A-Za-z0-9_-]{20,}|AKIA[0-9A-Z]{16}|AIza[0-9A-Za-z_-]{35}|xox[baprs]-[0-9A-Za-z-]{20,}|glpat-[0-9A-Za-z_-]{20,}|hf_[0-9A-Za-z]{20,}|-----BEGIN (RSA |EC |OPENSSH )?PRIVATE KEY-----|(MCP_OAUTH_PASSWORD|MCP_TOKEN|CLOUDFLARE_API_TOKEN|OPENAI_API_KEY|ANTHROPIC_API_KEY)=[A-Za-z0-9_./+=-]{20,})'

# Exempt this exact synthetic declaration only, not the rest of its file.
# Split the header here so the scanner does not exempt its own source.
allowed_oauth_canary='const PRIVATE_KEY_SECRET = "-----BEGIN '
allowed_oauth_canary+='PRIVATE KEY-----browser-oauth-private-key-canary-7f4c";'

is_sensitive_filename() {
  case "$1" in
    .env.example|*/.env.example) return 1 ;;
    .env|.env.*|*/.env|*/.env.*|*.pem|*.key|*.p12|*.pfx|*credentials*.json|*credential*.json|*oauth-password*|*oauth_password*|.mcp-bridge-state.json|*/.mcp-bridge-state.json|.mcp-bridge-state.tmp-*|*/.mcp-bridge-state.tmp-*|*oauth*state*.json|*oauth*state*.tmp-*)
      return 0 ;;
    *) return 1 ;;
  esac
}

report_filename() {
  printf 'sensitive filename: scope=%s file=%q\n' "$2" "$1" >&2
}

# Only matching lines are held internally; diagnostics never include values.
# Returning failure lets pipefail also reject any failed Git/file read.
scan_content() {
  local path="$1" scope="$2" matches status record line_number line hit=0
  if matches="$(grep -anE "$secret_regex")"; then
    while IFS= read -r record; do
      line_number="${record%%:*}"
      line="${record#*:}"
      line="${line%$'\r'}"
      if [[ "$path" == tests/oauth_browser.cjs && "$line" == "$allowed_oauth_canary" ]]; then
        continue
      fi
      printf 'potential credential signature: scope=%s file=%q line=%s\n' "$scope" "$path" "$line_number" >&2
      hit=1
    done <<< "$matches"
    return "$hit"
  else
    status="$?"
    if [[ "$status" != 1 ]]; then
      printf 'could not scan content: scope=%s file=%q\n' "$scope" "$path" >&2
      return 2
    fi
  fi
  return 0
}

# Temporary listings and clean-blob markers contain no credential content.
umask 077
scan_dir="$(mktemp -d "${TMPDIR:-/tmp}/mcp-bridge-scan.XXXXXX")"
file_list="$scan_dir/files"
trap 'rm -rf "$scan_dir"' EXIT
found=0

scan_blob() {
  local path="$1" object="$2" scope="$3" canary_scope=standard marker
  if is_sensitive_filename "$path"; then
    report_filename "$path" "$scope"
    found=1
    return
  fi
  # Unchanged blobs recur across history. Cache only successful scans, keeping
  # the path-specific canary separate and always checking filenames first.
  [[ "$path" != tests/oauth_browser.cjs ]] || canary_scope=oauth-fixture
  marker="$scan_dir/clean-$object-$canary_scope"
  [[ ! -f "$marker" ]] || return 0
  if git cat-file blob "$object" | scan_content "$path" "$scope"; then
    : > "$marker"
  else
    found=1
  fi
}

scan_revision() {
  local revision="$1" scope record metadata path file_mode object_type object
  scope="commit:$(git rev-parse --short=12 "$revision")"
  git ls-tree -rz --full-tree "$revision" > "$file_list"
  while IFS= read -r -d '' record; do
    metadata="${record%%$'\t'*}"
    path="${record#*$'\t'}"
    read -r file_mode object_type object <<< "$metadata"
    [[ "$object_type" == blob ]] || continue
    scan_blob "$path" "$object" "$scope"
  done < "$file_list"
}

scan_staged() {
  local record metadata path file_mode object stage
  git ls-files --stage -z > "$file_list"
  while IFS= read -r -d '' record; do
    metadata="${record%%$'\t'*}"
    path="${record#*$'\t'}"
    read -r file_mode object stage <<< "$metadata"
    [[ "$file_mode" != 160000 ]] || continue
    scan_blob "$path" "$object" "index:$stage"
  done < "$file_list"
}

scan_worktree() {
  local path
  git ls-files --cached --others --exclude-standard -z > "$file_list"
  while IFS= read -r -d '' path; do
    # Missing tracked paths are deletions. Never follow a link to external data.
    [[ -e "$path" || -L "$path" ]] || continue
    if is_sensitive_filename "$path"; then
      report_filename "$path" worktree
      found=1
    elif [[ -L "$path" ]]; then
      if ! readlink "./$path" | scan_content "$path" worktree; then found=1; fi
    elif [[ -f "$path" ]]; then
      if ! cat "./$path" | scan_content "$path" worktree; then found=1; fi
    fi
  done < "$file_list"
}

case "$mode" in
  --current) scan_revision HEAD ;;
  --staged) scan_staged ;;
  --worktree) scan_worktree ;;
  --history)
    revisions="$(git rev-list HEAD)"
    while IFS= read -r revision; do
      scan_revision "$revision"
    done <<< "$revisions"
    ;;
esac

if (( found != 0 )); then
  cat >&2 <<'ERROR'
Secret scan failed. No secret values were printed.
Remove sensitive files and credentials from the scanned content. Rotate any
real credential already exposed before cleaning published history.
ERROR
  exit 1
fi
printf 'Secret scan passed (%s): no high-confidence credential signatures or sensitive filenames found.\n' "$mode"
