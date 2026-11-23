# BannedBrowser — progress log

_Last updated: 2026-11-22 (session 2, milestone C done)._

## Status at a glance

| Milestone | Status | Notes |
|---|---|---|
| M0 Scaffold | ✅ done | workspace, check-deps, CI workflow, docs, headless PNG, window under Xvfb + Fluxbox |
| M1 URL + networking | ✅ done | 904/904 WPT urltestdata; HTTP/1.1 + TLS + proxies + codings + redirects; cookies (RFC 6265bis) with the Public Suffix List |
| M2 HTML | ✅ done | tokenizer 100 % (html5lib-tests), tree construction 100 % (WPT); processing instructions; lone surrogates in the tokenizer; `<selectedcontent>` |
| M3 CSS + style | 🚧 mostly | parser, selectors, cascade, var(), calc(), media, UA sheet, presentational hints; see gaps |
| M4 Layout I | 🚧 mostly | block/inline/floats/tables/flex(basic)/positioned/replaced; see gaps |
| M5 Paint | 🚧 mostly | AA rasterizer, borders, gradients, shadows, clips, opacity, glyph cache; 50 reftests |
| M6 GUI shell core | 🚧 started | tabs, toolbar, URL bar, overlay scrollbar, find bar, frames, links, multiple + private windows, test-state hook, ≡ and context menus, About window, certificate error page, frame history |
| M7–M11 | 📋 planned | |
| **v0.1 "Hello, Old Web"** | ✅ released 2026-11-21 ([v0.1.0](https://github.com/NepuOverfl0w/BannedBrowser/releases/tag/v0.1.0)) | example.com, theoldnet.com, 5 archived sites, snapshots, CI green on 3 OSes, packages; see checklist |

## Test numbers (session 2)

| Suite | Result |
|---|---|
| WPT `url/resources/urltestdata.json` | **904 / 904 (100.0 %)** |
| html5lib tokenizer (HEAD) | **7046 / 7046 (100 %)**, nothing skipped (incl. the 4 lone-surrogate cases) |
| Public Suffix List `tests/test_psl.txt` (HEAD) against the list the build embedded | **78 / 78 (100 %)** |
| test262 parse phase (`language`, `built-ins`, `annexB`, pinned `ebf6ae6`): parses exactly when it should | **48803/48803 (100 %)**, no panics |
| test262 execution (same tests, run in our interpreter; sloppy + strict variants) | **48803/48803 (100 %)**, no panics, floor at 100 %. Unicode NormalizationTest.txt (UAX #15) passes too. |
| HPACK, RFC 7541 Appendix C examples (C.1 integers, C.3/C.4 requests, C.6 responses with eviction) | **all pass**, plus Huffman round trips and malformed-block tests |
| HTTP/2 against the Python `h2` server (`tests/server/h2server.py`) | **8 / 8** integration tests: headers, POST bodies past the peer's window, a 20 MB download past ours, 20 requests multiplexed under a 5-stream limit, redirects/cookies/gzip, trailers, CONTINUATION, PING, REFUSED_STREAM and GOAWAY retries, a malformed content-length |
| Tree construction, WPT `html/syntax/parsing/resources` (pinned `9986bb3`, 2026-11-21) | **1959 / 1959 (100 %)**, plus the 14 `#script-on` cases with scripts running (**14 / 14**, in bb-webapi's `tree_script_on` test) |
| Reftests (`tests/reftests`) | **50 / 50** |
| Snapshot regressions (`tests/pages/snapshots`) | 12 pages (example.com ×2, theoldnet ×2, myip.wtf, 5 archived sites, retro, frames), rendered with the bundled `tests/fonts/` only, so they run on every OS |
| `cargo test --workspace` | 255 tests, 0 failures |
| GUI tests (`scripts/gui-smoke.sh`), Linux + Windows + macOS with real OS input | `basic.sh` (open, link click, back, wheel scroll, scrollbar track click + thumb drag, URL bar, new/close/reopen tab, resize, maximize, new window, private window), `frames.sh` (frameset render, wheel scrolls the frame under the pointer, targeted link loads into the named frame), `find.sh` (incremental find, stepping, scroll to match, not found, close), `menus.sh` (≡ menu, link/tab/page/URL-bar context menus, keyboard and mouse, Esc and click-outside) and `certerror.sh` (certificate error page: Go Back, a web page's forged accept link does nothing, Advanced… toggles, Accept the Risk and Continue loads the site), `about.sh` (About window from the ≡ menu, update check result, Tab/Enter opens a link in a tab, Esc closes) and `about-offline.sh` (a failed check says so and offers the releases page), `cookies.sh` (cookies shared across tabs, not sent from a private window) and `forms.sh` (type into a field, Tab to a checkbox and Space, arrow keys and the drop-down of a select, GET submission with a button; a two-line textarea POSTed) `profile.sh` (quit and restart: a persistent cookie and the window size are restored, a session cookie isn't) and `cache.sh` (a fresh page comes from the cache, Reload asks again, Ctrl+Shift+R bypasses, Back uses the cached copy) pass; each test gets a fresh profile |
| `scripts/check-deps.sh` | OK — 172 crates, none banned |
| Cross targets | `cargo clippy --target x86_64-pc-windows-gnu -D warnings` ✅ and Windows release `.exe` builds; `cargo check --target aarch64-apple-darwin` ✅ for every crate except those linking `ring` (see gaps) |

### Failing / excluded spec cases
* **Tokenizer:** none (7046 / 7046). Processing instructions (HTML §13.2.5.72–76) and lone surrogates are implemented.
* **Tree construction:** none (1959 / 1959). `select` is a default-scope boundary (current spec), and option selectedness plus `<selectedcontent>` mirroring are DOM state in `bb-dom` (`select.rs`), driven by the insertion/removing/attribute-change steps and the parser's "children parsed" step.

## Real sites checked (headless + GUI, 2026-11-21)

| Site | Result |
|---|---|
| https://example.com | ✅ matches Firefox for the static HTML: `light-dark()` background in both schemes, 26em column, 25vh padding, system-ui 16px/1.6. The extra translations, icon and "Learn more" link Firefox shows are inserted by `s.js` (needs M10 JS); a hand-applied post-JS snapshot renders correctly incl. RTL Arabic, CJK, Cyrillic (SVG icon not drawn). |
| http(s)://theoldnet.com/ | ✅ GIFs, `<font>` sizes/colors, `<center>`, bordered/nested tables with cellspacing/cellpadding, link colors, favicon. Its forms work (2026-11-22): "Search Archives" returns results (`/find?query=geocities`), and the year drop-down + Go submits `/get?url=…&year=1999&scripts=false&decode=false` (which redirects to web.archive.org, blocked in the dev sandbox). Full page (6889 px) in ~0.8 s. |
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

* **JavaScript (bb-js):** objects have no hidden classes yet (property lookups on new objects and prototype chains are the slow paths; the JIT is E2). Weak collections hold their entries strongly and FinalizationRegistry never calls back (allowed, but leaks). Temporal has the ISO calendar only: the other calendars need CLDR data and belong with Intl (not started). Time zones come from IANA tzdata 2026e, embedded at build time, so a newer release needs `scripts/gen-tzdata.py` and a rebuild. In that release CST6CDT, EST5EDT, MST7MDT and PST8PDT are full zones again, not links, so two intl402 tests from older data fail (`ZonedDateTime/links.js`). The local zone is the system's: `TZ`, else `/etc/localtime` or `/etc/timezone` on Unix, else Windows' own ICU (`icu.dll`, Windows 10 1903 and later; older Windows runs in UTC). A zone change while the browser runs isn't picked up until restart.
* **Layout:** no `position: fixed` relative to the viewport (treated like absolute at scroll 0); `sticky` = relative; CSS Grid treated as block; inline-block baseline uses the first line (CSS says last); block-in-inline approximated (no inline splitting); `border-collapse: collapse` uses zero spacing without conflict resolution (borders double up); table captions sit inside the table border; no `vertical-align` for inline children of different fonts beyond the basics; `<textarea>` content does not wrap; flex lacks baseline alignment and `align-content`.
* **DOM strings:** UTF-8 `String`s. The tokenizer keeps lone surrogates (WTF-8), but the tree builder replaces them with U+FFFD when inserting into the DOM. Switching DOM text and attributes to `bb_str::Wtf8Buf` is planned with the JS bindings (milestone E).
* **Text:** bidi reordering is per text piece (no full UAX #9 incl. atomics); punctuation at RTL line ends can land on the wrong side; no small-caps; no `@font-face` loading yet (rules are parsed, fonts not fetched); no hinting.
* **Paint:** SVG is not rendered (space reserved, nothing drawn); no `transform` beyond `translate()`; background-origin/clip ignored; GIF animation not played.
* **Style:** no CSS counters, no `@layer` ordering (flattened), `:has()` sibling forms unsupported, nesting rules ignored, `lab()/lch()/oklch()/color-mix()` unsupported.
* **Certificate exceptions:** a site whose certificate you accepted shows the normal secure icon in the address bar (no distinct "exception" indicator yet). Exceptions last for the session only (no permanent overrides).
* **`<details>`:** toggles with the mouse and with Space/Enter on its focused summary; no `toggle` event (no JS).
* **Forms:** work (typing, checkboxes, radios, drop-downs, buttons, GET/POST submission, Tab navigation; see CHANGELOG). Not yet: file uploads (a file input submits an empty file; no file picker), date/time/number/range/color pickers (edited as plain text, values sanitized), constraint validation beyond `required` (no validation message bubble; the empty field just gets focus), `<select size>`/`multiple` list boxes (shown as drop-downs), scrolling in long drop-downs (they're clamped to the window; arrow keys on the closed select reach every option), textarea line wrapping, mouse drag-selection and double-click word selection in page fields, a text-field context menu, IME preedit display in page fields, caret blinking, an "insecure form" warning for https→http POSTs, a confirmation before Back/Reload re-sends a POST, POSTs in frame history (a frame's form POST is restored as a GET), autofill and saved passwords (J), `submit`/`input`/`change` events (E).
* **Cookies:** RFC 6265bis cookies with the Public Suffix List, SameSite (unset = Lax), Secure/HttpOnly, `__Secure-`/`__Host-` prefixes, eviction limits; private windows have their own jar, cleared when the last one closes. Persistent cookies are saved in the profile (C3); session cookies are never written. Not yet: `document.cookie` (E), a cookie manager / "clear cookies" UI (J), partitioned/third-party cookie isolation, and cross-site link clicks are treated as same-site top-level navigations (the navigation's initiator isn't passed yet, so Strict cookies go along). A build made offline (`BB_PSL_OFFLINE=1`) has no list until the browser's first successful download; until then `Domain` attributes naming a parent domain are ignored (host-only cookies) and each host is its own site.
* **Profile:** one profile only (no profile manager); a second browser started on a profile that is in use runs without saving (it doesn't hand its URLs to the running one, as Firefox does); prefs hold only the window size so far (settings UI is J); no "clear recent history".
* **Network:** HTTP cache done (C4); not yet: storing several `Vary` variants of one URL (the latest wins), `stale-while-revalidate` / `stale-if-error`, `Range` requests and partial content, a "clear cache" UI (J); HTTP/2 done (C5), not yet: stream priorities (ignored, as RFC 9113 allows), connection coalescing across origins (§9.1.1), `h2c`, an HTTP/2 PING keep-alive for idle connections; `HTTP_PROXY` absolute-form only for plain http.
* **Frames:** frame navigations are in session history (each history entry stores its frames' URLs; Back/Forward reload only the frames that differ), but frame scroll positions aren't restored; no frame resizing by dragging borders (`noresize` is the only behaviour); iframes are opaque (child canvas background is always painted); `<object data=…html>` isn't a nested document; keyboard focus doesn't move into frames (keys scroll the largest frame of a frameset).
* **Find:** top-level document only (not inside frames); RTL runs highlight approximately; the counter is per tab but isn't refreshed when switching tabs with the bar open.
* **GUI:** menus are drawn inside the window (can't extend past its edges); the About window is opened from the ≡ menu only: macOS's app menu "About" item still shows the system's standard panel (replacing it needs AppKit calls; possible now that `unsafe` is allowed, planned), and "About BannedBrowser" sits directly in the ≡ menu because menus have no submenus (no Help submenu) yet; no bookmarks; no session restore; screenshots of overlapping windows in GUI tests can be stale.
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
| **E2** | JIT | `bb-js-jit`; every suite passes with and without it. |
| **F** | M3/M4/M7 CSS + layout | Every gap above, grid, transforms, sticky/fixed, `@font-face` + WOFF/WOFF2, full bidi, counters, `@layer`, nesting, color functions; WPT CSS subset; ≥150 reftests. |
| **G** | M5 paint + GPU | SVG, CSS animations/transitions, GIF/APNG animation, filters; wgpu compositing/raster in `bb-gpu` (software renderer stays the reference); **full WebGPU and WebGL 1 + 2** for web content (needs E's JS bindings). |
| **H** | HTTP/3 | Our QUIC (RFC 9000/9001/9002), QPACK, HTTP/3 framing; `Alt-Svc`. |
| **I** | Media | MP4/WebM demuxers, Opus/AAC, our VP9, cpal output, `<video>`/`<audio>`, MSE, `bb-media-os` hardware decode. No DRM. |
| **J** | M9 browser features | Bookmarks, history, downloads, settings, Reader View, passwords, session restore, pinned tabs, tab dragging, sidebars. |
| **K** | WebExtensions | MV2/MV3, content/background scripts, `browser.*`, blocking `webRequest`, DNR, `.xpi`, add-ons page; uBlock Origin, Privacy Badger, SponsorBlock, SteamDB. |
| **L** | M11 | DevTools, incremental style/layout, accessibility (accesskit), Windows `.exe` icon. |
| **M** | Release | Fix the known gaps left by A–L (agreed 2026-11-22: v0.2's milestones first, then the gaps, then the release), whatismybrowser.com acceptance, 100 % everywhere, `[release]` commit. |

**Done:** milestone A (session 2), except the 14 `#script-on` tree cases, which need JS and land in E.
CI is green on all three OSes with every suite at 100 % and the GUI tests on each.

**Done:** milestone B (session 2): certificate error page, About window, frame navigations in
session history. Remaining placement gaps (macOS app-menu About, no Help submenu) are listed under
"Known gaps".

**Now:** milestone C (M8 network + storage). Done: C1 cookies with the Public Suffix List (kept current: downloaded at build time, at first start and then daily); C2 interactive forms and keyboard focus (theoldnet's search and archive forms work); C3 profile storage (`bb-profile`, SQLite: persistent cookies, window size); C4 HTTP cache (RFC 9111); C5 HTTP/2 (our own framing, HPACK, flow control; ALPN `h2`). **Milestone C is done** (CI run 72 green on all three OSes).

**Now:** milestone E (Web APIs). Steps: E-a DOM core bindings; E-b events and the event loop; E-c scripts in pages, `document.write`, `#script-on`; E-d fetch/XHR, URL and encoding APIs; E-e storage, workers, Canvas 2D and the first whatismybrowser.com check. **Pages run scripts:** each page has a `bb-webapi` Window; the parser runs scripts as it goes (`document.write` included), `<noscript>` is hidden, DOM changes restyle and relayout the page (new stylesheets are collected again), timers and animation frames run from the tab's event loop, scripts and images that scripts insert are loaded (`new Image()`, `Option`, `Audio` too), clicks and keys reach listeners as trusted events before the default action (which a listener can cancel), and `location` assignments navigate. `bb-webapi` has Node, Document, Element, CharacterData, Attr, the collections, HTML element interfaces with reflected attributes, innerHTML/outerHTML, events with handlers and timers. Networking from scripts: `fetch()` with Headers, Request and Response (text/json/arrayBuffer/bytes), XMLHttpRequest (async and sync, progress events, response types), AbortController/AbortSignal (also for listeners), with requests on background threads through the page's client (cookies, cache); URL and URLSearchParams (bb-url now has the URL setters with the parser's state override: all 284 WPT setter cases pass); TextEncoder/TextDecoder over encoding_rs; the document base URL (`<base href>`). Its WPT runner (`cargo test -p bb-webapi --test wpt`) runs `dom/` (nodes, collections, events, lists, abort), `url/`, `encoding/` and `fetch/api/headers/`: 23833/31990 subtests pass so far. Not run yet: `encoding/legacy-mb-*` (pages in legacy encodings; the runner reads pages as UTF-8) and `encoding/streams` (no streams). Scripts can style and measure: `element.style` (CSSStyleDeclaration over the style attribute), `getComputedStyle`, `getBoundingClientRect`, `offset*`/`client*` sizes and `elementFromPoint`, answered from a fresh style and layout pass when the document changed. Missing so far: scripts in frames (each frame has its own Window, without cross-frame access), iframes in WPT tests, MutationObserver, Range/TreeWalker, AbortController, DOMParser and an XML parser, custom elements, Shadow DOM, UTS #46 IDNA processing in URLs, FormData and Blob, ReadableStream, storage, workers, canvas.

**Now:** milestone D (JS core). Done: the lexer, parser and RegExp syntax with all early errors (**test262 parse phase 100 %**); the interpreter (`bb-js/src/vm`): values, a mark-sweep heap, the essential internal methods, an AST → bytecode compiler with spec declaration instantiation, the VM (calls, exceptions, `finally` on every exit, fuel and depth limits); classes, generators, async functions, async generators and `for await`, Promise and the job queue, Proxy, BigInt (own arbitrary-precision arithmetic), our own RegExp engine (backtracking matcher with a step budget, Unicode 17 property tables); built-ins Object, Function, errors, Array, String, Number, Boolean, Symbol, Math, Reflect, JSON, Date, Map/Set/Weak*, ArrayBuffer/SharedArrayBuffer/DataView/typed arrays, Atomics (one agent), Iterator helpers; modules with `import()` and `import.meta`; explicit resource management (`using`, `await using`, DisposableStack, AsyncDisposableStack, SuppressedError); realms (`$262.createRealm`); built-ins written in JavaScript (`selfhost.js`) for the async ones (Array.fromAsync, disposeAsync); the ES2026 additions and stage-3 proposals test262 covers (Error.isError, Math.sumPrecise, Uint8Array base64/hex, JSON parse source text and rawJSON, iterator chunks/windows/includes/join/zip, Promise.allKeyed). mapped `arguments`; `import defer`; `$262.agent` threads with Atomics.wait/waitAsync/notify over shared memory; ShadowRealm; immutable ArrayBuffers; `Error.prototype.stack`; Temporal (all of `built-ins/Temporal`, written in JavaScript in `temporal.js`; ISO calendar, UTC and offset time zones); Annex B's [[IsHTMLDDA]] (for `document.all`) and the legacy RegExp statics (`RegExp.$1`, `lastMatch`, …); proper tail calls; decorators with auto-accessors, `addInitializer` and `Symbol.metadata`; JSON, text and bytes modules (`with { type }`); source-phase imports; module evaluation as the spec's cyclic-module algorithm (async siblings run concurrently); Unicode normalization (NFC/NFD/NFKC/NFKD); lone surrogates in eval'd source text. **test262 execution 100 %: milestone D's bar is met.** Temporal time zones from the IANA database (tzdata 2026e, embedded; transitions, gaps and overlaps, `getTimeZoneTransition`), and Date's local time from the same data. Known limits: Temporal has the ISO calendar only (CLDR calendars belong with Intl); the interpreter is a plain bytecode loop: identifiers resolved to slots, inline caches for named properties, dense array elements and rope strings made it 3–14× faster on micro benchmarks, but objects have no hidden classes yet (the JIT in E2 comes after them). Next: milestone E.

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
* **About window** (requested by the maintainer), inspired by Firefox's "About" dialog but with original branding only. **Done (session 2)** in `crates/bannedbrowser/src/about.rs`, except the two menu placements noted under "Known gaps" (macOS app menu, Help submenu). The check reports "up to date" only after a successful answer, "development build" for `-dev` versions, and "Couldn't check for updates" plus the releases page on any failure; `BB_UPDATE_URL` points it at a local fixture in the GUI tests.
  - A small, non-resizable window, opened from ≡ menu → Help → About BannedBrowser (on macOS, the app menu).
  - BannedBrowser logo and wordmark, the version and architecture (e.g. "0.2.0 (64-bit)"), and the release name.
  - Update status: checks the GitHub Releases API on open. Shows "BannedBrowser is up to date" or links to the newer release; nothing is installed automatically.
    - The repository is public since 2026-11-21, so the anonymous `releases/latest` API works. When the check fails (offline, rate-limited, any non-200), it must say "Couldn't check for updates" with a link to the releases page. It must never claim "up to date" without a successful answer.
  - Links: what's new (release notes), `about:bannedbrowser` (credits and name origin), license (MIT OR Apache-2.0), source code.
  - The unofficial fan-project / not-affiliated note.
  - Drawn with `bb-ui` like the rest of the chrome, light and dark theme, HiDPI, keyboard accessible (Esc closes).
* **Certificate error page** (requested by the maintainer). **Done (session 2)**, as specified below: `bb-net` records the failing certificate and a readable reason with a code (`CERT_EXPIRED`, `CERT_UNKNOWN_ISSUER`, `CERT_WRONG_NAME`, …); `resources/pages/certerror.html` has Go Back and an `<details>` "Advanced…"; `bb-action:` links are handled by the tab worker, and accepting needs the session token and a top-level page that is that certificate's error page; the exception pins host + DER. Tested by `bb-net` integration tests, engine tests, two reftests and the GUI test `tests/gui/certerror.sh` on all three OSes.
  - Two buttons: **Go Back** (recommended; goes home if there is no history) and **Advanced…**.
  - Advanced reveals the warning, the readable reason and an error code, plus **Accept the Risk and Continue**.
  - The reason must be human-readable, e.g. "the certificate expired on 2026-07-01", instead of today's raw `ExpiredContext { time: UnixTime(…) }`.
  - Planned design: accepting adds an in-memory exception through `bb-net`'s existing `ExceptionVerifier` / `TlsContext::add_exception`.
  - It should pin the exact certificate (host + DER bytes, like Firefox's overrides), not just the host as today.
  - The accept action needs a per-session secret token, so web pages can't link their way past a certificate error.
  - This needs either `:target`/`<details>` support or M10 JS for the Advanced toggle.

**GPU and `unsafe` decisions (maintainer, 2026-11-22, session 2):**
* `unsafe` is allowed in every crate (AGENTS.md hard rule 3; it replaced a short-lived
  "platform crates only" rule the same day). Each `unsafe` block carries a `// SAFETY:`
  comment, enforced by clippy. Features that were blocked only by the old rule are now in
  scope: the macOS app-menu "About BannedBrowser" item opening our About window (AppKit),
  GPU access (`bb-gpu`), the JIT (`bb-js-jit`), OS hardware video decoding (`bb-media-os`).
* v0.2 includes **full WebGPU** (on wgpu) and **full WebGL 1 and 2** for web content (our own
  WebGL implementation on top of `bb-gpu`; GLSL ES shaders translated with wgpu's `naga`).
  Which conformance suites gate them (WebGPU CTS, Khronos WebGL conformance) and how is
  decided when milestone G starts.

**JavaScript and media decisions (maintainer, 2026-11-21, session 2):**
* **JIT allowed.** `bb-js` gets a JIT in a separate crate, `crates/bb-js-jit`. The bytecode
  interpreter is the reference. test262 and every other suite must pass both with and without the JIT.
* **test262 scope:** `test/language`, `test/built-ins` and `test/annexB` must pass 100 %.
  `test/intl402` (the Intl API) and `test/staging` (proposals still in draft) are **out of
  scope for v0.2** and not run. This is a stated scope limit, not an exclusion inside a suite.
* **No DRM in v0.2.** Encrypted Media Extensions need a closed, licensed CDM, so protected
  playback is out of scope. A site that requests it gets a clear "protected content isn't
  supported" message. Normal YouTube playback is unaffected.

**Dependency decisions for v0.2 (decided by the maintainer 2026-11-21; details in
DEPENDENCIES.md):** GPU = `wgpu` (may also back WebGPU for content); video = OS
hardware decoders in `bb-media-os` plus our own VP9 fallback; audio = `cpal` for output, all codecs
ours. The original options, for the record:
1. *GPU backend.* `wgpu` implements the WebGPU standard, so it falls under the
   "no web-standard crates" rule; raw API bindings (`ash`/`metal`/`windows`)
   need `unsafe`, which the workspace forbids. Options: a narrowly scoped,
   documented `unsafe` exception confined to a `bb-gpu` platform crate, or an
   explicit allowance for `wgpu` as platform glue. (Superseded 2026-11-22: `unsafe` is
   allowed everywhere.)
2. *Codecs and audio output.* Video codecs are not web standards, but writing
   H.264/VP9/AV1 decoders from scratch is a multi-month project each.
   Options: our own decoders (start with VP9 or AV1 for YouTube), OS decoders
   via platform APIs (Media Foundation / VideoToolbox / VA-API: again `unsafe`
   FFI), or an allowed codec crate. Audio output needs a platform crate such as
   `cpal` (not on the allowed list yet).
