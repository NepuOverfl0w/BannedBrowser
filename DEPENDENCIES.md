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
| `encoding_rs` | bb-net, bb-engine | WHATWG Encoding Standard decoders (windows-1252, Shift_JIS, …). |
| `arboard` | shell | Clipboard. |
| `dirs` (planned, M8) | shell | Platform profile directory. |
| `serde_json` | shell, tests | GUI test-state file, spec-suite JSON. (`serde` derive comes with prefs in M8.) |
| `log`, `env_logger` | all | Logging. |
| `thiserror`, `anyhow` (binary only) | bb-net, shell | Error types. |
| `smallvec`, `bitflags`, `rustc-hash`, `crossbeam-channel` | various | Small-vector, flags, fast hashing, channels between UI and tab threads. |
| `proptest` (dev) | parsers | Randomized robustness tests. |

Planned (already allowed by the project rules, not yet used): `rfd`, `accesskit`,
`accesskit_winit`, `keyring`, `rusqlite` (bundled), `once_cell`, `wgpu` (behind a
`gpu` feature).
