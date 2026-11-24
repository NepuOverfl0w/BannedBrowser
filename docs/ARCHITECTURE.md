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
| `bb-xml` | XML 1.0 + Namespaces parser into bb-dom (internal DTD subset: entities, attribute defaults and types; no external entities; depth and expansion limits), the XML fragment parsing algorithm, and the DOM Parsing XML serializer |
| `bb-css` | CSS Syntax 3 tokenizer/parser, Selectors 4 matching, media queries, `@supports`, colors, `calc()` |
| `bb-style` | Cascade, custom properties, computed values, UA stylesheet, HTML presentational hints |
| `bb-text` | Font discovery (fontdb), CSS font matching + aliases, per-character fallback, shaping (rustybuzz), outlines, UAX #14/#9 helpers, the text editing model shared by the chrome and page form controls (`edit.rs`) |
| `bb-image` | PNG/JPEG/GIF/WebP via allowed codecs; own BMP/ICO decoders; premultiplied RGBA |
| `bb-paint` | Paths, exact-area anti-aliased rasterizer, gradients, clips, layers, blur shadows, glyph cache, display list renderer |
| `bb-layout` | Box tree, block/inline/float/table/flex/positioned layout, list markers, replaced elements, display-list generation, hit testing |
| `bb-engine` | Page pipeline (fetch → … → display list), parallel subresources, internal `about:` pages, error pages, frames (`frames.rs`), focus and interaction with page content (`interact.rs`), form submission (`forms.rs`) |
| `bb-profile` | The profile directory's SQLite database: persistent cookies (saved by a background thread shortly after changes and at exit), prefs (window size); an exclusive lock per session, damaged files moved aside, newer schemas left alone |
| `bb-ui` | Chrome toolkit: theme, original vector icons, text helpers, text-field model |
| `bb-js` | JavaScript engine: lexer, parser with early errors, RegExp syntax; `vm/`: values, GC heap of objects and environments, AST → stack bytecode compiler, interpreter loop, realm and built-ins. Names live in environment records (needed for `with` and sloppy `eval`), but the compiler mirrors each scope's bindings in order (`ScopeInfo`) and resolves identifiers to (hops, slot) pairs where no dynamic scope is in the way; the VM checks the name at that slot and falls back to lookup by name. Property access has per-site inline caches, objects keep integer keys from 0 densely, and strings are ropes flattened on first read; the collector runs between instructions of the outermost run only, so native code never holds unrooted references. Several realms can share one heap (each function records its [[Realm]]; calls switch the current one). A few async built-ins are written in JavaScript (`builtins/selfhost.js`), compiled once per thread and handed native helpers so they never touch patchable prototypes; `using` scopes keep their DisposeCapability on the operand stack, and disposal is emitted inline on every exit like `finally`. Modules are evaluated with the spec's cyclic-module algorithm (DFS indices, cycle roots, async parent lists); the host loader returns bytes, and `with { type }` imports become synthetic modules with a `default` export. Strict tail calls (`Op::TailCall`, marked by a pass over the finished bytecode) replace the caller's frame. Source text is Rust `char`s; eval of a string with lone surrogates maps them to an unused private-use block that the lexer maps back in literals (`lexer::encode_source`). Unicode normalization (`vm/normalize.rs`) uses tables generated by `scripts/gen-unicode-norm.py`; time zones (`vm/tz.rs`) use `tzdata.txt`, made by `scripts/gen-tzdata.py` from an IANA tzdata release compiled with `zic` (each zone's offsets, transitions and its POSIX TZ rule for later instants). Temporal reads it through native helpers in whole seconds; Date's LocalTime and UTC use the system zone (`tz::system_zone`: `TZ`, `/etc/localtime`, or Windows' `icu.dll`), which a host can override with `Vm::set_time_zone` |
| `bb-webapi` | Web APIs over bb-js and bb-dom (milestone E): platform objects are `Kind::Host` objects (an interface id from `class.rs` plus a payload such as a (document, node) pair) whose prototypes follow WebIDL; one wrapper per node, cached in the host and traced through `Vm::host_trace`. Live collections, NamedNodeMap and DOMStringMap are WebIDL legacy platform objects through bb-js's `HostExotic` hooks. Nodes live in their document's bb-dom arena, so adopting a node into another document copies it there and repoints its wrapper. `loader.rs` drives the HTML parser with the scripting flag on: `TreeBuilder::pump` stops after each `</script>`, the script runs with the document lent back to the host, and `document.write` inserts at the tokenizer's insertion point. Every DOM mutation goes through the spec's algorithms in `tree.rs` (insert, remove, replace, replace all, adopt) and `mutation.rs` (attribute changes, replace data), which queue mutation records for MutationObserver and keep live Ranges (`range.rs`) and NodeIterators (`traversal.rs`) valid; nodes the parser inserted get their records after each pump. The Window's named properties (elements by id) come from a WindowProperties exotic object between Window.prototype and EventTarget.prototype. Events dispatch with capture, target and bubble passes; event handler attributes compile on first use. The embedder supplies script fetching and the clock (`Embedder`) and drives the event loop (`Window::run_tasks`) |
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
