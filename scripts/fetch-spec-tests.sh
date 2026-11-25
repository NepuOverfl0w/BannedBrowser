#!/usr/bin/env bash
# Fetch the external conformance suites into third_party/ (git-ignored).
#   - html5lib-tests: tokenizer tests (HEAD).
#   - WPT html/syntax/parsing/resources/*.dat: the tree-construction tests, which moved
#     from html5lib-tests to WPT in 2026; WPT resources/ (testharness.js) and dom/ for
#     bb-webapi's WPT runner. Pinned to WPT_COMMIT for reproducible runs; bump it
#     deliberately and fix what the new tests reveal.
#   - WPT url/resources/urltestdata.json
#   - WHATWG entities.json (used to regenerate the named character reference table)
#   - test262 (test/language, test/built-ins, test/annexB, harness), pinned to TEST262_COMMIT:
#     the v0.2 JavaScript scope (intl402 and staging are out of scope).
#   - The Public Suffix List's own test vectors (tests/test_psl.txt, HEAD). The list itself
#     is downloaded by bb-net's build script, so both are current.
#   - Unicode 17 NormalizationTest.txt (UAX #15 conformance for String.prototype.normalize).
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TP="$ROOT/third_party"
mkdir -p "$TP"
cd "$TP"

if [ ! -d html5lib-tests/.git ]; then
  git clone -q --depth 40 https://github.com/html5lib/html5lib-tests html5lib-tests
fi

WPT_COMMIT="9986bb32c9e97707eff870ab284ebc5cc7ae8d21" # 2026-11-21
WPT_PATHS="html/syntax/parsing/resources/ resources/ dom/ url/ encoding/ fetch/api/ common/ domparsing/ FileAPI/ xhr/formdata/ workers/ html/webappapis/structured-clone/ webmessaging/ html/canvas/element/ html/canvas/resources/ images/ css/geometry/ interfaces/ custom-elements/"
if [ "$(git -C wpt rev-parse HEAD 2>/dev/null || true)" != "$WPT_COMMIT" ]; then
  rm -rf wpt
  git init -q wpt
  (cd wpt
   git remote add origin https://github.com/web-platform-tests/wpt
   # No leading "/": Git Bash on Windows would rewrite it into a Windows path. A pattern
   # with a slash in the middle is anchored at the root anyway.
   git sparse-checkout set --no-cone $WPT_PATHS
   git fetch -q --depth 1 --filter=blob:none origin "$WPT_COMMIT"
   git -c advice.detachedHead=false checkout -q FETCH_HEAD)
fi
# A checkout from before the DOM tests were added: widen it.
[ -f wpt/dom/nodes/Node-appendChild.html ] && [ -f wpt/url/url-constructor.any.js ] \
  && [ -f wpt/domparsing/DOMParser-parseFromString-xml.html ] || (cd wpt && git sparse-checkout set --no-cone $WPT_PATHS)
[ -f wpt/resources/testharness.js ] || { echo "WPT resources missing after fetch" >&2; exit 1; }
[ -d wpt/html/syntax/parsing/resources ] || { echo "WPT parsing tests missing after fetch" >&2; exit 1; }

mkdir -p wpt-url
[ -s wpt-url/urltestdata.json ] || curl -fsSL -o wpt-url/urltestdata.json \
  https://raw.githubusercontent.com/web-platform-tests/wpt/master/url/resources/urltestdata.json
[ -s wpt-url/setters_tests.json ] || curl -fsSL -o wpt-url/setters_tests.json \
  https://raw.githubusercontent.com/web-platform-tests/wpt/master/url/resources/setters_tests.json
[ -s entities.json ] || curl -fsSL -o entities.json https://html.spec.whatwg.org/entities.json
TEST262_COMMIT="ebf6ae620a0f292b4845cd4099cb9096735e64ae" # 2026-11-23
if [ "$(git -C test262 rev-parse HEAD 2>/dev/null || true)" != "$TEST262_COMMIT" ]; then
  rm -rf test262
  git init -q test262
  (cd test262
   # Byte-exact files on every OS: some tests check line terminators in source text
   # (Function.prototype.toString), which Git for Windows would turn into CRLF.
   git config core.autocrlf false
   git remote add origin https://github.com/tc39/test262
   git sparse-checkout set --no-cone test/language/ test/built-ins/ test/annexB/ harness/
   git fetch -q --depth 1 --filter=blob:none origin "$TEST262_COMMIT"
   git -c advice.detachedHead=false checkout -q FETCH_HEAD)
fi
[ -d test262/test/language ] || { echo "test262 missing after fetch" >&2; exit 1; }

mkdir -p psl
[ -s psl/test_psl.txt ] || curl -fsSL -o psl/test_psl.txt \
  https://raw.githubusercontent.com/publicsuffix/list/main/tests/test_psl.txt

# Unicode normalization conformance (UAX #15), for String.prototype.normalize.
mkdir -p ucd
[ -s ucd/NormalizationTest.txt ] || curl -fsSL -o ucd/NormalizationTest.txt \
  https://www.unicode.org/Public/17.0.0/ucd/NormalizationTest.txt
echo "spec suites ready in $TP"
