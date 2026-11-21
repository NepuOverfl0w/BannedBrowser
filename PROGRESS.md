# BannedBrowser — progress log

_Last updated: 2026-11-21 (session 1)._

## Status at a glance

| Milestone | Status | Notes |
|---|---|---|
| M0 Scaffold | ✅ done | workspace, check-deps, CI workflow, docs, headless PNG, window under Xvfb + Fluxbox |
| M1 URL + networking | ✅ done | 904/904 WPT urltestdata; HTTP/1.1 + TLS + proxies + codings + redirects; 16 integration tests |
| M2 HTML | ✅ done | tokenizer 99.60 %, tree construction 99.72 % (html5lib-tests) |
| M3 CSS + style | 🚧 mostly | parser, selectors, cascade, var(), calc(), media, UA sheet, presentational hints; see gaps |
| M4 Layout I | 🚧 mostly | block/inline/floats/tables/flex(basic)/positioned/replaced; see gaps |
| M5 Paint | 🚧 mostly | AA rasterizer, borders, gradients, shadows, clips, opacity, glyph cache; 46 reftests |
| M6 GUI shell core | 🚧 started | tabs, toolbar, URL bar, scrolling, links, multiple + private windows, test-state hook |
| M7–M11 | 📋 planned | |
| **v0.1 "Hello, Old Web"** | 🚧 in progress | example.com ✅, theoldnet.com ✅ (start + lite), see checklist |

## Test numbers (session 1)

| Suite | Result |
|---|---|
| WPT `url/resources/urltestdata.json` | **904 / 904 (100.0 %)** |
| html5lib tokenizer (HEAD `c5495c5`) | **7014 / 7042 (99.60 %)**, 4 skipped (lone surrogates can't be Rust strings) |
| html5lib tree-construction (pinned `224991e~1`) | **1779 / 1784 (99.72 %)**, 8 `#script-on` cases excluded |
| Reftests (`tests/reftests`) | **46 / 46** |
| Snapshot regressions (`tests/pages/snapshots`) | 5 pages, pass on the approved font set |
| `cargo test --workspace` | 105 test functions, 0 failures |
| GUI smoke (`scripts/gui-smoke.sh`) | `basic.sh` passes (open, link click, back, wheel scroll, URL bar, new/close/reopen tab, resize, maximize, new window, private window) |
| `scripts/check-deps.sh` | OK — 162 crates, none banned |
| Cross targets | `cargo clippy --target x86_64-pc-windows-gnu -D warnings` ✅ and Windows release `.exe` builds; `cargo check --target aarch64-apple-darwin` ✅ for every crate except those linking `ring` (see gaps) |

### Failing / excluded spec cases
* **Tokenizer (28):** all are `<?…>` processing-instruction tests added to html5lib-tests on 2026-09-29 (a newer spec change). The pinned tree-construction snapshot still expects the old bogus-comment behaviour, so PIs are not implemented yet.
* **Tree construction (5):** 4 × `<selectedcontent>` mirroring (customizable `<select>`; needs DOM option-insertion steps, not parser work) and `<font><select><option>a</option></font></select>` (adoption agency interaction with select).

## Real sites checked (headless + GUI, 2026-11-21)

| Site | Result |
|---|---|
| https://example.com | ✅ matches Firefox for the static HTML: `light-dark()` background in both schemes, 26em column, 25vh padding, system-ui 16px/1.6. The extra translations, icon and "Learn more" link Firefox shows are inserted by `s.js` (needs M10 JS); a hand-applied post-JS snapshot renders correctly incl. RTL Arabic, CJK, Cyrillic (SVG icon not drawn). |
| http(s)://theoldnet.com/ | ✅ GIFs, `<font>` sizes/colors, `<center>`, bordered/nested tables with cellspacing/cellpadding, form controls, link colors, favicon. Full page (6889 px) in ~0.8 s. |
| http://theoldnet.com/lite | ✅ |
| https://myip.wtf and https://myip.wtf/headers | ✅ (HTTPS through the sandbox's CONNECT proxy; `/headers` shows exactly what we send: `Accept`, `Accept-Encoding: gzip, deflate, br`, `Accept-Language`, keep-alive, `User-Agent: Mozilla/5.0 (X11; Linux x86_64) BannedBrowser/0.1.0-dev`) |
| Archived sites via theoldnet `/get?...` | 🚧 pages load and lay out, but their images come from **web.archive.org, which is blocked by this environment's egress policy** — images show alt text/placeholders. Please allow `web.archive.org` to complete v0.1 criterion 2. |

## v0.1 checklist

- [x] HTTP + HTTPS, redirects, gzip/deflate/br, chunked, charset + legacy encodings (BOM → header → meta prescan → fallback)
- [x] Quirks mode: table font-size quirk, line-height quirk for image-only lines
- [x] Full HTML parser
- [x] Presentational hints: bgcolor/text/link/vlink, font face/size/color, center, align/valign, width/height, border/cellpadding/cellspacing, hspace/vspace, hr attributes, background, nowrap, list types
- [x] CSS for example.com (`max-width` + `margin:auto`, `light-dark()`, `color-scheme`, system font families)
- [x] Floats and clear
- [x] HTML tables (auto + fixed, colspan/rowspan, spacing/padding, borders, % widths, nested)
- [x] Images: GIF (first frame shown; all frames decoded), JPEG, PNG, WebP, BMP, ICO; alt text when broken; sized placeholders
- [x] Image maps (`<map>/<area>` rect/circle/poly) clickable
- [ ] Frames: `<frameset>` currently renders as a list of links (allowed v0.1 fallback, logged as gap); `<iframe>` is a placeholder box
- [x] GUI: window, tab strip, back/forward/reload/home, URL bar, wheel/keyboard scrolling, link hover cursor + status, click to navigate, title in tab + window, favicon, loading indicator, error pages, light/dark
- [ ] GUI: scrollbar (none drawn yet), find bar, menus
- [x] Robustness: unknown tags, broken markup, missing images, `<script>` ignored, `<noscript>` shown
- [x] Local snapshots + regression tests (example.com, theoldnet.com + lite, myip.wtf)
- [ ] 5 archived sites via theoldnet (blocked: web.archive.org)
- [ ] CI green on all three OSes (workflow written; first run pending push)
- [ ] Release artifacts (`scripts/package.sh` written; Windows `.exe` cross-build verified locally)
- [x] README with real screenshots

## Known gaps and bugs (honest list)

* **Layout:** no `position: fixed` relative to the viewport (treated like absolute at scroll 0); `sticky` = relative; CSS Grid treated as block; inline-block baseline uses the first line (CSS says last); block-in-inline approximated (no inline splitting); `border-collapse: collapse` uses zero spacing without conflict resolution (borders double up); table captions sit inside the table border; no `vertical-align` for inline children of different fonts beyond the basics; `<textarea>` content does not wrap; flex lacks baseline alignment and `align-content`.
* **Text:** bidi reordering is per text piece (no full UAX #9 incl. atomics); punctuation at RTL line ends can land on the wrong side; no small-caps; no `@font-face` loading yet (rules are parsed, fonts not fetched); no hinting.
* **Paint:** SVG is not rendered (space reserved, nothing drawn); no `transform` beyond `translate()`; background-origin/clip ignored; GIF animation not played.
* **Style:** no CSS counters, no `@layer` ordering (flattened), `:has()` sibling forms unsupported, nesting rules ignored, `lab()/lch()/oklch()/color-mix()` unsupported.
* **Network:** no cookies or cache yet (M8); no HTTP/2; `HTTP_PROXY` absolute-form only for plain http.
* **GUI:** no scrollbars, no ≡ menu / context menus / find bar / bookmarks; no session restore; private windows share the network client (no cookies exist yet, so nothing leaks); screenshots of overlapping windows in GUI tests can be stale.
* **macOS cross-check:** `ring`'s build script needs the macOS SDK headers to compile its C/asm, which the Linux VM doesn't have; the macOS CI job covers it. Two attempts (clang + generic headers) failed; not retried further.
* **Environment:** `web.archive.org` blocked by egress policy; theoldnet.com serves "Hello World" to non-browser user agents (our UA is fine).

## Next steps (in order)

1. Push, watch the first CI run on Linux/Windows/macOS and fix platform issues.
2. Ask for `web.archive.org` on the allowlist; check 5 archived GeoCities-era sites and add them as snapshots.
3. GUI: overlay scrollbars, ≡ app menu, page/link/tab context menus, find bar (Ctrl+F), history dropdown, session restore.
4. Frames: real `<frameset>`/`<iframe>` rendering with nested pages and `target`.
5. Reftests to ≥150 (M7), real WPT CSS subset; fix `border-collapse`, captions, inline-block baseline.
6. `@font-face` (TTF/OTF first, WOFF/WOFF2 decoders), GIF animation, SVG basics for icons.
7. Then M8 (forms, cookies, cache, profile storage) and on through M11.
