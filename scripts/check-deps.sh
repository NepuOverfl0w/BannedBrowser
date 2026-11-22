#!/usr/bin/env bash
# Enforce BannedBrowser's "no borrowed engines" rule and the unsafe policy
# (see AGENTS.md, hard rules 1-3).
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

# The unsafe policy (AGENTS.md, hard rule 3): `unsafe` is allowed in every crate, but every
# `unsafe` block needs a `// SAFETY:` comment (clippy `undocumented_unsafe_blocks`, denied in the
# workspace lints). So every crate must inherit the workspace lints.

# unsafe_policy <repo-root>: prints violations, returns 1 if any.
unsafe_policy() {
  local root="$1" bad=0 toml dir
  for toml in "$root"/crates/*/Cargo.toml "$root"/xtask/Cargo.toml; do
    [[ -f "$toml" ]] || continue
    dir="${toml%/Cargo.toml}"; dir="${dir#"$root"/}"
    if ! awk '/^\[lints\]/{l=1; next} /^\[/{l=0} l && /workspace *= *true/{ok=1} END{exit !ok}' "$toml"; then
      echo "UNSAFE POLICY: $dir/Cargo.toml must inherit '[lints] workspace = true'"; bad=1
    fi
  done
  # Nobody may switch the SAFETY-comment lint off.
  while IFS= read -r hit; do
    [[ -z "$hit" ]] && continue
    echo "UNSAFE POLICY: the SAFETY-comment lint is switched off: ${hit#"$root"/}"; bad=1
  done < <(grep -rn --include='*.rs' -E 'allow\([^)]*undocumented_unsafe_blocks' "$root/crates" "$root/xtask" 2>/dev/null)
  return $bad
}

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
  # Unsafe policy on throwaway trees.
  t="$(mktemp -d)"; trap 'rm -rf "$t"' EXIT
  mkdir -p "$t/crates/ok/src" "$t/xtask"
  printf '[package]\nname="ok"\n[lints]\nworkspace = true\n' > "$t/crates/ok/Cargo.toml"
  printf '[package]\nname="x"\n[lints]\nworkspace = true\n' > "$t/xtask/Cargo.toml"
  printf '// SAFETY: documented.\nlet v = unsafe { f() };\n' > "$t/crates/ok/src/lib.rs"
  unsafe_policy "$t" >/dev/null || { echo "self-test: clean tree flagged"; unsafe_policy "$t"; fail=1; }
  printf '#[allow(clippy::undocumented_unsafe_blocks)] fn x() {}\n' > "$t/crates/ok/src/lib.rs"
  unsafe_policy "$t" >/dev/null && { echo "self-test: switching the SAFETY lint off not flagged"; fail=1; }
  rm "$t/crates/ok/src/lib.rs"
  printf '[package]\nname="ok"\n' > "$t/crates/ok/Cargo.toml"
  unsafe_policy "$t" >/dev/null && { echo "self-test: crate without workspace lints not flagged"; fail=1; }
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

unsafe_policy "$PWD" || status=1

if [[ $status == 0 ]]; then
  echo "check-deps: OK ($(echo "$TREE" | awk '{print $1}' | sort -u | wc -l) crates, none banned)"
else
  echo "check-deps: FAILED — see the banned crates / unsafe policy violations above (AGENTS.md, hard rules 1-3)" >&2
fi
exit $status
