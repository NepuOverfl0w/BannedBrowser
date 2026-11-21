#!/usr/bin/env bash
# Run all GUI tests under Xvfb + Fluxbox. Starts the local test server.
set -euo pipefail
cd "$(dirname "$0")/.."
export DISPLAY="${DISPLAY:-:99}"
command -v vdisplay-start >/dev/null && vdisplay-start >/dev/null
cargo build -q -p bannedbrowser
rm -f /tmp/bb-server.out
# The server exits when fd 3 (its stdin) closes, i.e. when this script ends.
exec 3> >(python3 tests/server/testserver.py --port 0 --exit-on-stdin-eof > /tmp/bb-server.out 2>/dev/null)
for _ in $(seq 1 50); do grep -q '^PORT' /tmp/bb-server.out 2>/dev/null && break; sleep 0.1; done
export BASE="http://127.0.0.1:$(awk '/^PORT/{print $2}' /tmp/bb-server.out)"
status=0
for t in tests/gui/*.sh; do
  [ "$(basename "$t")" = "lib.sh" ] && continue
  echo "== $t"
  bash "$t" || status=1
done
exit $status
