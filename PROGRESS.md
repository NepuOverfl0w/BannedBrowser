# BannedBrowser — progress log

_Last updated: 2026-11-21 (session 1)._

## Status at a glance

| Milestone | Status | Notes |
|---|---|---|
| M0 Scaffold | ✅ done | workspace, check-deps, CI workflow, docs, headless PNG, window under Xvfb + Fluxbox |
| M1 URL + networking | ✅ done | 904/904 WPT urltestdata; HTTP/1.1 + TLS + proxies + codings + redirects; 16 integration tests |
| M2 HTML | ✅ done | tokenizer 100 % (html5lib-tests), tree construction 100 % (WPT); processing instructions; lone surrogates in the tokenizer; `<selectedcontent>` |
| M3 CSS + style | 🚧 mostly | parser, selectors, cascade, var(), calc(), media, UA sheet, presentational hints; see gaps |
| M4 Layout I | 🚧 mostly | block/inline/floats/tables/flex(basic)/positioned/replaced; see gaps |
| M5 Paint | 🚧 mostly | AA rasterizer, borders, gradients, shadows, clips, opacity, glyph cache; 48 reftests |
| M6 GUI shell core | 🚧 started | tabs, toolbar, URL bar, overlay scrollbar, find bar, frames, links, multiple + private windows, test-state hook; menus 📋 |
| M7–M11 | 📋 planned | |
| **v0.1 "Hello, Old Web"** | ✅ released 2026-11-21 ([v0.1.0](https://github.com/NepuOverfl0w/BannedBrowser/releases/tag/v0.1.0)) | example.com, theoldnet.com, 5 archived sites, snapshots, CI green on 3 OSes, packages; see checklist |

## Test numbers (session 1)

| Suite | Result |
|---|---|
| WPT `url/resources/urltestdata.json` | **904 / 904 (100.0 %)** |
| html5lib tokenizer (HEAD `c5495c5`) | **7018 / 7046 (99.60 %)**, nothing skipped (lone-surrogate cases run since session 2) |
| Tree construction, WPT `html/syntax/parsing/resources` (pinned `9986bb3`, 2026-11-21) | **1959 / 1959 (100 %)**; 14 `#script-on` cases not run yet (need JS, milestone E) |
| Reftests (`tests/reftests`) | **48 / 48** |
| Snapshot regressions (`tests/pages/snapshots`) | 12 pages (example.com ×2, theoldnet ×2, myip.wtf, 5 archived sites, retro, frames), rendered with the bundled `tests/fonts/` only, so they run on every OS |
| `cargo test --workspace` | 119 test functions, 0 failures |
| GUI tests (`scripts/gui-smoke.sh`), Linux + Windows + macOS with real OS input | `basic.sh` (open, link click, back, wheel scroll, scrollbar track click + thumb drag, URL bar, new/close/reopen tab, resize, maximize, new window, private window), `frames.sh` (frameset render, wheel scrolls the frame under the pointer, targeted link loads into the named frame), `find.sh` (incremental find, stepping, scroll to match, not found, close) and `menus.sh` (≡ menu, link/tab/page/URL-bar context menus, keyboard and mouse, Esc and click-outside) pass |
| `scripts/check-deps.sh` | OK — 162 crates, none banned |
| Cross targets | `cargo clippy --target x86_64-pc-windows-gnu -D warnings` ✅ and Windows release `.exe` builds; `cargo check --target aarch64-apple-darwin` ✅ for every crate except those linking `ring` (see gaps) |

### Failing / excluded spec cases
* **Tokenizer:** none (7046 / 7046). Processing instructions (HTML §13.2.5.72–76) and lone surrogates are implemented.
* **Tree construction:** none (1959 / 1959). `select` is a default-scope boundary (current spec), and option selectedness plus `<selectedcontent>` mirroring are DOM state in `bb-dom` (`select.rs`), driven by the insertion/removing/attribute-change steps and the parser's "children parsed" step.
* **Not run yet:** 14 `#script-on` tree cases (8 in `tests*.dat`/`webkit02.dat`, 6 in `scripted_*.dat`); they need our JS engine (milestone E).

## Real sites checked (headless + GUI, 2026-11-21)

| Site | Result |
|---|---|
| https://example.com | ✅ matches Firefox for the static HTML: `light-dark()` background in both schemes, 26em column, 25vh padding, system-ui 16px/1.6. The extra translations, icon and "Learn more" link Firefox shows are inserted by `s.js` (needs M10 JS); a hand-applied post-JS snapshot renders correctly incl. RTL Arabic, CJK, Cyrillic (SVG icon not drawn). |
| http(s)://theoldnet.com/ | ✅ GIFs, `<font>` sizes/colors, `<center>`, bordered/nested tables with cellspacing/cellpadding, form controls drawn (not yet usable, see gaps), link colors, favicon. Full page (6889 px) in ~0.8 s. |
| http://theoldnet.com/lite | ✅ |
| https://myip.wtf and https://myip.wtf/headers | ✅ (HTTPS through the sandbox's CONNECT proxy; `/headers` shows exactly what we send: `Accept`, `Accept-Encoding: gzip, deflate, br`, `Accept-Language`, keep-alive, `User-Agent: Mozilla/5.0 (X11; Linux x86_64) BannedBrowser/0.1.0-dev`) |
| Archived sites via theoldnet `/get?...` | ✅ checked on GitHub runners (the dev sandbox can't reach web.archive.org): CI workflow "Archived sites" renders them live with our real UA (theoldnet redirects modern browsers to web.archive.org) and snapshots them with images. 9 of 12 candidates rendered live; 3 failed on web.archive.org timeouts / "Temporarily Offline". Kept as regression snapshots: **GeoCities Area51 fan page (1999), GeoCities home (Oct 1996), Netscape home (Oct 1996), WinZip home (1996), Microsoft IE download page (1996)**. Missing images are genuine archive 404s (geocities 2, netscape 1, microsoft-ie 1). Found and fixed a line-breaking bug on netscape.com (a word split across `<font>` elements overflowed its cell). |

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
- [x] Frames: real `<frameset>`/`<frame>` layout (rows/cols list-of-dimensions, nested framesets, borders, `frameborder`/`border`), `<iframe src|srcdoc>`, link `target` (`_self/_parent/_top/_blank`/named frames, `<base target>`), per-frame wheel/keyboard scrolling with a scroll indicator, `scrolling="no"`, container `marginwidth/marginheight`, depth limit 4 / 32 frames per page
- [x] GUI: window, tab strip, back/forward/reload/home, URL bar, wheel/keyboard scrolling, link hover cursor + status, click to navigate, title in tab + window, favicon, loading indicator, error pages, light/dark
- [x] GUI: overlay scrollbar (hover-widen, thumb drag, track paging, auto-hide)
- [x] GUI: find bar (Ctrl+F / Ctrl+G / F3, Enter / Shift+Enter, Esc; case-insensitive, matches across elements, highlights, "n of m", scrolls to the match)
- [x] GUI: ≡ app menu and context menus (page, link, image, tab, URL bar) with keyboard navigation. Done on `main` after the 0.1.0 release, so it ships with v0.2
- [x] Robustness: unknown tags, broken markup, missing images, `<script>` ignored, `<noscript>` shown
- [x] Local snapshots + regression tests (example.com, theoldnet.com + lite, myip.wtf)
- [x] 5 archived sites via theoldnet (GeoCities Area51, GeoCities home, Netscape, WinZip, Microsoft IE; live renders + offline snapshots)
- [x] CI green on all three OSes (build, clippy, tests, reftests, snapshots; GUI tests on Linux; since v0.2 work also on Windows and macOS)
- [x] Release artifacts built in CI on all three OSes: Linux `.tar.gz` (3.9 MB, binary + .desktop + icons), Windows `.zip` (2.9 MB), macOS `.app` zip (2.9 MB); stripped `dist` profile. Unsigned/not notarized.
- [x] README with real screenshots

## Known gaps and bugs (honest list)

* **Layout:** no `position: fixed` relative to the viewport (treated like absolute at scroll 0); `sticky` = relative; CSS Grid treated as block; inline-block baseline uses the first line (CSS says last); block-in-inline approximated (no inline splitting); `border-collapse: collapse` uses zero spacing without conflict resolution (borders double up); table captions sit inside the table border; no `vertical-align` for inline children of different fonts beyond the basics; `<textarea>` content does not wrap; flex lacks baseline alignment and `align-content`.
* **DOM strings:** UTF-8 `String`s. The tokenizer keeps lone surrogates (WTF-8), but the tree builder replaces them with U+FFFD when inserting into the DOM. Switching DOM text and attributes to `bb_str::Wtf8Buf` is planned with the JS bindings (milestone E).
* **Text:** bidi reordering is per text piece (no full UAX #9 incl. atomics); punctuation at RTL line ends can land on the wrong side; no small-caps; no `@font-face` loading yet (rules are parsed, fonts not fetched); no hinting.
* **Paint:** SVG is not rendered (space reserved, nothing drawn); no `transform` beyond `translate()`; background-origin/clip ignored; GIF animation not played.
* **Style:** no CSS counters, no `@layer` ordering (flattened), `:has()` sibling forms unsupported, nesting rules ignored, `lab()/lch()/oklch()/color-mix()` unsupported.
* **Forms:** controls (`<input>`, `<button>`, `<select>`, `<textarea>`) are only drawn. They can't be focused, typed into, opened or submitted, so search boxes like theoldnet's "Go" / "Search Archives" don't work. Interactive controls and form submission are milestone C.
* **Network:** no cookies or cache yet (M8); no HTTP/2; `HTTP_PROXY` absolute-form only for plain http.
* **Frames:** frame navigations are not in session history (Back leaves the frameset); no frame resizing by dragging borders (`noresize` is the only behaviour); iframes are opaque (child canvas background is always painted); `<object data=…html>` isn't a nested document; keyboard focus doesn't move into frames (keys scroll the largest frame of a frameset).
* **Find:** top-level document only (not inside frames); RTL runs highlight approximately; the counter is per tab but isn't refreshed when switching tabs with the bar open.
* **GUI:** menus are drawn inside the window (can't extend past its edges); "About BannedBrowser" opens `about:bannedbrowser` until the v0.2 About window; no bookmarks; no session restore; private windows share the network client (no cookies exist yet, so nothing leaks); screenshots of overlapping windows in GUI tests can be stale.
* **macOS cross-check:** `ring`'s build script needs the macOS SDK headers to compile its C/asm, which the Linux VM doesn't have; the macOS CI job covers it. Two attempts (clang + generic headers) failed; not retried further.
* **Packages:** not code-signed or notarized (macOS Gatekeeper / Windows SmartScreen will warn); no Windows icon resource embedded in the `.exe` yet.
* **Environment:** `web.archive.org` blocked by the dev sandbox's egress policy (archived-site checks run on GitHub runners); theoldnet.com serves "Hello World" to non-browser user agents (our UA is fine).

## Next steps: the v0.2 milestone order (agreed with the maintainer, 2026-11-21)

Each milestone ends with CI green on all three OSes, no excluded or failing tests, PROGRESS.md
and CHANGELOG `[Unreleased]` updated, pushed to `main`.

| # | Milestone | Contents |
|---|---|---|
| **A** | Test bar | Tokenizer input as code points (4 lone-surrogate cases); `<?…>` processing instructions (28); tree construction from current WPT incl. `<selectedcontent>` and `<font><select>`; floors to 100 %; bundled test fonts so snapshots run on every OS; GUI tests on Windows and macOS with real OS input. (`#script-on` lands in E.) |
| **B** | Maintainer requests | Certificate error page (readable reasons, DER-pinned exceptions, session token, `<details>`/`<summary>`); About window with an honest update check; frame navigations in session history. |
| **C** | M8 network + storage | Cookies, HTTP cache, profile storage (rusqlite), forms; HTTP/2 (HPACK, RFC 9113, ALPN). |
| **D** | M10a JS core | Parser, bytecode, interpreter, GC, built-ins, RegExp, Promise/async, modules; test262 `language`/`built-ins`/`annexB` in CI. |
| **E** | M10b Web APIs | DOM bindings, events + event loop, timers, fetch/XHR, storage, workers, `document.write`, Canvas 2D; `#script-on` cases; first whatismybrowser.com check. |
| **E2** | JIT | `bb-js-jit` (second `unsafe` exception); every suite passes with and without it. |
| **F** | M3/M4/M7 CSS + layout | Every gap above, grid, transforms, sticky/fixed, `@font-face` + WOFF/WOFF2, full bidi, counters, `@layer`, nesting, color functions; WPT CSS subset; ≥150 reftests. |
| **G** | M5 paint + GPU | SVG, CSS animations/transitions, GIF/APNG animation, filters; wgpu compositing/raster (software renderer stays the reference). |
| **H** | HTTP/3 | Our QUIC (RFC 9000/9001/9002), QPACK, HTTP/3 framing; `Alt-Svc`. |
| **I** | Media | MP4/WebM demuxers, Opus/AAC, our VP9, cpal output, `<video>`/`<audio>`, MSE, `bb-media-os` hardware decode. No DRM. |
| **J** | M9 browser features | Bookmarks, history, downloads, settings, Reader View, passwords, session restore, pinned tabs, tab dragging, sidebars. |
| **K** | WebExtensions | MV2/MV3, content/background scripts, `browser.*`, blocking `webRequest`, DNR, `.xpi`, add-ons page; uBlock Origin, Privacy Badger, SponsorBlock, SteamDB. |
| **L** | M11 | DevTools, incremental style/layout, accessibility (accesskit), Windows `.exe` icon. |
| **M** | Release | whatismybrowser.com acceptance, 100 % everywhere, `[release]` commit. |

**Now:** milestone A.

## v0.2 scope (agreed with the maintainer)

v0.2 will be developed by Claude. `AGENTS.md` holds the rules every coding agent
follows; Claude also reads `CLAUDE.md` for notes about its cloud VM.

v0.2 = M3–M11 in full, verified against https://www.whatismybrowser.com, plus:

* **HTTP/2** — our own framing, HPACK (RFC 7541), stream multiplexing and flow
  control (RFC 9113), negotiated via ALPN `h2` in rustls. `h2`/`hyper` stay banned.
* **HTTP/3** — our own QUIC transport (RFC 9000/9001/9002) on top of rustls'
  QUIC TLS API, QPACK (RFC 9204) and HTTP/3 framing (RFC 9114), discovered via
  `Alt-Svc`. `quinn`/`quiche`/`h3` stay banned.
* **Firefox extension parity** — WebExtensions: `manifest.json` (MV2 + MV3),
  content scripts, background scripts/service workers, `browser.*`/`chrome.*`
  APIs (tabs, storage, runtime messaging, webRequest, contextMenus, …) on our
  own JS engine (M10), `.xpi` installation, an add-ons manager page.
  **Acceptance extensions:** uBlock Origin, Privacy Badger, SponsorBlock
  (on youtube.com) and SteamDB (on store.steampowered.com). These imply a
  modern JS engine and DOM good enough for YouTube and the Steam store,
  `declarativeNetRequest`/blocking `webRequest`, cosmetic filtering via
  injected stylesheets, and extension options/popup pages.
* **Video playback** — `<video>`/`<audio>`, Media Source Extensions (YouTube
  needs MSE), MP4/WebM demuxers of our own; codecs (H.264, VP9, AV1, AAC,
  Opus) and audio output.
* **Hardware acceleration** — GPU compositing/rasterization and hardware
  video decode.

* **No excluded test cases** (maintainer, 2026-11-21). Every suite runs in full on every OS. Today's exclusions, each to be removed:
  - ~~**html5lib tokenizer, 4 lone-surrogate cases skipped.**~~ Done (session 2): the tokenizer takes WTF-8 input (`bb-str`) and keeps lone surrogates in its tokens; all 4 run and pass. Still open for E: the DOM stores UTF-8, so the tree builder replaces lone surrogates with U+FFFD on insertion; DOM strings must become WTF-8 for JS and `document.write`.
  - **Tree construction, 14 `#script-on` cases not run** (8 old ones plus 6 in WPT's `scripted_*.dat`). Run them with the scripting flag on once our JS engine exists (milestone E).
  - ~~**Tree construction is pinned to html5lib-tests `224991e~1`.**~~ Done (session 2): the suite now comes from WPT (`html/syntax/parsing/resources`, pinned to a WPT commit for reproducibility). That brought in processing instructions (now implemented), `#description` records, the new frameset-ok reset when `<body>` is inserted implicitly, and "parsing template contents" for the form element pointer (both fixed).
  - ~~**Snapshot regressions are skipped when the font set differs.**~~ Done (session 2): the test renders with only the bundled fonts in `tests/fonts/` (Liberation 2.1.5, Inter 4.0, Noto Naskh Arabic unmodified; Noto Sans SC and Noto Sans subsets; all OFL 1.1), ~7 MB. Two approvals changed after viewing: `retro` ("Comic Sans MS, Arial" now falls to Arial instead of the system's DejaVu alias) and `example.com-postjs` (Arabic now in Noto Naskh Arabic).
  - ~~**GUI tests run only on Linux.**~~ Done (session 2): all four GUI tests run in CI on Linux,
    Windows and macOS with real OS input: xdotool (Linux), `tests/gui/drivers/Driver.cs`
    (Windows, `SendInput`, built with the .NET Framework's `csc`) and
    `tests/gui/drivers/macos.swift` (macOS, `CGEvent` + Accessibility, built with `swiftc`).
    GitHub's macOS runners do grant Accessibility. macOS lessons: pressing the green button
    means full screen (zoom is Option-click), and hovering it opens a tiling popover that takes
    the keyboard, so tests keep the pointer inside the page.
* **All tests pass** (maintainer, 2026-11-21). v0.2 ships only with 100 % of every suite passing.
  - The pass-rate floors (URL 95 %, tokenizer 98 %, tree 90 %) become 100 %.
  - Known failures to fix:
    - ~~**html5lib tokenizer, 28 cases:** `<?…>` processing instructions.~~ Done (session 2); the tokenizer floor is 100 %.
    - ~~**Tree construction, 5 cases:** 4 × `<selectedcontent>` mirroring, plus `<font><select><option>`.~~ Done (session 2); the tree-construction floor is 100 %.
    - Plus whatever `#script-on` reveals once it runs (milestone E).
* **About window** (requested by the maintainer), inspired by Firefox's "About" dialog but with original branding only:
  - A small, non-resizable window, opened from ≡ menu → Help → About BannedBrowser (on macOS, the app menu).
  - BannedBrowser logo and wordmark, the version and architecture (e.g. "0.2.0 (64-bit)"), and the release name.
  - Update status: checks the GitHub Releases API on open. Shows "BannedBrowser is up to date" or links to the newer release; nothing is installed automatically.
    - The repository is public since 2026-11-21, so the anonymous `releases/latest` API works. When the check fails (offline, rate-limited, any non-200), it must say "Couldn't check for updates" with a link to the releases page. It must never claim "up to date" without a successful answer.
  - Links: what's new (release notes), `about:bannedbrowser` (credits and name origin), license (MIT OR Apache-2.0), source code.
  - The unofficial fan-project / not-affiliated note.
  - Drawn with `bb-ui` like the rest of the chrome, light and dark theme, HiDPI, keyboard accessible (Esc closes).
* **Certificate error page** (requested by the maintainer):
  - Two buttons: **Go Back** (recommended; goes home if there is no history) and **Advanced…**.
  - Advanced reveals the warning, the readable reason and an error code, plus **Accept the Risk and Continue**.
  - The reason must be human-readable, e.g. "the certificate expired on 2026-07-01", instead of today's raw `ExpiredContext { time: UnixTime(…) }`.
  - Planned design: accepting adds an in-memory exception through `bb-net`'s existing `ExceptionVerifier` / `TlsContext::add_exception`.
  - It should pin the exact certificate (host + DER bytes, like Firefox's overrides), not just the host as today.
  - The accept action needs a per-session secret token, so web pages can't link their way past a certificate error.
  - This needs either `:target`/`<details>` support or M10 JS for the Advanced toggle.

**JavaScript and media decisions (maintainer, 2026-11-21, session 2):**
* **JIT allowed.** `bb-js` gets a JIT in a separate crate, `crates/bb-js-jit`, the second
  `unsafe` exception (AGENTS.md hard rule 3). The bytecode interpreter stays `unsafe`-free and
  is the reference. test262 and every other suite must pass both with and without the JIT.
* **test262 scope:** `test/language`, `test/built-ins` and `test/annexB` must pass 100 %.
  `test/intl402` (the Intl API) and `test/staging` (proposals still in draft) are **out of
  scope for v0.2** and not run. This is a stated scope limit, not an exclusion inside a suite.
* **No DRM in v0.2.** Encrypted Media Extensions need a closed, licensed CDM, so protected
  playback is out of scope. A site that requests it gets a clear "protected content isn't
  supported" message. Normal YouTube playback is unaffected.

**Dependency decisions for v0.2 (decided by the maintainer 2026-11-21; details in
DEPENDENCIES.md):** GPU = `wgpu` (may also back WebGPU for content); video = OS
hardware decoders in `bb-media-os` (an `unsafe` exception, enforced by
`check-deps.sh`) plus our own VP9 fallback; audio = `cpal` for output, all codecs
ours. The original options, for the record:
1. *GPU backend.* `wgpu` implements the WebGPU standard, so it falls under the
   "no web-standard crates" rule; raw API bindings (`ash`/`metal`/`windows`)
   need `unsafe`, which the workspace forbids. Options: a narrowly scoped,
   documented `unsafe` exception confined to a `bb-gpu` platform crate, or an
   explicit allowance for `wgpu` as platform glue.
2. *Codecs and audio output.* Video codecs are not web standards, but writing
   H.264/VP9/AV1 decoders from scratch is a multi-month project each.
   Options: our own decoders (start with VP9 or AV1 for YouTube), OS decoders
   via platform APIs (Media Foundation / VideoToolbox / VA-API: again `unsafe`
   FFI), or an allowed codec crate. Audio output needs a platform crate such as
   `cpal` (not on the allowed list yet).
