#!/usr/bin/env bash
# Frames: a frameset renders its frames, the wheel scrolls the frame under the
# pointer (not the page), and a targeted link loads into the named frame.
set -euo pipefail
source "$(dirname "$0")/lib.sh"

start_browser "$BASE/frames/index.html"
wait_state '.title == "Frames Demo" and .loading == false and .frames == 3 and .frame_titles == ["banner", "nav", "Welcome"]'
shot frames-01

# Wheel over the main frame: the frame scrolls (a new render arrives), the page doesn't.
before=$(python3 -c "import json;print(json.load(open('$STATE'))['frame_renders'])")
xdotool mousemove --window "$WID" 700 400; sleep 0.2
for _ in 1 2 3; do xdotool click 5; sleep 0.05; done
wait_state ".frame_renders > $before and .scroll_y == 0"
shot frames-02-scrolled

# "Cool Links" (target=main) in the nav frame: nav starts at y=80+70, items ~ +38..+90.
click_at 80 $((80 + 70 + 77))
wait_state '.frame_titles == ["banner", "nav", "Links"] and .loading == false and .history_len == 1'
shot frames-03-links
pass "frames"
