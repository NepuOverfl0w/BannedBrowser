# BannedBrowser architecture

BannedBrowser is a Cargo workspace. Data flows one way through pure-ish stages,
each with a debug dump:

```
bytes ─▶ bb-net ─▶ encoding sniff ─▶ bb-html tokenizer ─▶ tree builder ─▶ bb-dom Document   (--dump-dom)
                                                                              │
     bb-css (stylesheets, selectors, media) ─▶ bb-style cascade ─▶ StyleMap   (--dump-style)
                                                                              │
                          bb-layout box tree ─▶ layout ─▶ fragments/lines   (--dump-layout)
                                                                              │
                         bb-layout paint ─▶ bb-paint DisplayList ─▶ Canvas (SoftwareCanvas) ─▶ PNG / window
```

## Crates

| Crate | Responsibility |
|---|---|
| `bb-str` | WTF-8 strings (`Wtf8Buf`): UTF-8 that can also hold lone surrogates, for web strings; UTF-16 conversion |
| `bb-url` | WHATWG URL parser/serializer, hosts (IPv4/IPv6/IDNA), percent-encoding, form encoding |
| `bb-net` | HTTP/1.1 client with keep-alive pool, HTTP/2 (`h2/`: framing, HPACK, flow control, one multiplexed connection per origin), TLS (rustls), CONNECT proxies, content codings, redirects, `data:`/`file:`/`about:`, encoding sniffing, the HTTP cache (`cache.rs`, RFC 9111), cookies (`cookies.rs`, RFC 6265bis) and the Public Suffix List (`psl.rs`; embedded by `build.rs`, refreshed daily by the shell) |
| `bb-dom` | Arena DOM (`NodeId` indices), dirty flags, html5lib dump, serializer; select/option state (`select.rs`) and form control values, checkedness, form owners (`forms.rs`) |
| `bb-html` | HTML tokenizer (full state machine + entity table) and tree builder (all insertion modes) |
| `bb-css` | CSS Syntax 3 tokenizer/parser, Selectors 4 matching, media queries, `@supports`, colors, `calc()` |
| `bb-style` | Cascade, custom properties, computed values, UA stylesheet, HTML presentational hints |
| `bb-text` | Font discovery (fontdb), CSS font matching + aliases, per-character fallback, shaping (rustybuzz), outlines, UAX #14/#9 helpers, the text editing model shared by the chrome and page form controls (`edit.rs`) |
| `bb-image` | PNG/JPEG/GIF/WebP via allowed codecs; own BMP/ICO decoders; premultiplied RGBA |
| `bb-paint` | Paths, exact-area anti-aliased rasterizer, gradients, clips, layers, blur shadows, glyph cache, display list renderer |
| `bb-layout` | Box tree, block/inline/float/table/flex/positioned layout, list markers, replaced elements, display-list generation, hit testing |
| `bb-engine` | Page pipeline (fetch → … → display list), parallel subresources, internal `about:` pages, error pages, frames (`frames.rs`), focus and interaction with page content (`interact.rs`), form submission (`forms.rs`) |
| `bb-profile` | The profile directory's SQLite database: persistent cookies (saved by a background thread shortly after changes and at exit), prefs (window size); an exclusive lock per session, damaged files moved aside, newer schemas left alone |
| `bb-ui` | Chrome toolkit: theme, original vector icons, text helpers, text-field model |
| `bb-js` | JavaScript engine: lexer, parser with early errors, RegExp syntax; `vm/`: values, GC heap of objects and environments, AST → stack bytecode compiler, interpreter loop, realm and built-ins. Names are resolved at run time through environment records (needed for `with` and sloppy `eval`); the collector runs between instructions of the outermost run only, so native code never holds unrooted references. Several realms can share one heap (each function records its [[Realm]]; calls switch the current one). A few async built-ins are written in JavaScript (`builtins/selfhost.js`), compiled once per thread and handed native helpers so they never touch patchable prototypes; `using` scopes keep their DisposeCapability on the operand stack, and disposal is emitted inline on every exit like `finally`. Modules are evaluated with the spec's cyclic-module algorithm (DFS indices, cycle roots, async parent lists); the host loader returns bytes, and `with { type }` imports become synthetic modules with a `default` export. Strict tail calls (`Op::TailCall`, marked by a pass over the finished bytecode) replace the caller's frame |
| `bannedbrowser` | Binary: CLI, headless mode, winit/softbuffer shell, tab worker threads |

## Threads

```
UI thread (winit event loop) ──── chrome painting (bb-ui → bb-paint), input, URL bar
   │  ▲  crossbeam channels + EventLoopProxy user events
   ▼  │
Tab worker thread × N ──── owns its Page (DOM/styles/layout are !Send), loads, restyles, renders viewport frames
   │
Scoped fetch workers (≤ 6 per page) ──── stylesheets, @imports, images in parallel
```

The UI thread never blocks on network, parsing or layout: it sends `Navigate`,
`Render`, `Hover`, `Click` messages and receives `Loaded`, `Frame`, `Hover`,
`Clicked` events. Render requests are coalesced. Panics in a page pipeline are
caught and replaced with the "BannedBrowser hit a snag" page.

## Key decisions

* **One paint path.** Chrome and content both paint through `bb-paint`; the
  `SoftwareCanvas` is the reference renderer used by all tests. A `wgpu`
  canvas may come later behind a `gpu` feature.
* **Coordinates.** Layout runs in absolute CSS px; relative offsets are applied
  at paint/hit-test time; the renderer applies scroll and device scale.
* **Spec fidelity.** Algorithms cite their spec sections in comments. Partial
  implementations carry `TODO(spec §x)` comments and are listed in `PROGRESS.md`.
* **Incremental later.** DOM nodes carry style/layout dirty flags; today every
  change restyles and relayouts the whole page (fast enough for v0.1).
