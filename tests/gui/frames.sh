#!/usr/bin/env bash
# Frames: a frameset renders its frames, the wheel scrolls the frame under the
# pointer (not the page), a targeted link loads into the named frame, and Back/Forward
# step through frame navigations.
set -euo pipefail
source "$(dirname "$0")/lib.sh"

start_browser "$BASE/frames/index.html"
wait_state '.title == "Frames Demo" and .loading == false and .frames == 3 and .frame_titles == ["banner", "nav", "Welcome"]'
shot frames-01

# Wheel over the main frame: the frame scrolls (a new render arrives), the page doesn't.
before=$("$PY" -c "import json;print(json.load(open('$STATE'))['frame_renders'])")
wheel_at 700 400 3
wait_state ".frame_renders > $before and .scroll_y == 0"
shot frames-02-scrolled

# "Cool Links" (target=main) in the nav frame: nav starts at y=80+70, items ~ +38..+90.
click_at 80 $((80 + 70 + 77))
wait_state '.frame_titles == ["banner", "nav", "Links"] and .loading == false and .history_len == 2 and .history_index == 1'
shot frames-03-links

# The frame navigation is a history entry: Back restores the frame (the page stays the
# frameset), Forward returns to it.
key_until alt+Left '.frame_titles == ["banner", "nav", "Welcome"] and .history_index == 0 and .loading == false'
wait_state '.title == "Frames Demo" and .frames == 3'
shot frames-04-back
key_until alt+Right '.frame_titles == ["banner", "nav", "Links"] and .history_index == 1 and .loading == false'
pass "frames"
