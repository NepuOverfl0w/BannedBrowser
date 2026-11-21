# Shared helpers for GUI tests.
BIN="${BB_BIN:-target/debug/bannedbrowser}"
STATE="${STATE:-/tmp/bb-gui-state.json}"
OUT="tests/gui/out"
mkdir -p "$OUT"
rm -f "$STATE"
BB_PID=""
WID=""

# Fallbacks for machines without the cloud-VM helper scripts (e.g. CI runners).
if ! command -v vwait-window >/dev/null 2>&1; then
  vwait-window() {
    local t=0
    while [ $t -lt $(( ${2:-20} * 10 )) ]; do
      local w; w=$(xdotool search --name "$1" 2>/dev/null | head -1)
      [ -n "$w" ] && { echo "$w"; return 0; }
      sleep 0.1; t=$((t+1))
    done
    return 1
  }
fi
if ! command -v vshot >/dev/null 2>&1; then
  vshot() { local w; w=$(xdotool search --name "$2" 2>/dev/null | head -1); [ -n "$w" ] && import -window "$w" "$1"; }
fi

cleanup() { [ -n "$BB_PID" ] && kill "$BB_PID" 2>/dev/null || true; }
trap cleanup EXIT

fail() {
  echo "FAIL: $*" >&2
  [ -f "$STATE" ] && cat "$STATE" >&2
  echo "--- browser log (last 40 lines) ---" >&2
  tail -40 "$OUT/browser.log" >&2 2>/dev/null || true
  shot failure || true
  exit 1
}
pass() { echo "PASS: $1"; }

start_browser() {
  BB_TEST_STATE="$STATE" "$BIN" --window-size 1280x800 "$@" >"$OUT/browser.log" 2>&1 &
  BB_PID=$!
  WID=$(vwait-window BannedBrowser 30) || fail "window did not appear"
  xdotool windowactivate --sync "$WID" 2>/dev/null || true
  sleep 0.5
}

# wait_state <python-ish jq-like expression over the state JSON>
wait_state() {
  local expr="$1" t=0
  while [ $t -lt 300 ]; do
    if [ -f "$STATE" ] && python3 - "$STATE" "$expr" <<'PY' 2>/dev/null
import json, sys, re
s = json.load(open(sys.argv[1]))
expr = re.sub(r'(?<![\w"])\.([a-z_]+)', r's.get("\1")', sys.argv[2])
expr = re.sub(r'\btrue\b', 'True', re.sub(r'\bfalse\b', 'False', re.sub(r'\bnull\b', 'None', expr)))
sys.exit(0 if eval(expr) else 1)
PY
    then return 0; fi
    sleep 0.1; t=$((t+1))
  done
  fail "timed out waiting for: $expr"
}

key() { xdotool key --clearmodifiers "$1"; sleep 0.2; }
type_text() { xdotool type --delay 15 "$1"; sleep 0.2; }
click_at() { xdotool mousemove --window "$WID" "$1" "$2"; sleep 0.3; xdotool click 1; sleep 0.3; }
wheel_down() { xdotool mousemove --window "$WID" 640 400; for _ in $(seq 1 "$1"); do xdotool click 5; sleep 0.05; done; }
shot() { vshot "$OUT/$1.png" BannedBrowser >/dev/null 2>&1 || true; }
