#!/usr/bin/env bash
# Rebuild the two subset fonts in tests/fonts/ (both OFL 1.1 without Reserved Font Names, so
# subsets may keep their names):
#   - NotoSansSC-Regular-subset.otf: Noto Sans SC cut down to the CJK characters used by the
#     test pages, so snapshots render them on every OS without a multi-megabyte font.
#   - NotoSans-Regular-subset.ttf: Noto Sans cut down to U+FFFD REPLACEMENT CHARACTER, which
#     the other bundled fonts lack (theoldnet's proxy turns some characters into it).
# Run it after adding CJK text to a test page.
#   Needs: python3 with fonttools (pip install fonttools), curl.
set -euo pipefail
cd "$(dirname "$0")/.."
work="$(mktemp -d)"; trap 'rm -rf "$work"' EXIT
curl -fsSL -o "$work/NotoSansSC-Regular.otf" \
  https://raw.githubusercontent.com/notofonts/noto-cjk/main/Sans/SubsetOTF/SC/NotoSansSC-Regular.otf
# Every CJK ideograph, CJK punctuation, fullwidth form and Hangul syllable in tests/**.html.
chars="$(python3 - <<'PY'
import html, pathlib
seen = set()
for p in pathlib.Path("tests").rglob("*.htm*"):
    for c in html.unescape(p.read_text(encoding="utf-8", errors="replace")):
        o = ord(c)
        if 0x2E80 <= o <= 0x9FFF or 0xF900 <= o <= 0xFAFF or 0xFF00 <= o <= 0xFFEF or 0xAC00 <= o <= 0xD7AF:
            seen.add(c)
print("".join(sorted(seen)))
PY
)"
pyftsubset "$work/NotoSansSC-Regular.otf" --text="$chars" --name-IDs='*' --name-languages='*' \
  --output-file=tests/fonts/NotoSansSC-Regular-subset.otf
echo "subset with ${#chars} characters written to tests/fonts/NotoSansSC-Regular-subset.otf"
curl -fsSL -o "$work/NotoSans-Regular.ttf" \
  https://raw.githubusercontent.com/notofonts/notofonts.github.io/main/fonts/NotoSans/hinted/ttf/NotoSans-Regular.ttf
pyftsubset "$work/NotoSans-Regular.ttf" --unicodes=U+FFFD --name-IDs='*' --name-languages='*' \
  --output-file=tests/fonts/NotoSans-Regular-subset.ttf
