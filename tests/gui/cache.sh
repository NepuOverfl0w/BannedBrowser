#!/usr/bin/env bash
# HTTP cache: visiting a fresh page again uses the cache; Reload revalidates (here: refetches,
# there's no validator); Ctrl+Shift+R bypasses the cache; Back uses the cached copy.
set -euo pipefail
source "$(dirname "$0")/lib.sh"

P="$BASE/cache?id=gui&cc=max-age%3D600&html=1"
start_browser "$P"
wait_state '.title == "hits=1" and .loading == false'

# Another page, then the same URL again from the address bar: served from the cache.
key accel+l
type_text "$BASE/gui/start.html"
key Return
wait_state '.title == "GUI Start" and .loading == false'
key accel+l
type_text "$P"
key Return
wait_state '.history_len == 3 and .loading == false and .title == "hits=1"'

# Reload asks the server again.
key F5
wait_state '.title == "hits=2" and .loading == false'

# Hard reload (Ctrl+Shift+R) bypasses the cache.
key accel+shift+r
wait_state '.title == "hits=3" and .loading == false'

# Somewhere else, then Back: the cached copy, no request.
key accel+l
type_text "$BASE/gui/start.html"
key Return
wait_state '.title == "GUI Start" and .loading == false'
key_until alt+Left '.title == "hits=3" and .loading == false'
pass "cache"
