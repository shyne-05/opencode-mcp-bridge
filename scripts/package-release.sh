#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

cargo build --release --locked
install -m 0644 scripts/browser.cjs target/release/browser.cjs
node --check target/release/browser.cjs

env -u NODE_PATH node target/release/browser.cjs version | grep -Fx 'mcp-browser-helper/2' >/dev/null

VERSION="$(awk -F '"' '$1 ~ /^version = / { print $2; exit }' Cargo.toml)"
printf 'Packaged MCP Bridge %s with browser helper protocol %s\n' "$VERSION" 'mcp-browser-helper/2'
