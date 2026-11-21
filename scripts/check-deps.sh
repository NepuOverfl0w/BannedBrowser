#!/usr/bin/env bash
# Enforce BannedBrowser's "no borrowed engines" rule (see CLAUDE.md §1.1).
#
# Runs `cargo tree -e normal,build --prefix none` for the whole workspace and
# fails if any banned crate appears. Plain names match exactly (plus `-sys` /
# `_sys` variants); names ending in `*` match as prefixes.
#
# Usage: scripts/check-deps.sh [--self-test]
set -euo pipefail
cd "$(dirname "$0")/.."

BANNED=(
  html5ever markup5ever markup5ever_rcdom xml5ever scraper kuchiki kuchikiki select
  lol_html tl html_parser htmlstream
  cssparser selectors lightningcss stylo style 'servo*' simplecss
  taffy stretch morphorm cosmic-text parley swash
  webrender vello femtovg tiny-skia resvg usvg 'blitz*'
  wry tao 'webview*' 'webkit2gtk*' 'cef*' 'ultralight*' 'mozjs*' 'gecko*'
  'boa*' rquickjs 'quickjs*' v8 rusty_v8 'deno_core*' js-sys
  url reqwest hyper ureq isahc curl attohttpc cookie cookie_store http
  'egui*' eframe 'iced*' 'slint*' 'gtk*' 'gtk4*' 'relm*' 'druid*' 'xilem*' 'fltk*' 'qt*' 'tauri*'
)

is_banned() {
  local crate="$1" pat base
  for pat in "${BANNED[@]}"; do
    if [[ "$pat" == *'*' ]]; then
      base="${pat%\*}"
      [[ "$crate" == "$base"* ]] && { echo "$pat"; return 0; }
    else
      if [[ "$crate" == "$pat" || "$crate" == "$pat-sys" || "$crate" == "${pat}_sys" ]]; then
        echo "$pat"; return 0
      fi
    fi
  done
  return 1
}

if [[ "${1:-}" == "--self-test" ]]; then
  fail=0
  for c in url hyper servo-media gtk-sys tiny-skia js-sys qtcore cef-dll-sys; do
    is_banned "$c" >/dev/null || { echo "self-test: expected '$c' to be banned"; fail=1; }
  done
  for c in urlencoding httparse winit softbuffer rustls fontdb rustybuzz styled stylish; do
    if is_banned "$c" >/dev/null; then echo "self-test: '$c' wrongly banned"; fail=1; fi
  done
  [[ $fail == 0 ]] && echo "check-deps self-test OK"
  exit $fail
fi

TREE="$(cargo tree --workspace -e normal,build --prefix none 2>/dev/null)" || {
  echo "check-deps: cargo tree failed" >&2; cargo tree --workspace -e normal,build --prefix none >/dev/null; exit 2; }

status=0
while read -r crate _rest; do
  [[ -z "$crate" ]] && continue
  if pat="$(is_banned "$crate")"; then
    echo "BANNED dependency: $crate (matches '$pat')"
    status=1
  fi
done < <(echo "$TREE" | awk '{print $1}' | sort -u)

if [[ $status == 0 ]]; then
  echo "check-deps: OK ($(echo "$TREE" | awk '{print $1}' | sort -u | wc -l) crates, none banned)"
else
  echo "check-deps: FAILED — remove the banned crates above (see CLAUDE.md §1.1)" >&2
fi
exit $status
