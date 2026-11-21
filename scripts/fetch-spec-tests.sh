#!/usr/bin/env bash
# Fetch the external conformance suites into third_party/ (git-ignored).
#   - html5lib-tests: tokenizer tests from HEAD, tree-construction tests from the
#     last commit before they moved to WPT (224991e^).
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
TREE_COMMIT="224991e~1"
if [ ! -d html5lib-tests/tree-construction ]; then
  (cd html5lib-tests && git fetch -q --depth 40 origin master || true
   git archive "$TREE_COMMIT" tree-construction | tar -x)
fi

mkdir -p wpt-url
[ -s wpt-url/urltestdata.json ] || curl -fsSL -o wpt-url/urltestdata.json \
  https://raw.githubusercontent.com/web-platform-tests/wpt/master/url/resources/urltestdata.json
[ -s entities.json ] || curl -fsSL -o entities.json https://html.spec.whatwg.org/entities.json
echo "spec suites ready in $TP"
