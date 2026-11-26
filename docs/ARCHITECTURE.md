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

Scripts sit beside the pipeline: `bb-webapi` gives each page a Window on a `bb-js` VM; the HTML
parser pauses at each `</script>` to run it, and DOM changes made by scripts mark the page dirty,
so the next frame restyles and relays it out.

## Crates

| Crate | Responsibility |
|---|---|
| `bb-str` | WTF-8 strings (`Wtf8Buf`): UTF-8 that can also hold lone surrogates, for web strings; UTF-16 conversion |
| `bb-url` | WHATWG URL parser/serializer, hosts (IPv4/IPv6/IDNA), percent-encoding, form encoding |
| `bb-net` | HTTP/1.1 client with keep-alive pool, HTTP/2 (`h2/`: framing, HPACK, flow control, one multiplexed connection per origin), TLS (rustls), CONNECT proxies, content codings, redirects, `data:`/`file:`/`about:`, encoding sniffing, the HTTP cache (`cache.rs`, RFC 9111), cookies (`cookies.rs`, RFC 6265bis) and the Public Suffix List (`psl.rs`; embedded by `build.rs`, refreshed daily by the shell) |
| `bb-dom` | Arena DOM (`NodeId` indices), dirty flags, html5lib dump, serializer; select/option state (`select.rs`), form control values, checkedness, form owners (`forms.rs`), the focused element and focusable areas (`focus.rs`), constraint validation (`validity.rs`; `pattern` and URL checks are hooks the host installs, as bb-dom has no JS or URL parser), and shadow trees (`shadow.rs`: a shadow root is a parentless DocumentFragment the document maps to its host; slot assignment is computed from the tree; `flat_children` gives the flat tree style and layout walk) |
| `bb-html` | HTML tokenizer (full state machine + entity table) and tree builder (all insertion modes; declarative shadow roots from `<template shadowrootmode>` where the document allows them). `TreeBuilder::pump` returns a `Pause`: after a `</script>`, before a start tag that one of the host's custom element definitions names (the token is held while the host runs the constructor and appends the attributes, then the parser uses the host's element), and after that element is inserted |
| `bb-xml` | XML 1.0 + Namespaces parser into bb-dom (internal DTD subset: entities, attribute defaults and types; no external entities; depth and expansion limits), the XML fragment parsing algorithm, and the DOM Parsing XML serializer |
| `bb-css` | CSS Syntax 3 tokenizer/parser, Selectors 4 matching (with CSS Scoping's `:host`, `:host()`, `:host-context()` and `::slotted()`), media queries, `@supports`, colors, `calc()` |
| `bb-style` | Cascade, custom properties, computed values, UA stylesheet, HTML presentational hints; styles the flat tree, with each sheet scoped to its tree (a shadow tree's sheets style that tree, its host through `:host` and its slotted elements through `::slotted()`, ordered by encapsulation context) |
| `bb-text` | Font discovery (fontdb), CSS font matching + aliases, per-character fallback, shaping (rustybuzz), outlines, UAX #14/#9 helpers, the text editing model shared by the chrome and page form controls (`edit.rs`) |
| `bb-image` | PNG/JPEG/GIF/WebP via allowed codecs; own BMP/ICO decoders; premultiplied RGBA |
| `bb-paint` | Paths, exact-area anti-aliased rasterizer (nonzero and even-odd), gradients, clips, layers, blur shadows, glyph cache, display list renderer; `canvas2d.rs`: the canvas 2D context's drawing model (state stack, stroker, gradients, patterns, compositing and blending, shadows, images, text, SVG path data) |
| `bb-layout` | Box tree, block/inline/float/table/flex/positioned layout, list markers, replaced elements, display-list generation, hit testing |
| `bb-engine` | Page pipeline (fetch → … → display list), parallel subresources, internal `about:` pages, error pages, frames (`frames.rs`; a scripted page's frames render their frame windows' documents, lent back to the windows while scripts run), focus and interaction with page content (`interact.rs`), form submission (`forms.rs`), the page's script host (`script.rs`: the `bb-webapi` Embedder that fetches scripts, answers style and layout queries, loads script-inserted images and scripts, and turns input into DOM events), canvas bitmaps handed to layout as replaced content |
| `bb-profile` | The profile directory's SQLite database: persistent cookies and `localStorage` (saved by a background thread shortly after changes and at exit), prefs (window size); an exclusive lock per session, damaged files moved aside, newer schemas left alone |
| `bb-ui` | Chrome toolkit: theme, original vector icons, text helpers, text-field model |
| `bb-js` | JavaScript engine: lexer, parser with early errors, RegExp syntax; `vm/`: values, GC heap of objects and environments, AST → stack bytecode compiler, interpreter loop, realm and built-ins. Names live in environment records (needed for `with` and sloppy `eval`), but the compiler mirrors each scope's bindings in order (`ScopeInfo`) and resolves identifiers to (hops, slot) pairs where no dynamic scope is in the way; the VM checks the name at that slot and falls back to lookup by name. Property access has per-site inline caches, objects keep integer keys from 0 densely, and strings are ropes flattened on first read; the collector runs between instructions of the outermost run only, so native code never holds unrooted references. Several realms can share one heap (each function records its [[Realm]]; calls switch the current one). A few async built-ins are written in JavaScript (`builtins/selfhost.js`), compiled once per thread and handed native helpers so they never touch patchable prototypes; `using` scopes keep their DisposeCapability on the operand stack, and disposal is emitted inline on every exit like `finally`. Modules are evaluated with the spec's cyclic-module algorithm (DFS indices, cycle roots, async parent lists); the host's `loader` hook (given the VM, so it can find the current realm's host) resolves and fetches, its `resolver` hook backs `import.meta.resolve` and `import()`'s up-front resolution, and `host_enqueue_task` lets `import()` load in a host task; a module record keeps its map key and its base URL apart (inline modules get unique keys); the host loader returns bytes, and `with { type }` imports become synthetic modules with a `default` export. Strict tail calls (`Op::TailCall`, marked by a pass over the finished bytecode) replace the caller's frame. Source text is Rust `char`s; eval of a string with lone surrogates maps them to an unused private-use block that the lexer maps back in literals (`lexer::encode_source`). Unicode normalization (`vm/normalize.rs`) uses tables generated by `scripts/gen-unicode-norm.py`; time zones (`vm/tz.rs`) use `tzdata.txt`, made by `scripts/gen-tzdata.py` from an IANA tzdata release compiled with `zic` (each zone's offsets, transitions and its POSIX TZ rule for later instants). Temporal reads it through native helpers in whole seconds; Date's LocalTime and UTC use the system zone (`tz::system_zone`: `TZ`, `/etc/localtime`, or Windows' `icu.dll`), which a host can override with `Vm::set_time_zone` |
| `bb-webapi` | Web APIs over bb-js and bb-dom (milestone E): platform objects are `Kind::Host` objects (an interface id from `class.rs` plus a payload such as a (document, node) pair) whose prototypes follow WebIDL; one wrapper per node, cached in the host and traced through `Vm::host_trace`. Live collections, NamedNodeMap and DOMStringMap are WebIDL legacy platform objects through bb-js's `HostExotic` hooks. Nodes live in their document's bb-dom arena, so adopting a node into another document copies it there and repoints its wrapper. `loader.rs` drives the HTML parser with the scripting flag on: `TreeBuilder::pump` stops after each `</script>` and around each defined custom element's start tag, the script or constructor runs with the document lent back to the host, and `document.write` inserts at the tokenizer's insertion point. Every DOM mutation goes through the spec's algorithms in `tree.rs` (insert, remove, replace, replace all, adopt) and `mutation.rs` (attribute changes, replace data), which queue mutation records for MutationObserver and keep live Ranges (`range.rs`) and NodeIterators (`traversal.rs`) valid; nodes the parser inserted get their records after each pump. Dedicated workers (`worker.rs`) each get a thread with their own bb-js VM and Host (a `Window` built with the worker scope: only worker-exposed interfaces, a DedicatedWorkerGlobalScope global); messages cross threads as bb-js `Serialized` structured clones over channels, a shared counter tells the parent's `has_tasks`, and the embedder's waker (`Embedder::waker`, a thread-local the tab sets) wakes the tab loop; workers fetch through `Embedder::worker_net` (the page's `Send` client). The Window's named properties (elements by id) come from a WindowProperties exotic object between Window.prototype and EventTarget.prototype. MessageChannel ports and BroadcastChannel (`port.rs`) deliver through the same task queues. Shared workers (`shared_worker.rs`): a process-wide manager (a static list under a mutex) finds a running SharedWorkerGlobalScope by constructor origin, URL and name, or starts one; connections arrive on its thread as port ends to receive, and each connecting window holds a token whose last drop lets the worker close. Frames (`frames.rs`): each iframe or frame of a page gets a window of its own, a Host in a realm of the page's VM (bb-js keeps a host per realm, tags platform objects with their realm, refuses to unwrap another realm's, and runs a dropped realm's code no more); the top Window's event loop runs every window's tasks in its realm, and windows of other origins are reached only through cross-origin wrapper objects. Other modules: `net.rs` (fetch, XHR, Headers, AbortController; requests run on background threads through the page's client), `storage.rs` (Web Storage, `document.cookie`), `xml.rs` (DOMParser, XMLSerializer over `bb-xml`), `blob.rs` (Blob, File, FileReader, FormData, blob URLs, crypto), `canvas.rs` (the `<canvas>` element and CanvasRenderingContext2D over `bb-paint::canvas2d`; each change bumps a generation counter so the engine repaints only changed canvases), `selection.rs`, `range.rs`, `traversal.rs`, `ce.rs` (custom elements: the registry, upgrades and reactions, entered around the mutation algorithms), `streams.rs` with `streams.js` (the Streams Standard in JavaScript, installed per realm on first use), `details.rs` (the details toggle event and name groups), `focus.rs` (focus()/blur(), activeElement, the focus events; the embedder syncs its own focus with the document's), `internals.rs` (ElementInternals, form-associated custom elements and their callbacks, CustomStateSet), `shadow.rs` (attachShadow, ShadowRoot, HTMLSlotElement's assignment API, slotchange signals from each slot's last-seen assigned nodes, getHTML/setHTMLUnsafe), `importmap.rs` (import map parsing, merging and module specifier resolution, with each window's resolved module set; `loader.rs` registers `<script type=importmap>` and prefetches parser-inserted module graphs at prepare time). Events dispatch with DOM §2.9's event path (shadow-adjusted targets, retargeted `target` and `relatedTarget`, `composedPath()` hiding closed trees) and capture, target and bubble passes, with a microtask checkpoint after each listener called from a task; event handler attributes compile on first use. The embedder supplies script fetching and the clock (`Embedder`) and drives the event loop (`Window::run_tasks`) |
| `bannedbrowser` | Binary: CLI, headless mode, winit/softbuffer shell, tab worker threads (each tab's loop waits on its command channel and a wake channel that timers, network replies and workers use) |

## Threads

```
UI thread (winit event loop) ──── chrome painting (bb-ui → bb-paint), input, URL bar
   │  ▲  crossbeam channels + EventLoopProxy user events
   ▼  │
Tab worker thread × N ──── owns its Page (DOM/styles/layout are !Send) and its script Window, loads, runs
   │                          scripts and their event loop, restyles, renders viewport frames
   ├── Scoped fetch workers (≤ 6 per page) ──── stylesheets, @imports, images in parallel
   ├── Script request threads ──── fetch() / XHR / script and image loads started by scripts
   └── Web worker threads (≤ 64 per process) ──── one bb-js VM each; messages as structured clones
```

The UI thread never blocks on network, parsing or layout: it sends `Navigate`,
`Render`, `Hover`, `Click` messages and receives `Loaded`, `Frame`, `Hover`,
`Clicked` events. Render requests are coalesced. Panics in a page pipeline are
caught and replaced with the "BannedBrowser hit a snag" page.

## Key decisions

* **One paint path.** Chrome and content both paint through `bb-paint`; the
  `SoftwareCanvas` is the reference renderer used by all tests. GPU compositing
  (`wgpu`, in a planned `bb-gpu` crate) comes in milestone G, with the software
  renderer kept as the reference.
* **Coordinates.** Layout runs in absolute CSS px; relative offsets are applied
  at paint/hit-test time; the renderer applies scroll and device scale.
* **Spec fidelity.** Algorithms cite their spec sections in comments. Partial
  implementations carry `TODO(spec §x)` comments and are listed in `PROGRESS.md`.
* **Incremental later.** DOM nodes carry style/layout dirty flags; today every
  change restyles and relayouts the whole page (fast enough for v0.1).
