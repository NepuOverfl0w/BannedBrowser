#!/usr/bin/env bash
# Basic GUI flow: open, type URL, navigate, click link, back, scroll, tabs,
# reopen closed tab, new window, private window, resize, maximize.
# Requires: a test server on $BASE and the OS's input driver (see scripts/gui-smoke.sh and
# tests/gui/lib.sh).
set -euo pipefail
source "$(dirname "$0")/lib.sh"

start_browser "$BASE/gui/start.html"
wait_state '.title == "GUI Start" and .loading == false'
shot 01-start

# Click the big link (content area starts 80px below the window top).
click_at 100 130
wait_state '.title == "GUI Page Two" and .history_len == 2'
shot 02-page2

key_until alt+Left '.title == "GUI Start" and .history_index == 0'

# Scroll with the wheel.
wheel_down 6
wait_state '.scroll_y > 100'
key_until Home '.scroll_y == 0'

# Overlay scrollbar (6px from the right edge): track click pages down, thumb drag scrolls
# to the end.
W=$(state_num win_w); H=$(state_num win_h)
click_at $((W - 6)) $((H - 100))
wait_state '.scroll_y > 400 and .scrollbar_visible == true'
key_until Home '.scroll_y == 0'
mouse_move $((W - 6)) 95
mouse_down
wait_state '.scrollbar_dragging == true'
mouse_move $((W - 6)) $((H - 10))
shot 02b-scrollbar-drag
mouse_up
wait_state '.scrollbar_dragging == false and .scroll_y > 2000'
key_until Home '.scroll_y == 0'

# URL bar: focus, type, enter.
key accel+l
type_text "$BASE/gui/page2.html"
key Return
wait_state '.title == "GUI Page Two" and .focused == "content"'

# Tabs.
key accel+t
wait_state '.window_tabs == 2 and .focused == "urlbar"'
key accel+w
wait_state '.window_tabs == 1'
key accel+shift+t
wait_state '.window_tabs == 2'
key accel+w
wait_state '.window_tabs == 1'

# Resize and maximize through the window manager.
resize_window 900 600
wait_state '.window_tabs == 1 and .win_w < 1000 and .maximized == false'
maximize_window
wait_state '.maximized == true'
shot 03-maximized

# New window and private window.
key accel+n
wait_state '.windows == 2'
key accel+shift+p
wait_state '.windows == 3'
sleep 1
shot 04-private
pass "basic"
