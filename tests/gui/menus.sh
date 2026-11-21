#!/usr/bin/env bash
# ≡ app menu and context menus (page, link, tab, URL bar): open, keyboard and
# mouse navigation, actions, closing by Esc and by clicking outside.
set -euo pipefail
source "$(dirname "$0")/lib.sh"

start_browser "$BASE/gui/start.html"
wait_state '.title == "GUI Start" and .loading == false'

# ≡ app menu → New Tab (keyboard).
click_at 1256 60
wait_state '.menu["kind"] == "app" and "New Tab" in .menu["items"] and "About BannedBrowser" in .menu["items"]'
shot menus-01-app
key Down
key Return
wait_state '.menu == null and .window_tabs == 2'
key accel+w
wait_state '.window_tabs == 1'

# Clicking outside an open menu closes it without activating what's underneath.
click_at 1256 60
wait_state '.menu["kind"] == "app"'
click_at 100 130
wait_state '.menu == null'
sleep 0.5
wait_state '.title == "GUI Start" and .history_len == 1'

# Link context menu → Open Link in New Tab (opens in the background).
mouse_move 100 130
wait_state '.hover_link != null'
right_click
wait_state '.menu["kind"] == "link" and .menu["items"][0] == "Open Link in New Tab"'
shot menus-02-link
key Down
key Return
wait_state '.menu == null and .window_tabs == 2 and .active_tab == 0'

# Tab context menu → Close Other Tabs (5th enabled item, the separator is skipped).
mouse_move 100 20
right_click
wait_state '.menu["kind"] == "tab"'
shot menus-03-tab
for _ in 1 2 3 4 5; do key Down; done
wait_state '.menu["items"][.menu["hover"]] == "Close Other Tabs"'
key Return
wait_state '.menu == null and .window_tabs == 1'

# Page context menu (no link under the pointer) → Esc closes it.
mouse_move 900 500
wait_state '.hover_link == null'
right_click
wait_state '.menu["kind"] == "page" and .menu["items"][0] == "Back"'
shot menus-04-page
key Escape
wait_state '.menu == null'

# URL bar context menu → Select All (Up wraps to the last item).
mouse_move 600 60
right_click
wait_state '.menu["kind"] == "urlbar" and .focused == "urlbar" and "Copy" in .menu["items"]'
# The whole address is selected, so Cut/Copy are enabled: Down from the top skips
# only the separator (Undo -> Cut).
key Down
key Down
wait_state '.menu["items"][.menu["hover"]] == "Cut"'
key Escape
wait_state '.menu == null'
right_click
wait_state '.menu["kind"] == "urlbar"'
shot menus-05-urlbar
key Up
wait_state '.menu["items"][.menu["hover"]] == "Select All"'
key Return
wait_state '.menu == null'
pass "menus"
