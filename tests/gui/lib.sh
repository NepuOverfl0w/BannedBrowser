# Shared helpers for GUI tests: start the browser, wait on its state file, send real input.
#
# Input goes through a per-OS driver, as a user's would:
#   Linux (X11)  xdotool
#   Windows      tests/gui/drivers/Driver.cs (SendInput), built with the .NET Framework's csc
#   macOS        tests/gui/drivers/macos.swift (CGEvent + Accessibility), built with swiftc
# Coordinates in the tests are logical pixels relative to the window's content area. Keys use
# xdotool names; "accel" means Cmd on macOS and Ctrl elsewhere.
case "$(uname -s)" in
  MINGW* | MSYS* | CYGWIN*) GUI_OS=windows ;;
  Darwin) GUI_OS=macos ;;
  *) GUI_OS=x11 ;;
esac
if [ "$GUI_OS" = windows ]; then PY="$(command -v python)"; else PY="$(command -v python3)"; fi
BIN="${BB_BIN:-target/debug/bannedbrowser}"
OUT="tests/gui/out"
mkdir -p "$OUT"
# A relative path works for both the shell and the (native) browser on every OS.
STATE="${STATE:-$OUT/state.json}"
rm -f "$STATE"
BB_PID=""
BB_OS_PID=""
WID=""
DRIVER=""
MOUSE_HELD=""

# Fallbacks for machines without the cloud-VM helper scripts (e.g. CI runners).
if [ "$GUI_OS" = x11 ] && ! command -v vwait-window >/dev/null 2>&1; then
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
if [ "$GUI_OS" = x11 ] && ! command -v vshot >/dev/null 2>&1; then
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

# Build the Windows / macOS input driver (once; rebuilt when its source changes).
build_driver() {
  case "$GUI_OS" in
    windows)
      DRIVER="$OUT/bb-gui-driver.exe"
      if [ ! -f "$DRIVER" ] || [ tests/gui/drivers/Driver.cs -nt "$DRIVER" ]; then
        local csc=/c/Windows/Microsoft.NET/Framework64/v4.0.30319/csc.exe
        "$csc" -nologo -out:"$(cygpath -w "$DRIVER")" -r:System.Drawing.dll \
          "$(cygpath -w tests/gui/drivers/Driver.cs)" >"$OUT/driver-build.log" 2>&1 \
          || { cat "$OUT/driver-build.log" >&2; fail "could not build the Windows input driver"; }
      fi
      ;;
    macos)
      DRIVER="$OUT/bb-gui-driver"
      if [ ! -f "$DRIVER" ] || [ tests/gui/drivers/macos.swift -nt "$DRIVER" ]; then
        swiftc -O tests/gui/drivers/macos.swift -o "$DRIVER" >"$OUT/driver-build.log" 2>&1 \
          || { cat "$OUT/driver-build.log" >&2; fail "could not build the macOS input driver"; }
      fi
      "$DRIVER" check \
        || fail "this process may not post input events: grant Accessibility to the test runner"
      ;;
  esac
}

drv() { "$DRIVER" "$@" || fail "input driver failed: $*"; }

# The window's content area origin in logical pixels and its scale, from the state file.
# Prints screen coordinates in the driver's unit for content-relative logical (x, y):
# physical pixels on Windows, points on macOS.
screen_xy() {
  "$PY" - "$STATE" "$1" "$2" "$GUI_OS" <<'PY'
import json, sys
s = json.load(open(sys.argv[1]))
x, y = float(sys.argv[2]), float(sys.argv[3])
k = 1.0 if sys.argv[4] == "macos" else float(s.get("scale") or 1)
print(round((s["win_x"] + x) * k), round((s["win_y"] + y) * k))
PY
}

start_browser() {
  [ "$GUI_OS" = x11 ] || build_driver
  BB_TEST_STATE="$STATE" "$BIN" --window-size 1280x800 "$@" >"$OUT/browser.log" 2>&1 &
  BB_PID=$!
  if [ "$GUI_OS" = x11 ]; then
    WID=$(vwait-window BannedBrowser 30) || fail "window did not appear"
    xdotool windowactivate --sync "$WID" 2>/dev/null || true
  else
    BB_OS_PID="$BB_PID"
    if [ "$GUI_OS" = windows ] && [ -r "/proc/$BB_PID/winpid" ]; then BB_OS_PID=$(cat "/proc/$BB_PID/winpid"); fi
    wait_state '.windows >= 1 and .win_x != None'
    activate_window
  fi
  sleep 0.5
}

activate_window() {
  case "$GUI_OS" in
    x11) xdotool windowactivate --sync "$WID" 2>/dev/null || true ;;
    *)
      WAIT_TRIES=1 wait_state '.has_focus == true' && return 0
      drv activate "$BB_OS_PID"
      WAIT_TRIES=50 wait_state '.has_focus == true' || fail "the browser window did not get the focus"
      ;;
  esac
}

# wait_state <python-ish jq-like expression over the state JSON>
wait_state() {
  local expr="$1" t=0 tries="${WAIT_TRIES:-300}"
  while [ $t -lt "$tries" ]; do
    if [ -f "$STATE" ] && "$PY" - "$STATE" "$expr" <<'PY' 2>/dev/null
import json, sys, re
s = json.load(open(sys.argv[1]))
expr = re.sub(r'(?<![\w"])\.([a-z_]+)', r's.get("\1")', sys.argv[2])
expr = re.sub(r'\btrue\b', 'True', re.sub(r'\bfalse\b', 'False', re.sub(r'\bnull\b', 'None', expr)))
sys.exit(0 if eval(expr) else 1)
PY
    then return 0; fi
    sleep 0.1; t=$((t+1))
  done
  [ -n "${WAIT_TRIES:-}" ] && return 1
  fail "timed out waiting for: $expr"
}

# key <combo>: e.g. accel+t, shift+Return, alt+Left, Escape.
key() {
  local k="$1"
  if [ "$GUI_OS" = macos ]; then k="${k//accel+/cmd+}"; else k="${k//accel+/ctrl+}"; fi
  case "$GUI_OS" in
    x11) xdotool key --clearmodifiers "$k" ;;
    *) drv key "$k" ;;
  esac
  sleep 0.2
}

# key_until <key> <condition>: press a key, retrying (up to 3 times) if the
# window manager swallowed it (seen occasionally on CI right after a click).
key_until() {
  local k="$1" cond="$2" i
  for i in 1 2 3; do
    activate_window
    key "$k"
    if WAIT_TRIES=30 wait_state "$cond" 2>/dev/null; then return 0; fi
  done
  wait_state "$cond"
}

type_text() {
  case "$GUI_OS" in
    x11) xdotool type --delay 15 "$1" ;;
    *) drv type "$1" ;;
  esac
  sleep 0.2
}

# mouse_move <x> <y>: content-relative logical pixels (a drag while mouse_down is held).
mouse_move() {
  case "$GUI_OS" in
    x11) xdotool mousemove --window "$WID" "$1" "$2" ;;
    *)
      local xy
      xy=$(screen_xy "$1" "$2") || fail "no window position in the state file"
      # shellcheck disable=SC2086 # two numbers
      drv move $xy ${MOUSE_HELD:+left}
      ;;
  esac
  sleep 0.3
}

mouse_down() {
  case "$GUI_OS" in x11) xdotool mousedown 1 ;; *) drv down left ;; esac
  MOUSE_HELD=1
  sleep 0.2
}

mouse_up() {
  case "$GUI_OS" in x11) xdotool mouseup 1 ;; *) drv up left ;; esac
  MOUSE_HELD=""
  sleep 0.3
}

left_click() { case "$GUI_OS" in x11) xdotool click 1 ;; *) drv click left ;; esac; sleep 0.3; }
right_click() { case "$GUI_OS" in x11) xdotool click 3 ;; *) drv click right ;; esac; sleep 0.3; }
click_at() { mouse_move "$1" "$2"; left_click; }

# wheel_at <x> <y> <notches>: scroll down over a point.
wheel_at() {
  mouse_move "$1" "$2"
  case "$GUI_OS" in
    x11) for _ in $(seq 1 "$3"); do xdotool click 5; sleep 0.05; done ;;
    *) drv wheel "$3" ;;
  esac
  sleep 0.2
}
wheel_down() { wheel_at 640 400 "$1"; }

# Resize / maximize through the window manager (the OS), not the app.
resize_window() {
  case "$GUI_OS" in
    x11) xdotool windowsize "$WID" "$1" "$2" ;;
    *) drv resize "$BB_OS_PID" "$1" "$2" ;;
  esac
  sleep 1
}

maximize_window() {
  case "$GUI_OS" in
    x11) wmctrl -i -r "$WID" -b add,maximized_vert,maximized_horz ;;
    *) drv maximize "$BB_OS_PID" ;;
  esac
  sleep 1
}

shot() {
  case "$GUI_OS" in
    x11) vshot "$OUT/$1.png" BannedBrowser >/dev/null 2>&1 || true ;;
    windows) [ -n "$DRIVER" ] && "$DRIVER" shot "$OUT/$1.png" >/dev/null 2>&1 || true ;;
    macos) screencapture -x "$OUT/$1.png" >/dev/null 2>&1 || true ;;
  esac
}
