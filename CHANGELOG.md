# Changelog

All notable changes to BannedBrowser. Versions follow [SemVer](https://semver.org/);
the format is loosely based on [Keep a Changelog](https://keepachangelog.com/).

## [Unreleased] — v0.2

Everything on `main` after the 0.1.0 release commit counts toward v0.2.
The v0.2 scope is in [PROGRESS.md](PROGRESS.md).

### Added
- **≡ app menu:** New Tab, New Window, New Private Window, Find in Page…, About BannedBrowser, Exit/Quit.
- **Context menus:**
  - Page: Back, Forward, Reload, Find in Page….
  - Link: open in new tab, window or private window; Copy Link.
  - Image: Open Image in New Tab, Copy Image Link.
  - Tab: New, Reload, Duplicate, Close, Close Other Tabs, Reopen Closed Tab.
  - URL bar: Undo, Cut, Copy, Paste, Paste & Go, Delete, Select All.
- Menus work with the mouse (click, or press-drag-release) and keyboard (arrows, Enter, Esc, the
  context-menu key or Shift+F10). The last v0.1 checklist item, finished after the 0.1.0 release.

### Changed
- `about:bannedbrowser` matches the README: tagline, what we wrote ourselves, the name's origin
  (Silver Wolf's handle `@Banned_Player`, replacing the old "nickname" claim) with the
  trademark disclaimer, acknowledgements, license, and links to the repository and releases.
- The README status banner points at the 0.1.0 release.
- Version is `0.2.0-dev` on `main`.

### Development
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
