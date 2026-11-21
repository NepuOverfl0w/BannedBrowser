#!/usr/bin/env bash
# Fetch the external conformance suites into third_party/ (git-ignored).
#   - html5lib-tests: tokenizer tests (HEAD).
#   - WPT html/syntax/parsing/resources/*.dat: the tree-construction tests, which moved
#     from html5lib-tests to WPT in 2026. Pinned to WPT_COMMIT for reproducible runs; bump
#     it deliberately and fix what the new tests reveal.
#   - WPT url/resources/urltestdata.json
#   - WHATWG entities.json (used to regenerate the named character reference table)
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TP="$ROOT/third_party"
mkdir -p "$TP"
cd "$TP"

if [ ! -d html5lib-tests/.git ]; then
  git clone -q --depth 40 https://github.com/html5lib/html5lib-tests html5lib-tests
fi

WPT_COMMIT="9986bb32c9e97707eff870ab284ebc5cc7ae8d21" # 2026-11-21
if [ "$(git -C wpt rev-parse HEAD 2>/dev/null || true)" != "$WPT_COMMIT" ]; then
  rm -rf wpt
  git init -q wpt
  (cd wpt
   git remote add origin https://github.com/web-platform-tests/wpt
   # No leading "/": Git Bash on Windows would rewrite it into a Windows path. A pattern
   # with a slash in the middle is anchored at the root anyway.
   git sparse-checkout set --no-cone html/syntax/parsing/resources/
   git fetch -q --depth 1 --filter=blob:none origin "$WPT_COMMIT"
   git -c advice.detachedHead=false checkout -q FETCH_HEAD)
fi
[ -d wpt/html/syntax/parsing/resources ] || { echo "WPT parsing tests missing after fetch" >&2; exit 1; }

mkdir -p wpt-url
[ -s wpt-url/urltestdata.json ] || curl -fsSL -o wpt-url/urltestdata.json \
  https://raw.githubusercontent.com/web-platform-tests/wpt/master/url/resources/urltestdata.json
[ -s entities.json ] || curl -fsSL -o entities.json https://html.spec.whatwg.org/entities.json
echo "spec suites ready in $TP"
