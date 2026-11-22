#!/usr/bin/env bash
# About window: opened from the ≡ menu, shows the update check's result (here a newer release
# from a local fixture standing in for the GitHub Releases API), Tab moves between links,
# Enter opens one in a browser tab, Esc closes the window.
set -euo pipefail
source "$(dirname "$0")/lib.sh"
export BB_UPDATE_URL="$BASE/gui/releases-latest.json"

start_browser "$BASE/gui/start.html"
wait_state '.title == "GUI Start" and .loading == false and .about == None'

# ≡ menu → About BannedBrowser.
W=$(state_num win_w)
click_at $((W - 24)) 60
wait_state '.menu["kind"] == "app"'
menu_select "About BannedBrowser"
wait_state '.about != None and .about["update"] == "newer:9.9.9" and .about["links"][0] == "update"'
WAIT_TRIES=50 wait_state '.about["has_focus"] == true' || fail "the About window didn't get the focus"
sleep 0.5
shot about-01

# Tab: update link, What's new, Credits; Enter opens Credits (about:bannedbrowser) in a tab.
key Tab
wait_state '.about["focus"] == "update"'
key Tab
key Tab
wait_state '.about["focus"] == "credits"'
key Return
wait_state '.window_tabs == 2 and .url == "about:bannedbrowser"'

# Back to the About window (the menu item brings it to the front); Esc closes it.
W=$(state_num win_w)
click_at $((W - 24)) 60
wait_state '.menu["kind"] == "app"'
menu_select "About BannedBrowser"
WAIT_TRIES=50 wait_state '.about["has_focus"] == true' || fail "the About window didn't come back to the front"
key Escape
wait_state '.about == None'
pass "about"
