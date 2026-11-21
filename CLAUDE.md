# CLAUDE.md — working rules for BannedBrowser

BannedBrowser is a Firefox-inspired desktop browser written **from scratch in Rust**
(Windows, macOS, Linux). Every web-platform component is ours. Read `PROGRESS.md`
and `docs/ARCHITECTURE.md` at the start of every session.

## Hard rules
1. **No borrowed engines or web-spec crates.** Banned list lives in
   `scripts/check-deps.sh` (html5ever, cssparser, selectors, taffy, cosmic-text,
   tiny-skia, url, hyper, reqwest, http, egui, iced, gtk, servo*, boa*, …).
   Run it before every commit. New crates must be listed in `DEPENDENCIES.md`
   with a justification and must not implement a web standard, layout,
   painting, text layout or a GUI toolkit.
2. **Allowed crates only** (platform glue): winit, softbuffer, ttf-parser,
   fontdb, rustybuzz, unicode-*, png/zune-jpeg/gif/image-webp, rustls(ring)+
   webpki-roots, flate2, brotli-decompressor, encoding_rs, arboard, rfd, dirs,
   accesskit, keyring, rusqlite(bundled), serde(+json), log/env_logger,
   thiserror, anyhow (binary only), smallvec, bitflags, rustc-hash,
   crossbeam-channel, once_cell; dev-only: image, proptest.
3. `#![forbid(unsafe_code)]` in every engine crate (workspace lint forbids it everywhere).
4. **Never panic on web content.** Every parser gets randomized robustness tests.
   Tabs catch panics and show "BannedBrowser hit a snag".
5. UI thread never blocks on network/parse/layout.
6. Engine crates contain no OS-specific code; OS code lives in `crates/bannedbrowser`
   behind `#[cfg(target_os = ...)]`. Cmd on macOS, Ctrl elsewhere. HiDPI via `scale_factor()`.
7. Original branding only — no Firefox/Mozilla or Honkai: Star Rail assets.
8. Cite spec sections above non-trivial algorithms (`// HTML §13.2.6.4.7 "in body"`).
9. Never fake progress: no stubs claiming support, no loosened/skipped tests,
   no hard-coded expectations. Partial = say so in PROGRESS.md.

## Before marking anything done
```
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
scripts/check-deps.sh
```
Also: `cargo clippy --target x86_64-pc-windows-gnu` and `cargo check --target aarch64-apple-darwin`.

## Environment (Claude Code cloud VM)
- `vdisplay-start && export DISPLAY=:99` (Xvfb + Fluxbox); `vshot out.png [title]`; `vwait-window BannedBrowser`.
- Long builds: run in background, poll the log.
- Spec suites: `scripts/fetch-spec-tests.sh` → `third_party/` (git-ignored).
- HTTPS goes through `HTTPS_PROXY` with a MITM CA in `SSL_CERT_FILE`; bb-net honours both.
- web.archive.org is blocked by the sandbox egress policy (archived-site images won't load here).
- theoldnet.com returns "Hello World" to non-browser user agents; use a `Mozilla/5.0` UA.

## Workflow
- Small conventional commits (`feat(html): …`, `fix(net): …`, `test: …`, `docs: …`).
- End of session: update PROGRESS.md (status, numbers, gaps, next steps), commit, push
  to the session branch.
- Stuck 3 times on the same failure → write down attempts in PROGRESS.md, move on.
- Look at rendering output yourself (headless `--screenshot` + Read the PNG).
