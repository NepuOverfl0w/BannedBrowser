#!/usr/bin/env bash
# Check the archived sites in tests/pages/archived-sites.txt. Needs a network
# that reaches theoldnet.com and web.archive.org (GitHub runners do; the
# Claude Code sandbox blocks web.archive.org). For each site:
#   1. live render with BannedBrowser's real UA (theoldnet redirects modern
#      browsers to web.archive.org)       -> out/archived/live/<name>.png
#   2. offline snapshot with images        -> tests/pages/snapshots/archived-<name>/
#   3. render of the offline snapshot      -> out/archived/snapshot/<name>.png
set -uo pipefail
cd "$(dirname "$0")/.."
BIN="${BB_BIN:-target/release/bannedbrowser}"
OUT=out/archived
mkdir -p "$OUT/live" "$OUT/snapshot"
while read -r name url; do
  [[ -z "${name:-}" || "$name" == \#* ]] && continue
  echo "== $name"
  timeout 240 "$BIN" --screenshot "$OUT/live/$name.png" --window-size 1024x768 "$url" \
    > "$OUT/live/$name.log" 2>&1 || echo "   live render failed (see $OUT/live/$name.log)"
  tail -1 "$OUT/live/$name.log"
  dir="tests/pages/snapshots/archived-$name"
  for attempt in 1 2 3; do
    rm -rf "$dir"
    if timeout 400 python3 scripts/snapshot-site.py "$url" "archived-$name" > "$OUT/snapshot/$name.log" 2>&1 \
       && ! grep -q "Please wait" "$dir/index.html"; then
      break
    fi
    echo "   snapshot attempt $attempt failed; retrying"
    rm -rf "$dir"
    sleep 30
  done
  if [ -f "$dir/index.html" ]; then
    tail -1 "$OUT/snapshot/$name.log"
    timeout 120 "$BIN" --screenshot "$OUT/snapshot/$name.png" --window-size 1024x768 "$dir/index.html" \
      >> "$OUT/snapshot/$name.log" 2>&1 || echo "   snapshot render failed"
  fi
  sleep 5
done < tests/pages/archived-sites.txt
exit 0
