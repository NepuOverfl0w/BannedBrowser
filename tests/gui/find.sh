#!/usr/bin/env bash
# Find in page: Ctrl+F opens the find bar, typing searches incrementally,
# Enter/F3 step through matches (scrolling them into view), Esc closes.
set -euo pipefail
source "$(dirname "$0")/lib.sh"

start_browser "$BASE/frames/welcome.html"
wait_state '.title == "Welcome" and .loading == false'

key_until accel+f '.find_open == true'
type_text "paragraph"
wait_state '.find_total == 40 and .find_active == 0'
shot find-01

# "Paragraph 3" etc: jump to a far match with repeated Enter.
for _ in $(seq 1 30); do key Return; done
wait_state '.find_active == 30 and .scroll_y > 300'
shot find-02-scrolled
key shift+Return
wait_state '.find_active == 29'
key F3
wait_state '.find_active == 30'

# No match.
key accel+a
type_text "zebra"
wait_state '.find_total == 0'
shot find-03-notfound

key_until Escape '.find_open == false'
pass "find"
