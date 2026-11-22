#!/usr/bin/env bash
# About window when the update check fails: it must say so (never "up to date") and link to
# the releases page.
set -euo pipefail
source "$(dirname "$0")/lib.sh"
export BB_UPDATE_URL="$BASE/gui/no-such-release.json"

start_browser "$BASE/gui/start.html"
wait_state '.title == "GUI Start" and .loading == false'
W=$(state_num win_w)
click_at $((W - 24)) 60
wait_state '.menu["kind"] == "app"'
menu_select "About BannedBrowser"
wait_state '.about != None and .about["update"] == "failed" and .about["links"][0] == "update"'
sleep 0.5
shot about-offline
pass "about-offline"
