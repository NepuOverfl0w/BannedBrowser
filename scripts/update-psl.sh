#!/usr/bin/env bash
# Update resources/psl/public_suffix_list.dat from publicsuffix.org (the only supported source).
set -euo pipefail
cd "$(dirname "$0")/.."
tmp="$(mktemp)"; trap 'rm -f "$tmp"' EXIT
curl -fsSL -o "$tmp" https://publicsuffix.org/list/public_suffix_list.dat
grep -q "===BEGIN ICANN DOMAINS===" "$tmp" || { echo "unexpected download" >&2; exit 1; }
mv "$tmp" resources/psl/public_suffix_list.dat
trap - EXIT
grep -m2 -E "^// (VERSION|COMMIT):" resources/psl/public_suffix_list.dat
