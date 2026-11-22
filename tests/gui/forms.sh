#!/usr/bin/env bash
# Forms: typing into a text field, Tab between controls, Space on a checkbox, a select's
# arrow keys and drop-down, GET submission with a button; a textarea and a POST.
set -euo pipefail
source "$(dirname "$0")/lib.sh"

start_browser "$BASE/gui/forms.html"
wait_state '.title == "forms" and .loading == false and "q" in .targets'

click_target q
wait_state '.page_focus == "text"'
type_text "hello world"
# Tab to the checkbox and check it with Space.
key Tab
wait_state '.page_focus == "checkbox"'
key space
# Tab to the select; Down picks "two"; the drop-down then picks "three".
key Tab
wait_state '.page_focus == "select"'
key Down
click_target s
wait_state '.menu != None and .menu["kind"] == "select"'
menu_select "   three"
wait_state '.menu == None'
click_target go
wait_state '.title == "GET q=hello+world&c=yes&s=three" and .loading == false'

# Back to the form; a textarea with two lines, posted.
key alt+Left
wait_state '.title == "forms" and .loading == false and "t" in .targets'
click_target t
wait_state '.page_focus == "textarea"'
type_text "line 1"
key Return
type_text "line 2"
click_target send
wait_state '.title == "POST t=line+1%0D%0Aline+2" and .loading == false'
pass "forms"
