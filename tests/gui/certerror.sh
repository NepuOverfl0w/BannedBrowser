#!/usr/bin/env bash
# Certificate error page: Go Back (home when there's no history), Advanced… (a <details>)
# reveals the reason and Accept the Risk and Continue, which loads the site; a web page can't
# accept a certificate for the user.
set -euo pipefail
source "$(dirname "$0")/lib.sh"

start_browser "$HTTPS_BASE/gui/start.html"
wait_state '.title == "Security risk: 127.0.0.1" and .loading == false and "go-back" in .targets and "accept" not in .targets'
shot certerror-01

# Go Back (Recommended) with no history goes home.
click_target go-back
wait_state '.url == "about:newtab" and .loading == false'

# A web page can't accept the certificate: its forged action link does nothing.
key accel+l
type_text "$BASE/gui/sneaky.html"
key Return
wait_state '.title == "Sneaky" and .loading == false'
click_at 100 130
sleep 1
wait_state '.title == "Sneaky"'

# Back to the site: still blocked. Advanced… reveals the reason and the accept button.
key accel+l
type_text "$HTTPS_BASE/gui/start.html"
key Return
wait_state '.title == "Security risk: 127.0.0.1" and .loading == false and "advanced-toggle" in .targets'
click_target advanced-toggle
wait_state '"accept" in .targets'
shot certerror-02-advanced

# Accept the Risk and Continue: the page loads over HTTPS.
click_target accept
wait_state '.title == "GUI Start" and .loading == false and .url.startswith("https://")'
shot certerror-03-accepted
pass "certerror"
