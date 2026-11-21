#!/usr/bin/env bash
# Basic GUI flow: open, type URL, navigate, click link, back, scroll, tabs,
# reopen closed tab, new window, private window, resize, maximize.
# Requires: DISPLAY with a window manager, xdotool, wmctrl, python3, a test
# server on $BASE (see scripts/gui-smoke.sh).
set -euo pipefail
source "$(dirname "$0")/lib.sh"

start_browser "$BASE/gui/start.html"
wait_state '.title == "GUI Start" and .loading == false'
shot 01-start

# Click the big link (content area starts 80px below the window top).
click_at 100 130
wait_state '.title == "GUI Page Two" and .history_len == 2'
shot 02-page2

key alt+Left
wait_state '.title == "GUI Start" and .history_index == 0'

# Scroll with the wheel.
wheel_down 6
wait_state '.scroll_y > 100'
key Home
wait_state '.scroll_y == 0'

# URL bar: focus, type, enter.
key ctrl+l
type_text "$BASE/gui/page2.html"
key Return
wait_state '.title == "GUI Page Two" and .focused == "content"'

# Tabs.
key ctrl+t
wait_state '.window_tabs == 2 and .focused == "urlbar"'
key ctrl+w
wait_state '.window_tabs == 1'
key ctrl+shift+t
wait_state '.window_tabs == 2'
key ctrl+w
wait_state '.window_tabs == 1'

# Resize and maximize through the window manager.
xdotool windowsize "$WID" 900 600
sleep 1
wait_state '.window_tabs == 1'
wmctrl -i -r "$WID" -b add,maximized_vert,maximized_horz
sleep 1
shot 03-maximized

# New window and private window.
key ctrl+n
wait_state '.windows == 2'
key ctrl+shift+p
wait_state '.windows == 3'
sleep 1
shot 04-private
pass "basic"
