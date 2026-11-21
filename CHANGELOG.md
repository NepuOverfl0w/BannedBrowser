# Changelog

All notable changes to BannedBrowser. Versions follow [SemVer](https://semver.org/);
the format is loosely based on [Keep a Changelog](https://keepachangelog.com/).

## [0.1.0] — "Hello, Old Web" (unreleased)

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
- 47 reftests; offline snapshot regressions of real sites and our own retro and frames pages.
- GUI tests under Xvfb + Fluxbox (basic flow, scrollbar, frames, find).
- Randomized robustness tests for every parser.
- CI on Linux, Windows and macOS; packages: Linux tarball, Windows zip, macOS `.app`.
