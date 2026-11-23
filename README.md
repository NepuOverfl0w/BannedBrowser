<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/assets/logo-dark.png">
  <img alt="BannedBrowser" src="docs/assets/logo-light.png" width="560">
</picture>

### A web browser hacked together from scratch in Rust. No engines borrowed, no rules followed.

[![CI](https://github.com/NepuOverfl0w/BannedBrowser/actions/workflows/ci.yml/badge.svg)](https://github.com/NepuOverfl0w/BannedBrowser/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/NepuOverfl0w/BannedBrowser?include_prereleases&label=release&color=7c5cff)](https://github.com/NepuOverfl0w/BannedBrowser/releases)
![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue)
![Rust](https://img.shields.io/badge/rust-stable%201.85%2B-orange?logo=rust)
![Platforms](https://img.shields.io/badge/platforms-Windows%20%7C%20macOS%20%7C%20Linux-3df2e0)
![Engine](https://img.shields.io/badge/engine-100%25%20homemade-7c5cff)

[Download](#-getting-started) · [Build](#-getting-started) · [Features](#-features) · [Screenshots](#-screenshots) · [Roadmap](#-roadmap) · [FAQ](#-faq)

</div>

---

> **Status: [v0.1.0 "Hello, Old Web"](https://github.com/NepuOverfl0w/BannedBrowser/releases/tag/v0.1.0) released.**
> BannedBrowser renders example.com, theoldnet.com and archived 1990s sites convincingly.
> Since then `main` (on its way to v0.2) has gained working forms, a persistent profile, an HTTP
> cache, HTTP/2 and its own JavaScript engine (passing all of test262), which now runs page
> scripts (the DOM, events, timers). It has no bookmarks or sandboxing yet. See [PROGRESS.md](PROGRESS.md) for the
> honest details.

## 📑 Contents

- [Screenshots](#-screenshots)
- [Why "BannedBrowser"?](#-why-bannedbrowser)
- [What makes it different](#-what-makes-it-different)
- [Features](#-features)
- [Web compatibility snapshot](#-web-compatibility-snapshot)
- [Architecture](#-architecture)
- [Getting started](#-getting-started)
- [Usage](#-usage)
- [Testing](#-testing)
- [Roadmap](#-roadmap)
- [FAQ](#-faq)
- [Contributing](#-contributing)
- [Acknowledgements](#-acknowledgements)
- [License](#-license)

## 📸 Screenshots

<div align="center">
<img src="docs/screenshots/theoldnet-light.png" alt="theoldnet.com in BannedBrowser" width="860">
<br><sub>theoldnet.com — tables, <code>&lt;font&gt;</code>, <code>&lt;center&gt;</code>, GIFs and form controls, all laid out and painted by BannedBrowser's own engine.</sub>
</div>

| | |
|---|---|
| ![example.com, light](docs/screenshots/example-com-light.png) <br> example.com (light) | ![example.com, dark](docs/screenshots/example-com-dark.png) <br> example.com honouring `color-scheme` + `light-dark()` (dark) |
| ![New tab, light](docs/screenshots/newtab-light.png) <br> New-tab page (rendered by our engine) | ![New tab, dark](docs/screenshots/newtab-dark.png) <br> New-tab page, dark theme |
| ![Private window](docs/screenshots/private-window.png) <br> Private window (dark-violet frame) | ![myip.wtf headers](docs/screenshots/myip-headers.png) <br> https://myip.wtf/headers — what we send |
| ![Frameset](docs/screenshots/frameset.png) <br> A `<frameset>` page after clicking a `target="main"` link | ![Retro test page](docs/screenshots/retro-page.png) <br> Our own 1990s-style test page (original artwork) |
| ![Netscape, 1996](docs/screenshots/archived-netscape-1996.png) <br> netscape.com as archived in Oct 1996 (via theoldnet.com) | ![GeoCities, 1996](docs/screenshots/archived-geocities-1996.png) <br> geocities.com as archived in Oct 1996 (via theoldnet.com) |

<details>
<summary>More: example.com with its JavaScript-inserted translations applied by hand (Arabic RTL, CJK, Cyrillic)</summary>

![multilingual](docs/screenshots/example-com-multilingual-dark.png)

</details>

All screenshots are real captures (`vshot` under Xvfb + Fluxbox, or `--headless --screenshot`). No mock-ups.

## 🐺 Why "BannedBrowser"?

The name is a nod to **Silver Wolf** (银狼, *Yín Láng*) from ***Honkai: Star Rail***, whose
handle is **`@Banned_Player`** ([character wiki](https://honkai-star-rail.fandom.com/wiki/Silver_Wolf)).
She is a member of the Stellaron Hunters and a genius hacker who has mastered "Aether Editing"
to tamper with the data of reality, and who views the universe as a massive immersive
simulation game ([Wikipedia](https://en.wikipedia.org/wiki/Silver_Wolf_(Honkai:_Star_Rail))).
It fits the project's spirit: a browser that *bans* borrowed engines and builds every piece
of the web platform from scratch, just to see if it can.

> *BannedBrowser is an unofficial fan-named project and is not affiliated with, endorsed by,
> or sponsored by HoYoverse. Honkai: Star Rail and Silver Wolf are trademarks of their
> respective owners.* No game art, character images or logos are used anywhere in this project.

## 🧪 What makes it different

Most "new browsers" are a skin on Chromium, Gecko or WebKit. BannedBrowser writes the
web platform itself — and enforces it: [`scripts/check-deps.sh`](scripts/check-deps.sh)
fails the build if `cargo tree` contains html5ever, cssparser, selectors, taffy,
cosmic-text, tiny-skia, url, hyper, reqwest, egui, gtk, servo, boa, … (full list in
[AGENTS.md](AGENTS.md)).

| We wrote it ourselves | We use a crate for it |
|---|---|
| WHATWG URL parser, IDNA, Punycode | Windows & input: `winit`; pixels to screen: `softbuffer` |
| HTTP/1.1 client, keep-alive pool, chunked, redirects, proxies, `data:`/`file:`/`about:` | TLS: `rustls` (ring) + `webpki-roots` |
| HTML tokenizer + tree builder (all insertion modes) | Decompression: `flate2`, `brotli-decompressor` |
| DOM | Text encodings: `encoding_rs` |
| CSS tokenizer, parser, selectors, media queries, cascade, `calc()`, `var()` | Font files/discovery: `fontdb` (+`ttf-parser`) |
| Block/inline/float/table/flex/positioned layout | Shaping: `rustybuzz`; Unicode tables: `unicode-*` |
| Anti-aliased rasterizer, gradients, shadows, clipping, glyph cache | Image codecs: `png`, `zune-jpeg`, `gif`, `image-webp` (BMP/ICO are ours) |
| The browser chrome toolkit and icons | Clipboard: `arboard` |

See [DEPENDENCIES.md](DEPENDENCIES.md) for every crate and why.

## ✨ Features

Legend: ✅ working · 🚧 partial · 📋 planned

<details open>
<summary><b>Browsing, tabs & windows</b></summary>

| Feature | Status |
|---|---|
| Tabs (new, close, middle-click close, reopen closed, Ctrl+Tab, Ctrl+1…9) | ✅ |
| Multiple windows, private windows | ✅ |
| Back / forward / reload / home, URL bar with editing + IME | ✅ |
| Link hover status + pointer cursor, click / Ctrl+click (background tab), `target=_blank` | ✅ |
| Wheel and keyboard scrolling, overlay scrollbar (drag, track paging) | ✅ |
| Favicons, loading throbber + progress line, page title in tab and window | ✅ |
| Light / dark theme following the OS (`--dark` / `--light` override) | ✅ |
| Find in page (Ctrl+F) with highlights and match counter | ✅ |
| ≡ app menu; context menus for pages, links, images, tabs and the URL bar | ✅ |
| About window (version, release name, update check, credits, license, source) | ✅ |
| Certificate error page (Go Back, Advanced…, Accept the Risk and Continue) | ✅ |
| Sidebars | 📋 |
| Session restore, pinned tabs, drag to reorder | 📋 |

</details>

<details>
<summary><b>Bookmarks · History · Downloads · Privacy · Search · Reader View · DevTools</b></summary>

| Feature | Status |
|---|---|
| Bookmarks (toolbar, menu, Library, HTML import/export) | 📋 M9 |
| History with frecency, sidebar, Library | 📋 M9 |
| Downloads manager | 📋 M9 |
| Private browsing (separate in-memory state) | ✅ own cookie jar and memory-only cache; nothing is written to the profile |
| Search engines + keywords | 🚧 non-URL input searches DuckDuckGo |
| Reader View | 📋 M9 |
| View source, DevTools, Browser Console | 📋 M11 |

</details>

<details open>
<summary><b>Web platform</b></summary>

| Area | Status |
|---|---|
| URL Standard | ✅ 100 % WPT urltestdata |
| HTTP/1.1, HTTP/2 (own HPACK and framing), HTTPS, gzip/deflate/br, chunked, redirects, proxies | ✅ |
| Cookies (RFC 6265bis, Public Suffix List, SameSite), saved in the profile | ✅ |
| Forms: typing, checkboxes, radios, drop-downs, buttons, GET/POST submission, Tab navigation | ✅ |
| HTTP cache (RFC 9111: freshness, revalidation, Vary, partitioned by site) | ✅ |
| HTML parsing | ✅ 100 % html5lib tokenizer, 100 % WPT tree construction |
| Legacy presentational attributes, quirks mode | ✅ |
| CSS selectors, cascade, custom properties, `calc()`, media queries | ✅ |
| Colors incl. `light-dark()`, `hsl()`, `hwb()`, legacy color parsing | ✅ |
| Block, inline, floats, margin collapsing | ✅ |
| HTML tables (colspan/rowspan, auto + fixed) | ✅ |
| Flexbox | 🚧 basic |
| Positioning (relative/absolute) | ✅ · fixed/sticky 🚧 |
| Images (PNG, JPEG, GIF, WebP, BMP, ICO), image maps | ✅ (GIF animation 📋) |
| Fonts: system fonts, fallback (CJK, Thai, Arabic, emoji) | ✅ · `@font-face` 📋 |
| Bidi text | 🚧 basic reordering |
| Frames: `<frameset>`, `<iframe>` (src/srcdoc), link targets, per-frame scrolling, frame navigations in history | ✅ |
| SVG | 📋 |
| JavaScript | 🚧 own engine (all of test262) runs page scripts: the DOM, events, timers, `document.write`; fetch/XHR, storage, workers, canvas and iframes' scripts still to come |

</details>

## 🌐 Web compatibility snapshot

| Site | Status |
|---|---|
| example.com | ✅ (static HTML; its JS-added translations need the JS engine) |
| theoldnet.com (start page + lite) | ✅ |
| myip.wtf, myip.wtf/headers | ✅ |
| Archived 1996–99 sites via theoldnet: GeoCities (home + a fan page), Netscape, WinZip, Microsoft IE | ✅ live + offline snapshots |
| Wikipedia, Hacker News | 📋 not tested yet |

| Test suite | Pass rate |
|---|---|
| WPT urltestdata | **904 / 904 (100 %)** |
| html5lib tokenizer | **7046 / 7046 (100 %)** |
| WPT tree construction | **1959 / 1959 (100 %)**, + 14 / 14 `#script-on` |
| Reftests | **50 / 50** |
| test262 parse phase (`language`, `built-ins`, `annexB`) | **48803 / 48803 (100 %)** |
| test262 execution | **48803 / 48803 (100 %)** |
| Unicode NormalizationTest (UAX #15) | **all lines + every other code point** |
| Public Suffix List tests | **78 / 78 (100 %)** |

## 🏗 Architecture

```mermaid
flowchart LR
  N[bb-net<br/>HTTP/TLS] --> E[encoding<br/>sniffing]
  E --> H[bb-html<br/>tokenizer + tree builder]
  H --> D[bb-dom]
  C[bb-css<br/>parser, selectors] --> S[bb-style<br/>cascade]
  D --> S
  S --> L[bb-layout<br/>box tree + layout]
  T[bb-text<br/>fonts + shaping] --> L
  I[bb-image] --> L
  L --> P[bb-paint<br/>display list + rasterizer]
  P --> W[window / PNG]
```

```mermaid
flowchart TB
  UI["UI thread<br/>winit events · chrome (bb-ui) · compositing"]
  T1["Tab worker 1<br/>Page: DOM · style · layout · render"]
  T2["Tab worker N"]
  F["Fetch workers (≤6 per page)<br/>stylesheets · @import · images"]
  UI -- Navigate / Render / Hover / Click --> T1
  T1 -- Loaded / Frame / Hover / Clicked --> UI
  UI --> T2
  T1 --> F
```

| Crate | What it does |
|---|---|
| `bb-str` | WTF-8 strings (web strings that may hold lone surrogates) |
| `bb-url` | WHATWG URL parser & serializer |
| `bb-net` | HTTP/1.1, HTTP/2 (own HPACK), TLS, proxies, content codings, cookies, Public Suffix List, HTTP cache, `data:`/`file:`/`about:`, encoding sniffing |
| `bb-dom` | Arena DOM with dirty flags |
| `bb-html` | HTML tokenizer & tree builder |
| `bb-css` | CSS syntax, selectors, media queries, values |
| `bb-style` | Cascade, computed values, UA stylesheet, presentational hints |
| `bb-text` | Font discovery, fallback, shaping, line breaking |
| `bb-image` | Image decoding |
| `bb-paint` | Display list & software rasterizer |
| `bb-layout` | Box tree, layout, display-list generation, hit testing |
| `bb-engine` | Per-page pipeline, forms, focus |
| `bb-profile` | Profile storage (SQLite): cookies, preferences |
| `bb-ui` | Chrome widget toolkit & icons |
| `bb-js` | JavaScript engine: lexer, parser, early errors, RegExp syntax; bytecode interpreter, GC and built-ins (in progress) |
| `bannedbrowser` | The app: CLI, headless mode, windowed shell |

More in [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

## 🚀 Getting started

**Prebuilt binaries:** see [Releases](https://github.com/NepuOverfl0w/BannedBrowser/releases)
(Windows `.zip`, macOS `.app` zip, Linux tarball). The latest is
[v0.1.0](https://github.com/NepuOverfl0w/BannedBrowser/releases/tag/v0.1.0); it predates forms,
the profile, the HTTP cache and HTTP/2, which are on `main`.

**Build from source** (stable Rust 1.85+):

```bash
cargo run --release -- https://example.com
```

The build downloads the current [Public Suffix List](https://publicsuffix.org/list/) (used for
cookies) with `curl`. Offline, set `BB_PSL_FILE=/path/to/public_suffix_list.dat`, or
`BB_PSL_OFFLINE=1` to build without it; the browser then downloads it at first start.

<details>
<summary>Windows</summary>

Install Rust via [rustup](https://rustup.rs) with the MSVC toolchain (Visual Studio Build
Tools, "Desktop development with C++"), or the GNU toolchain with MinGW-w64. No other
dependencies.

</details>

<details>
<summary>macOS</summary>

Install the Xcode Command Line Tools (`xcode-select --install`) and Rust via rustup.

</details>

<details>
<summary>Linux</summary>

```bash
# Debian / Ubuntu
sudo apt install build-essential pkg-config libxkbcommon-dev libwayland-dev libx11-dev fontconfig fonts-dejavu-core
# Fedora
sudo dnf install gcc pkgconf-pkg-config libxkbcommon-devel wayland-devel libX11-devel fontconfig dejavu-sans-fonts
# Arch
sudo pacman -S base-devel libxkbcommon wayland libx11 fontconfig ttf-dejavu
```

</details>

## 🕹 Usage

```text
bannedbrowser [OPTIONS] [URL]...
  --headless                 Render without a window
  --screenshot <FILE.png>    Write a PNG of the page (implies --headless)
  --window-size <WxH>        Window / viewport size in CSS px (default 1280x800)
  --full-page                Screenshot the whole document
  --scale <N>                Device pixel ratio for screenshots
  --dump-dom | --dump-style | --dump-layout   Print a pipeline stage and exit
  --dark | --light           Force the color scheme
  --extra-ca <FILE.pem>      Trust an extra CA bundle (also honours SSL_CERT_FILE)
  --private-window           Open a private window
  --new-tab <URL>            Open URL in a new tab
  --profile <DIR>            Profile directory (also BB_PROFILE)
```

HTTPS/HTTP proxies are taken from `HTTPS_PROXY`, `HTTP_PROXY`, `ALL_PROXY` and `NO_PROXY`.

| Action | Windows / Linux | macOS |
|---|---|---|
| Focus URL bar | Ctrl+L, Alt+D, F6 | Cmd+L |
| New tab / close tab / reopen closed tab | Ctrl+T / Ctrl+W / Ctrl+Shift+T | Cmd+T / Cmd+W / Cmd+Shift+T |
| New window / private window | Ctrl+N / Ctrl+Shift+P | Cmd+N / Cmd+Shift+P |
| Next / previous tab | Ctrl+Tab / Ctrl+Shift+Tab | Ctrl+Tab / Ctrl+Shift+Tab |
| Go to tab 1…8 / last | Ctrl+1…8 / Ctrl+9 (Alt+1…9) | Cmd+1…8 / Cmd+9 |
| Back / forward | Alt+← / Alt+→ | Cmd+[ / Cmd+] |
| Reload / reload bypassing the cache | Ctrl+R, F5 / Ctrl+Shift+R | Cmd+R / Cmd+Shift+R |
| Home | Alt+Home | Alt+Home |
| Scroll | Space, Shift+Space, PageUp/Down, arrows, Home/End | same |
| Quit | Ctrl+Q | Cmd+Q |

**`about:` pages:** `about:newtab`, `about:home`, `about:blank`, `about:bannedbrowser`, `about:about`.

**Profile location** (cookies, window size; `--profile` or `BB_PROFILE` overrides it): `%APPDATA%\BannedBrowser\profile`, `~/Library/Application Support/BannedBrowser/profile`, `~/.local/share/BannedBrowser/profile` (`$XDG_DATA_HOME`). The HTTP cache and the downloaded Public Suffix List live under the OS cache directory (`BannedBrowser/http-cache`, `BannedBrowser/psl`).

## 🧾 Testing

```bash
scripts/fetch-spec-tests.sh            # html5lib-tests, WPT, test262, PSL tests, entities.json → third_party/
cargo test --workspace                 # unit, spec, integration, robustness, reftests, snapshots
scripts/run-reftests.sh                # reftests only (diff PNGs in tests/reftests/out/)
scripts/gui-smoke.sh                   # GUI tests with real OS input (Linux, Windows, macOS)
scripts/check-deps.sh                  # the no-borrowed-engines rule
```

The Claude Code cloud VM provides `vdisplay-start` (Xvfb `:99` + Fluxbox), `vshot` and
`vwait-window`. GUI tests assert on the JSON the browser writes to `$BB_TEST_STATE`.
Details: [docs/TESTING.md](docs/TESTING.md).

## 🗺 Roadmap

- [x] **v0.1 "Hello, Old Web"** — example.com ✅, theoldnet.com ✅, 5 archived sites ✅, CI on 3 OSes ✅, release packages ✅
- [x] M0 Scaffold
- [x] M1 URL & networking
- [x] M2 HTML
- [ ] M3 CSS & style (mostly done)
- [ ] M4 Layout I (mostly done)
- [ ] M5 Paint (mostly done; 50 reftests)
- [ ] M6 GUI shell core (started)
- [ ] M7 Layout II (grid, transforms, `@font-face`, …)
- [x] M8 Forms, cookies, profile storage, HTTP cache
- [ ] M9 Bookmarks, history, downloads, settings, Reader View, passwords
- [ ] M10 JavaScript engine (parser and interpreter pass all of test262; Web APIs next)
- [ ] M11 DevTools, performance, accessibility, packaging

## ❓ FAQ

**Why not use Servo / a WebView?** Because then it would be someone else's browser. The whole point is to build the platform ourselves.

**Is it safe for daily browsing?** Not yet. There is no sandboxing or process isolation, and no JavaScript yet (safer, but it breaks many sites). Treat it as an experiment.

**Does it run JavaScript?** Yes, with our own engine (`bb-js`, passing all of test262) and Web APIs (`bb-webapi`): scripts can change the DOM, listen to clicks and keys, use timers and `document.write`. Many Web APIs are still missing (fetch, XHR, storage, workers, canvas, scripts in frames), so script-heavy sites may not work yet.

**Why Rust?** Memory safety for parsers that eat hostile input all day, great tooling, and fearless threading for the per-tab pipelines. Every engine crate is `#![forbid(unsafe_code)]`.

**Why does it look like Firefox?** The chrome is inspired by Firefox's Proton design (floating tabs, roomy URL bar, ≡ menu). It uses no Mozilla code, names, icons or artwork and is not affiliated with Mozilla.

## 🤝 Contributing

Read [docs/CONTRIBUTING.md](docs/CONTRIBUTING.md) (setup per OS, how every test works,
good first areas) and the [code of conduct](CODE_OF_CONDUCT.md). Report security problems
privately as described in [SECURITY.md](SECURITY.md). In short: no web-platform crates
(check-deps enforces it), `cargo fmt` + `clippy -D warnings`, cite spec sections, add a
reftest for rendering changes, and use conventional commits (`feat(layout): …`).

## 🙏 Acknowledgements

- The **WHATWG** and **CSSWG** specifications, **html5lib-tests**, **Web Platform Tests** and **test262**.
- **Firefox / Mozilla** for UI inspiration (not affiliated).
- The authors of `winit`, `softbuffer`, `rustls`, `ring`, `fontdb`, `ttf-parser`, `rustybuzz`, `encoding_rs`, the image codecs and the other crates we rely on.
- **Silver Wolf / Honkai: Star Rail** for the name (unofficial fan reference; see above).

## 📄 License

Licensed under either of [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option.
