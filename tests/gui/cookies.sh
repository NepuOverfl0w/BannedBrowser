#!/usr/bin/env bash
# Cookies: a site's cookie comes back on the next visit; a private window has its own jar.
set -euo pipefail
source "$(dirname "$0")/lib.sh"

start_browser "$BASE/set-cookie?c=gui%3D1%3B+Path%3D%2F&to=%2Fcookie-title"
wait_state '.title == "gui=1" and .loading == false'

# A new tab in the same window shares the cookie.
key accel+t
wait_state '.window_tabs == 2 and .focused == "urlbar"'
type_text "$BASE/cookie-title"
key Return
wait_state '.title == "gui=1" and .loading == false'

# A private window doesn't see it.
key accel+shift+p
wait_state '.windows == 2'
WAIT_TRIES=50 wait_state '.private == true' || fail "the private window isn't the focused one"
key accel+l
type_text "$BASE/cookie-title"
key Return
wait_state '.private == true and .title == "none" and .loading == false'
pass "cookies"
