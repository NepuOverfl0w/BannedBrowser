#!/usr/bin/env bash
# Profile: persistent cookies and the window size survive a restart; session cookies don't.
set -euo pipefail
source "$(dirname "$0")/lib.sh"

start_browser "$BASE/set-cookie?c=keep%3D1%3B+Max-Age%3D3600%3B+Path%3D%2F&c=gone%3D1%3B+Path%3D%2F&to=%2Fcookie-title"
wait_state '.title == "keep=1; gone=1" and .loading == false and .profile == "created"'
resize_window 900 620
wait_state '.win_w < 1000 and .maximized == false'
W=$(state_num win_w)
H=$(state_num win_h)
quit_browser

# Restart on the same profile, without --window-size.
WINDOW_SIZE='' start_browser "$BASE/cookie-title"
wait_state ".title == \"keep=1\" and .loading == false and .profile == \"opened\" and abs(.win_w - $W) <= 1 and abs(.win_h - $H) <= 1"
pass "profile"
