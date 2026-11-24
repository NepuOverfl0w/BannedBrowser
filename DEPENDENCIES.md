# Dependencies

BannedBrowser writes every web-platform component itself. Third-party crates are
limited to platform glue and narrow utilities. `scripts/check-deps.sh` enforces
the banned list on every CI run.

| Crate | Used by | Why |
|---|---|---|
| `winit` (no default features; x11, wayland) | shell | Cross-platform windows and input events. Default `wayland-csd-adwaita` is disabled because it pulls in `tiny-skia`. |
| `softbuffer` | shell | Presents our CPU-rendered RGBA buffer to the window. |
| `ttf-parser` (via `rustybuzz`'s re-export) | bb-text | Reads font tables and glyph outlines (we rasterize outlines ourselves). |
| `fontdb` | bb-text | Discovers system fonts and matches family/weight/style. |
| `rustybuzz` | bb-text | OpenType text shaping (glyph positioning). |
| `unicode-linebreak`, `unicode-segmentation`, `unicode-bidi`, `unicode-normalization` | bb-text, bb-url | Unicode data tables (UAX #14, #29, #9, NFC/NFKC for IDNA). |
| `png`, `zune-jpeg` (+ its `zune-core` option types), `gif`, `image-webp` | bb-image, bb-paint (PNG output) | Image codecs. BMP and ICO decoders are our own. |
| `rustls` (ring, no default features), `webpki-roots` | bb-net | TLS 1.2/1.3 and the Mozilla root store. `rustls-pki-types` comes with rustls (PEM parsing for extra CAs). |
| `flate2`, `brotli-decompressor` | bb-net | `gzip`/`deflate`/`br` content codings. |
| `encoding_rs` | bb-net, bb-dom, bb-engine, bb-webapi | WHATWG Encoding Standard decoders and encoders (windows-1252, Shift_JIS, …): page decoding, form submission, TextDecoder/TextEncoder. |
| `arboard` | shell | Clipboard. |
| `getrandom` | bb-webapi | The operating system's secure random generator (platform glue, already in the tree through `ring`): `crypto.getRandomValues`/`randomUUID`, blob URL UUIDs, multipart boundaries. |
| `dirs` | shell | Platform cache and data directories (the Public Suffix List cache, the profile). |
| `rusqlite` (`bundled`: SQLite compiled in, no system library) | bb-profile | The profile database (`profile.sqlite`): persistent cookies and prefs, with SQLite's locking and crash-safe transactions. Pulls in `libsqlite3-sys` and small helpers (`hashlink`, `fallible-iterator`, `fallible-streaming-iterator`). |
| `serde_json` | shell, tests | GUI test-state file, spec-suite JSON. (`serde` derive isn't used: prefs are plain SQLite rows.) |
| `log`, `env_logger` | all | Logging. |
| `thiserror`, `anyhow` (binary only) | bb-net, shell | Error types. |
| `smallvec`, `bitflags`, `rustc-hash`, `crossbeam-channel` | various | Small-vector, flags, fast hashing, channels between UI and tab threads (and the tab loop's `select!` over commands and wake-ups). |
| `proptest` (dev) | parsers | Randomized robustness tests. |

Planned (already allowed by the project rules, not yet used): `rfd`, `accesskit`,
`accesskit_winit`, `keyring`, `once_cell`.

## v0.2 decisions (maintainer, 2026-11-21)

Not added yet. Each crate gets a row in the table above, with its exact use, when
it is first used.

| Area | Decision | Boundaries |
|---|---|---|
| GPU / hardware acceleration | **`wgpu`** (+ its `naga`) as platform glue for GPU compositing and rasterization. It may also back the **WebGPU** API exposed to web content. | Our `unsafe` stays in `bb-gpu`, each block with a `// SAFETY:` comment. All painting is our own shaders and display-list code. The CPU rasterizer stays as the fallback and as the reference for reftests. |
| Video decoding | **OS hardware decoders**: Media Foundation (Windows), VideoToolbox (macOS), VA-API (Linux), in the new crate `bb-media-os`, plus **our own pure-Rust VP9 software decoder** as the fallback. | `bb-media-os` uses `unsafe` for the FFI (allowed in every crate since 2026-11-22, AGENTS.md hard rule 3): FFI only, every block documented with `// SAFETY:`. The bindings it uses (e.g. `windows`, `objc2-*`, a VA-API binding) are to be justified here when added. MP4/WebM demuxing and Media Source Extensions are ours. |
| Audio | **`cpal`** for audio device output (WASAPI / CoreAudio / ALSA). | Opus, AAC, MP3 and Vorbis decoders, resampling, and the Web Audio graph are ours. |

## JavaScript JIT (maintainer, 2026-11-21, session 2)

| Area | Decision | Boundaries |
|---|---|---|
| JS JIT | Allowed, in the new crate **`bb-js-jit`**. | `unsafe` only for mapping executable memory and entering generated code, each block with a `// SAFETY:` comment. The code generator itself is ours. Any crate it needs for the OS memory calls is justified here when added. `bb-js`'s interpreter stays `unsafe`-free (bb-js's only `unsafe` is asking Windows' ICU for the time zone) and every test must pass with the JIT off. |

## GPU crate (maintainer, 2026-11-22, session 2)

| Area | Decision | Boundaries |
|---|---|---|
| GPU, WebGPU, WebGL | The new crate **`bb-gpu`** hosts the `wgpu` device and surfaces, GPU compositing, and the GPU side of WebGPU and WebGL 1/2. | Every `unsafe` block has a `// SAFETY:` comment; the crate exposes a safe API. WebGL's API, state machine and validation are ours; shaders are translated with `naga`. Crates it needs beyond `wgpu` are justified here when added. |

## Data files

| File | Source and licence | Used by |
|---|---|---|
| Public Suffix List (not in the repository) | <https://publicsuffix.org/list/public_suffix_list.dat>, MPL-2.0, unmodified. `crates/bb-net/build.rs` downloads it with the system `curl` and embeds it (`BB_PSL_FILE=<path>` uses a local copy, `BB_PSL_OFFLINE=1` embeds none); the browser downloads it again at first start and then at most once a day into its cache directory (`dirs::cache_dir()/BannedBrowser/psl`), as publicsuffix.org asks. The notice is on `about:bannedbrowser`. | `bb-net` cookies and same-site checks (`psl.rs`) |
| `crates/bb-js/src/vm/tzdata.txt` | Generated by `scripts/gen-tzdata.py` from the IANA time zone database (<https://www.iana.org/time-zones>, tzdata 2026e; public domain). The release's Zone and Link lines, compiled with its own `zic`, re-encoded compactly; the data itself is unchanged. | `bb-js` Temporal time zones and Date local time (`vm/tz.rs`) |
