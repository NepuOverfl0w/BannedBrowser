#!/usr/bin/env bash
# Run all GUI tests (tests/gui/*.sh) with real OS input; starts the local test server.
#   Linux:   under X11 (Xvfb + a window manager such as Fluxbox), driven with xdotool.
#   Windows: in Git Bash on an interactive desktop, driven with SendInput.
#   macOS:   in a logged-in session, driven with CGEvent (the runner needs Accessibility).
# See tests/gui/lib.sh.
set -euo pipefail
cd "$(dirname "$0")/.."
case "$(uname -s)" in
  MINGW* | MSYS* | CYGWIN*) PY=python ;;
  Darwin) PY=python3 ;;
  *)
    PY=python3
    export DISPLAY="${DISPLAY:-:99}"
    command -v vdisplay-start >/dev/null && vdisplay-start >/dev/null
    ;;
esac
cargo build -q -p bannedbrowser
mkdir -p tests/gui/out
SERVER_OUT=tests/gui/out/server.out
rm -f "$SERVER_OUT"
# The server exits when fd 3 (its stdin) closes, i.e. when this script ends.
exec 3> >("$PY" tests/server/testserver.py --port 0 --https-port 0 --exit-on-stdin-eof > "$SERVER_OUT" 2>tests/gui/out/server.err)
# Python can take a while to start on a cold CI runner: allow up to 60 s.
for _ in $(seq 1 600); do grep -q '^HTTPS_PORT' "$SERVER_OUT" 2>/dev/null && break; sleep 0.1; done
grep -q '^HTTPS_PORT' "$SERVER_OUT" || {
  echo "test server did not start; its stderr:" >&2
  cat tests/gui/out/server.err >&2 || true
  exit 1
}
export BASE="http://127.0.0.1:$(awk '/^PORT/{print $2}' "$SERVER_OUT" | tr -d '\r')"
# HTTPS with a certificate from the test-only CA, which the browser doesn't trust.
export HTTPS_BASE="https://127.0.0.1:$(awk '/^HTTPS_PORT/{print $2}' "$SERVER_OUT" | tr -d '\r')"
status=0
for t in tests/gui/*.sh; do
  [ "$(basename "$t")" = "lib.sh" ] && continue
  echo "== $t"
  bash "$t" || status=1
done
exit $status
