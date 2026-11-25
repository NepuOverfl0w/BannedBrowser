# Changelog

All notable changes to BannedBrowser. Versions follow [SemVer](https://semver.org/);
the format is loosely based on [Keep a Changelog](https://keepachangelog.com/).

## [Unreleased] — v0.2

Everything on `main` after the 0.1.0 release commit counts toward v0.2.
The v0.2 scope is in [PROGRESS.md](PROGRESS.md).

### Added
- **International domain names follow UTS #46**: bb-url maps hosts with Unicode's IDNA
  mapping table (generated from Unicode 18 data), with CheckBidi and CheckJoiners; all of WPT's
  IdnaTestV2 and toascii cases pass (ASCII hosts are only lowercased, as the URL Standard's
  tests expect).
- Headless screenshots let the page's scripts run for a moment after load (`--script-wait <ms>`,
  default 1000), so pages that finish with timers or requests are captured as they settle.
- **Pages run JavaScript** (milestone E, in progress): the new `bb-webapi` crate gives each page
  a Window with the DOM (Node, Document, Element, Text, Attr, collections, HTML element
  interfaces, innerHTML), events with capture and bubbling and `on…` handlers, timers,
  `requestAnimationFrame`, `queueMicrotask`, console, `location` and `navigator`, `element.style`,
  `getComputedStyle` and element geometry (`getBoundingClientRect`, `offsetWidth`, …), `fetch()`,
  XMLHttpRequest, AbortController, URL/URLSearchParams, TextEncoder/TextDecoder, `localStorage`
  (saved in the profile), `sessionStorage` and `document.cookie`, MutationObserver, Range and
  StaticRange, NodeIterator and TreeWalker, `getSelection()` and named access on the Window, DOMParser and
  XMLSerializer over a new XML parser (`bb-xml`), XMLHttpRequest's `responseXML`, Blob, File,
  FileReader, FormData, blob URLs, `crypto.getRandomValues`/`randomUUID`, dedicated workers
  (own thread and VM; structured clone in bb-js), `structuredClone`, `window.postMessage`,
  MessageChannel/MessagePort (transferable to workers), BroadcastChannel, Canvas 2D
  (`<canvas>` with CanvasRenderingContext2D, Path2D, ImageData, `createImageBitmap`), and
  frames with their own windows: each iframe and frame runs its document's scripts in its own
  realm of the page's VM, `contentWindow`/`contentDocument`, `parent`, `top`, `frames[i]`,
  `frameElement` and `postMessage` work between them, and a frame of another origin is only
  reachable through a cross-origin wrapper (`postMessage`, `location` assignment, a few
  properties; anything else throws a SecurityError). `document.open`/`close` and
  `document.write` after loading replace the document, as in other browsers. Form events:
  the `<details>` `toggle` event and exclusive `name` groups,
  `input`/`change` from checkboxes, radios, selects and edited text fields, cancelable `reset`
  and `submit` (SubmitEvent with `submitter`), `form.submit()` and `requestSubmit()`, and
  activation behavior for clicks scripts dispatch (`checkbox.click()` toggles it, `link.click()`
  navigates, labels and summaries work). Module scripts import other modules (static and
  dynamic `import`, URL specifiers), and module workers (`type: 'module'`) run. Custom
  elements: `customElements.define`, autonomous and customized built-in elements, upgrades,
  and connected/disconnected/adopted/attributeChanged callbacks, `:defined`, ElementInternals
  (form-associated custom elements with form values, validity and form callbacks; custom
  states and `:state()`). Streams: ReadableStream,
  WritableStream, TransformStream, the queuing strategies, TextEncoderStream/TextDecoderStream,
  `Response.body` and `Blob.stream()`, readable byte streams with BYOB readers, and stream
  request bodies (`duplex: 'half'`). The
  Geometry Interfaces: DOMPoint, DOMQuad, DOMMatrix (with CSS transform strings) and
  `WebKitCSSMatrix`, structured-cloneable, and canvas transforms as DOMMatrix. Shadow DOM:
  `attachShadow`, ShadowRoot, slots with named and manual assignment and `slotchange`,
  declarative shadow DOM (`<template shadowrootmode>`, `setHTMLUnsafe`, `parseHTMLUnsafe`,
  `getHTML`), events retargeted across shadow boundaries with `composedPath()`, and pages
  rendering the flat tree with shadow-scoped styles (`:host`, `::slotted()`). The parser
  runs scripts as it goes, with `document.write`; script changes restyle and relayout the
  page; clicks and keys reach page listeners first and can be canceled. Measured with WPT's
  `dom/`, `domparsing/`, `FileAPI/`, `xhr/formdata/`, `url/`, `encoding/` and `fetch/api/headers/` tests
  and `workers/`, `webmessaging/`, structured clone, `custom-elements/`, `streams/` and
  `shadow-dom/` (103793/125284 subtests so far).
- **JavaScript engine (milestone D):** `bb-js` has a lexer and a parser
  for the whole ES2025 grammar (plus stage-3 import phases and `using`) with its early errors,
  measured against test262, and an interpreter: bytecode compiler, stack VM, garbage
  collector and the core built-ins (Object, Function, errors, Array, String, Number, Boolean,
  Symbol, Math, Reflect), then classes, generators, async functions and generators, Promise,
  Proxy, BigInt, JSON, Date, Map/Set and the weak collections, typed arrays and DataView,
  Atomics, iterator helpers and our own RegExp engine; then modules (`import`, `export`,
  `import()`, `import.meta`, top-level await), explicit resource management (`using`,
  `await using`, DisposableStack), realms, and the ES2026 and stage-3 built-ins test262
  covers (Array.fromAsync, Error.isError, Math.sumPrecise, Uint8Array base64/hex,
  JSON.rawJSON, iterator chunks/windows/includes/join/zip, Promise.allKeyed); then mapped
  `arguments`, `import defer`, ShadowRealm, immutable ArrayBuffers, Atomics.wait/waitAsync
  across `$262.agent` threads, `Error.prototype.stack`, Temporal (ISO calendar; the IANA
  time zone database, tzdata 2026e, which Date's local time uses too), [[IsHTMLDDA]] for `document.all` and the legacy RegExp statics
  (`RegExp.$1`, `RegExp.lastMatch`, …), proper tail calls, decorators and auto-accessors,
  JSON, text and bytes modules (`import … with { type: "json" }`) and source-phase imports:
  all of test262 (`language`, `built-ins`, `annexB`: 48803 tests, sloppy and strict) passes,
  and so does Unicode's normalization conformance test. It also has the structured clone
  algorithm (serialize, deserialize, transfer) that workers and `postMessage` use.
- **HTTP/2** (RFC 9113) with our own HPACK (RFC 7541, incl. Huffman coding), negotiated with
  ALPN `h2` over TLS (also through CONNECT proxies); HTTP/1.1 otherwise. One connection per
  origin carries every request to it (multiplexed, up to the server's
  `SETTINGS_MAX_CONCURRENT_STREAMS`), with flow control both ways, trailers, PING, and requests
  retried on a new connection after GOAWAY or REFUSED_STREAM. Server push is turned off.
  Tested against an independent server (the Python `h2` package) and live sites.
- **HTTP cache** (RFC 9111): responses are reused while fresh (`max-age`, `Expires`, or a
  heuristic from `Last-Modified`) and revalidated with `If-None-Match` / `If-Modified-Since`
  when stale (a 304 refreshes the stored headers); `Vary`, `no-store`, `no-cache`, and
  invalidation after POST and other unsafe methods. The cache is partitioned by top-level site,
  stored on disk (256 MB, least recently used entries evicted), and never stores or replays
  `Set-Cookie`. Reload revalidates, Ctrl+Shift+R / Ctrl+F5 / Shift+Reload bypass the cache,
  Back/Forward prefer it. Private windows get a memory-only cache, forgotten with the last
  private window.
- **Profile storage:** a profile directory (`--profile <dir>`, `$BB_PROFILE`, else the
  platform's data directory, e.g. `~/.local/share/BannedBrowser/profile`) with one SQLite
  database. Persistent cookies and `localStorage` survive restarts (session cookies and private
  windows' cookies and storage are never written); they are saved a moment after they change and at exit. The window size and
  maximized state are restored. A second BannedBrowser on the same profile runs without saving
  instead of overwriting it; a damaged database is moved aside and a new one started; a profile
  from a newer version is left untouched.
- **Forms work:** click or Tab into text fields and type (selection, word moves, undo, copy,
  cut, paste, IME, `maxlength`, `readonly`, `placeholder`, `size`); textareas (`rows`/`cols`,
  Enter, Up/Down); checkboxes and radio groups (mouse, Space, arrow keys); `<select>` drop-downs
  (with `<optgroup>`, disabled options, arrow keys on the closed select); buttons; `<label>`;
  `fieldset disabled`. Forms submit with GET or POST (`application/x-www-form-urlencoded`,
  `multipart/form-data`, `text/plain`) in the page's encoding, with `formaction`/`formmethod`/
  `formtarget`, image buttons, `_charset_`, `dirname`, Enter in a text field (implicit
  submission), reset buttons, and the `required` check (the empty field gets focus). The parser
  associates controls with their form across tables, as old sites need. Submissions follow
  `target` into frames and new tabs; Back/Forward/Reload re-send a POST.
- **Keyboard focus in pages:** Tab/Shift+Tab move through links, controls and `<summary>`
  (positive `tabindex` first, across frames), with a focus ring; Enter follows a focused link;
  Space/Enter toggle a focused `<details>`. Tab from the address bar enters the page, and Tab
  past the last element returns to the address bar.
- **≡ app menu:** New Tab, New Window, New Private Window, Find in Page…, About BannedBrowser, Exit/Quit.
- **Context menus:**
  - Page: Back, Forward, Reload, Find in Page….
  - Link: open in new tab, window or private window; Copy Link.
  - Image: Open Image in New Tab, Copy Image Link.
  - Tab: New, Reload, Duplicate, Close, Close Other Tabs, Reopen Closed Tab.
  - URL bar: Undo, Cut, Copy, Paste, Paste & Go, Delete, Select All.
- Menus work with the mouse (click, or press-drag-release) and keyboard (arrows, Enter, Esc, the
  context-menu key or Shift+F10). The last v0.1 checklist item, finished after the 0.1.0 release.

- **HTML tokenizer:** input is a sequence of code points that may include lone surrogates
  (HTML §13.2.3.5), via the new `bb-str` crate's WTF-8 strings. Surrogates are kept in every
  token; the html5lib tokenizer suite now runs without skipping any case.
- **Processing instructions** (`<?target data?>`, HTML §13.2.5.72–76): tokenized, inserted as
  `ProcessingInstruction` DOM nodes wherever comments go, and serialized as `<?target data?>`.
  Invalid or `xml`/`xml-stylesheet` targets still become bogus comments. Tokenizer: 7046/7046.

- **Option selectedness and `<selectedcontent>`** (customizable `<select>`): options keep a
  selectedness state (first enabled option by default, last `selected` one wins), and a
  `<selectedcontent>` inside the select's `<button>` mirrors the selected option's content
  (without scripts, frames, embeds or objects). The drop-down shows the option the DOM says is
  selected.

- **Cookies** (RFC 6265bis): `Set-Cookie` parsing (all date formats, Max-Age, Domain, Path,
  Secure, HttpOnly, SameSite), the Public Suffix List (a site can't set cookies for `co.uk` or
  `github.io`), SameSite rules for subresources and frames (unset means Lax), `__Secure-` and
  `__Host-` prefixes, eviction limits. Private windows have their own cookies, forgotten when
  the last private window closes. Persistent cookies are saved in the profile (see Profile storage).
- **Public Suffix List kept current:** the list isn't stored in the repository. The build
  downloads it and embeds it, and the browser downloads it again at first start and then at
  most once a day (publicsuffix.org's guidance), installing it only if it is a valid, newer
  list. Domains with a trailing dot keep it (`example.com.` → `com.`), and domains with empty
  labels have no registrable domain. Without any list (an offline build before the first
  download), `Domain` attributes that name a parent domain are ignored, so cookies stay
  host-only. The list's own `test_psl.txt` vectors run in CI.
- **Certificate error page:** "Go Back (Recommended)" (home when there's no history) and
  "Advanced…", which shows a readable reason (e.g. "the certificate expired on 2026-07-01"), an
  error code and **Accept the Risk and Continue**. Accepting trusts only that exact certificate
  for that host, for the session, and only works from the error page itself (a per-session
  secret token; web pages can't link past the warning).
- **About window** (≡ menu → About BannedBrowser): logo and wordmark, version and architecture,
  release name, an update check against the GitHub Releases API ("up to date" only after a
  successful answer; development builds and failures say so, with a link to the releases page),
  links to what's new, credits, license and source code, and the fan-project note. Light and
  dark, HiDPI, keyboard (Tab, Enter, Esc). Nothing is installed automatically.
- **`<details>` and `<summary>`:** closed details show only their summary (or a default
  "Details"), the summary comes first, and clicking it toggles `open`.

### Fixed
- A named `<button>` submitted an empty value with its form; it now submits its `value`.
- Frame navigations are session history entries: Back after following a link inside a frameset
  goes back to the previous frame contents instead of leaving the frameset, and only the frames
  that differ are reloaded.
- `</font>` (and other end tags) no longer reach past a `<select>`: `select` is a scope boundary.
- `<frameset>` after a `<template>` in the head: implicitly inserting `<body>` resets the
  frameset-ok flag (current spec).
- `<form>` inside template contents, including `template` fragments: the form element pointer
  follows the spec's "parsing template contents" (template on the stack, or a template context).

### Changed
- `about:bannedbrowser` matches the README: tagline, what we wrote ourselves, the name's origin
  (Silver Wolf's handle `@Banned_Player`, replacing the old "nickname" claim) with the
  trademark disclaimer, acknowledgements, license, and links to the repository and releases.
- The README status banner points at the 0.1.0 release.
- Version is `0.2.0-dev` on `main`.

### Development
- GUI tests run on Windows and macOS too, with real OS input (`SendInput`, `CGEvent`), in CI on
  every push. They now also assert window resizing and maximizing.
- Snapshot regressions render with bundled, openly licensed fonts (`tests/fonts/`: Liberation,
  Inter, Noto Naskh Arabic, small Noto Sans SC / Noto Sans subsets) instead of the system's, so
  they run on Linux, Windows and macOS rather than being skipped off Linux.
- Tree-construction tests come from WPT (`html/syntax/parsing/resources`, where they moved from
  html5lib-tests), pinned to a WPT commit. Tokenizer 7046/7046 and tree construction 1959/1959; both suites
  must now pass 100 %.
- Ready for a public repository: `SECURITY.md` (private vulnerability reporting),
  `CODE_OF_CONDUCT.md`, issue templates (bug, website compatibility, feature) and a pull
  request template; `docs/CONTRIBUTING.md` now covers setup per OS, every test layer, and
  good first areas.
- "Re-publish release" workflow: manually re-creates a release and its tag on another
  commit of `main` (used after history rewrites). It refuses before deleting anything when the
  Actions token couldn't re-create the tag, and keeps a tag that already points at the target.
- `scripts/check-deps.sh` enforces the `unsafe` policy:
  - Every crate except `crates/bb-media-os` must inherit `unsafe_code = "forbid"`.
  - No `allow(unsafe_code)` anywhere else.
  - Each `unsafe` in `bb-media-os` needs a `// SAFETY:` comment.
  - Its self-test now runs in CI, and works with macOS's BSD tools (it used GNU-only `sed -i`).
- `AGENTS.md` is the rulebook for every coding agent; `CLAUDE.md` holds Claude-specific notes.
- The v0.2 plan in PROGRESS.md covers:
  - dependency decisions (wgpu, OS video decoders + own VP9, cpal);
  - no excluded and no failing tests;
  - certificate-error page buttons;
  - a Firefox-inspired About window.

## [0.1.0] — "Hello, Old Web" — 2026-11-21

The first milestone: a from-scratch engine and shell that render the static, 1990s-style
web and simple modern pages. No JavaScript yet. See [PROGRESS.md](PROGRESS.md) for the
complete, honest status, including the known gaps.

### Engine
- **URL:** WHATWG URL Standard parser incl. IDNA/Punycode hosts. Passes 904/904 of WPT `urltestdata`.
- **Networking:**
  - HTTP/1.1 with keep-alive pooling, chunked transfer and gzip/deflate/brotli.
  - Redirects; HTTPS through rustls with webpki roots and extra CAs.
  - `HTTP(S)_PROXY`/`NO_PROXY` with CONNECT.
  - `data:`, `file:` and `about:` URLs.
  - Encoding sniffing: BOM → header → `<meta>` prescan → fallback.
- **HTML:** full tokenizer and tree builder, including the current-spec `<select>` parsing. html5lib-tests: 99.60 % tokenizer, 99.72 % tree construction.
- **CSS:**
  - Syntax 3 parser; Selectors 4 incl. `:is/:where/:not/:has`.
  - Cascade with origins and `!important`; custom properties; `calc()/min()/max()/clamp()`.
  - Media queries and `@supports`.
  - Colors incl. `light-dark()`, HTML §15 UA stylesheet, quirks mode and presentational hints.
- **Layout:**
  - Block and inline formatting with margin collapsing; floats and clear.
  - UAX #14 line breaking and basic bidi.
  - HTML tables (auto/fixed, colspan/rowspan); basic flexbox; relative/absolute positioning.
  - Replaced elements, list markers, image maps.
- **Frames:**
  - `<frameset>`/`<frame>` with rows/cols and borders; `<iframe src|srcdoc>`.
  - Link targets (`_self/_parent/_top/_blank`, named frames, `<base target>`).
  - Per-frame scrolling; depth/count limits.
- **Paint:**
  - Anti-aliased software rasterizer with subpixel-positioned glyph cache.
  - Borders, backgrounds and gradients, box shadows, clips, opacity.
- **Images:** PNG, JPEG, GIF (first frame), WebP, plus our own BMP/ICO decoders.
- **Find in page** with highlights.

### Browser shell
- Proton-inspired window with tab strip, toolbar, URL bar (editing, IME, undo) and favicons.
- Loading indicator, link hover status, overlay scrollbar and a find bar.
- Tabs: new, close, reopen, switch; Ctrl/middle-click opens background tabs.
- Multiple windows and private windows; light/dark themes.
- The UI thread never blocks: each tab runs on its own worker thread. Panics show a "hit a snag" page.
- Headless mode: `--screenshot`, `--dump-dom/style/layout`, `--user-agent`.

### Quality
- 48 reftests; offline snapshot regressions of real sites (example.com, theoldnet.com, myip.wtf, and
  five archived 1996–99 sites: GeoCities ×2, Netscape, WinZip, Microsoft IE) and our own
  retro and frames pages.
- GUI tests under Xvfb + Fluxbox (basic flow, scrollbar, frames, find).
- Randomized robustness tests for every parser.
- CI on Linux, Windows and macOS; packages: Linux tarball, Windows zip, macOS `.app`.
