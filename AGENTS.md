# AGENTS.md: working rules for BannedBrowser

These are the canonical rules for every coding agent (Codex, Claude, …) and human working
on this repository. `CLAUDE.md` only adds notes for Claude's cloud VM.

BannedBrowser is a Firefox-inspired desktop browser written **from scratch in Rust** for
Windows, macOS and Linux. Every web-platform component is ours.

## Start here

1. Read `PROGRESS.md`: the status, the test numbers, the honest gap list, and the
   **"v0.2 scope"** section (what v0.2 must deliver, and the decisions already made).
2. Read `docs/ARCHITECTURE.md`: the crates and how data flows through them.
3. Read `docs/TESTING.md` and `DEPENDENCIES.md`.

**Current state:**
- v0.1.0 "Hello, Old Web" is released:
  https://github.com/imm0nv1nhtv/BannedBrowser/releases/tag/v0.1.0
- `main` is the development branch.
- **v0.2 is next.** It covers M3–M11 in full, plus:
  - HTTP/2 and HTTP/3
  - WebExtensions parity: uBlock Origin, Privacy Badger, SponsorBlock on youtube.com, SteamDB on store.steampowered.com
  - video playback, GPU acceleration, and the certificate-error page buttons
- v0.2 is verified against https://www.whatismybrowser.com. See PROGRESS.md for the details.

## Hard rules

1. **No borrowed engines or web-spec crates.**
   - The banned list lives in `scripts/check-deps.sh`: html5ever, cssparser, selectors, taffy,
     cosmic-text, tiny-skia, url, hyper, reqwest, http, egui, iced, gtk, servo\*, boa\*, js-sys, ….
     Run it before every commit.
   - New crates must be listed in `DEPENDENCIES.md` with a justification.
   - New crates must not implement a web standard, layout, painting, text layout or a GUI toolkit.
   - **Decided exception (v0.2):** `wgpu` (with its `naga`) is allowed as the GPU API, and it may
     also back WebGPU for web content. Our own shaders still do all painting.
2. **Allowed crates only** (platform glue):
   - winit, softbuffer, ttf-parser, fontdb, rustybuzz, unicode-\*.
   - png/zune-jpeg/gif/image-webp, rustls (ring) + webpki-roots, flate2, brotli-decompressor, encoding_rs.
   - arboard, rfd, dirs, accesskit, keyring, rusqlite (bundled), serde (+json).
   - log/env_logger, thiserror, anyhow (binary only), smallvec, bitflags, rustc-hash, crossbeam-channel, once_cell.
   - For v0.2:
     - wgpu (GPU).
     - cpal (audio *device output* only; all audio/video codecs and Web Audio are ours).
     - The OS media FFI bindings used by `bb-media-os`, each justified in DEPENDENCIES.md when added.
   - Dev-only: image, proptest.
3. **No `unsafe`.**
   - The workspace lint is `unsafe_code = "forbid"` and every crate inherits it.
   - **Single exception:** `crates/bb-media-os`, the FFI to OS hardware video decoders (Media
     Foundation, VideoToolbox, VA-API). It doesn't inherit the workspace lints. Every `unsafe`
     there needs a `// SAFETY:` comment directly above it.
   - `scripts/check-deps.sh` enforces all of this.
   - Video must still play without that crate: our own pure-Rust VP9 decoder is the fallback.
4. **Never panic on web content.** Every parser gets randomized robustness tests. Tabs catch
   panics and show "BannedBrowser hit a snag".
5. **The UI thread never blocks** on network, parsing or layout. Each tab runs its `Page` on a
   worker thread (`crates/bannedbrowser/src/tab.rs`); `Page` is not `Send` (it uses `Rc`).
6. **Engine crates contain no OS-specific code.**
   - OS code lives in `crates/bannedbrowser`, and OS video decoding in `crates/bb-media-os`,
     behind `#[cfg(target_os = ...)]`.
   - Use Cmd on macOS and Ctrl elsewhere. Get HiDPI from `scale_factor()`.
7. **Original branding only:**
   - No Firefox/Mozilla names, logos, icons or code.
   - No Honkai: Star Rail art, logos or fonts.
   - The name is a fan nod to Silver Wolf's handle `@Banned_Player`. Keep the disclaimers in
     the README and `about:bannedbrowser`.
8. **Cite spec sections** above non-trivial algorithms (`// HTML §13.2.6.4.7 "in body"`).
9. **Never fake progress.**
   - No stubs claiming support, no loosened or skipped tests, no hard-coded expectations.
   - When something is partial, say so in PROGRESS.md.
10. **v0.2 testing bar:**
    - **No excluded test cases, and every test must pass.**
    - The pass-rate floors in the suite tests (URL 95 %, html5lib tokenizer 98 %, tree
      construction 90 %) go to 100 %.
    - PROGRESS.md lists the current exclusions and known failures to remove.

## Before marking anything done

```
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
scripts/check-deps.sh --self-test && scripts/check-deps.sh
```

Also run these cross-target checks:
- `cargo clippy --target x86_64-pc-windows-gnu -p bannedbrowser -- -D warnings`
- `cargo check --target aarch64-apple-darwin` (crates that link `ring` need the macOS SDK; CI covers them).

For rendering changes, run `scripts/run-reftests.sh` and look at the output yourself:
render with `bannedbrowser --headless --screenshot out.png URL` and inspect the PNG.

## Repository map

| Path | What |
|---|---|
| `crates/bb-url` | WHATWG URL Standard |
| `crates/bb-net` | HTTP/1.1, TLS, proxies, codings, `data:`/`file:`/`about:`, encoding sniffing (HTTP/2 and HTTP/3 go here in v0.2) |
| `crates/bb-dom`, `bb-html` | DOM; HTML tokenizer and tree builder |
| `crates/bb-css`, `bb-style` | CSS parsing and selectors; cascade, computed values, UA sheet, presentational hints |
| `crates/bb-text` | fonts, fallback, shaping, line breaking and bidi helpers |
| `crates/bb-image`, `bb-paint` | image decoding; rasterizer, display list, glyph cache |
| `crates/bb-layout` | box tree, layout, paint (display-list building), hit testing, find-in-page |
| `crates/bb-engine` | the `Page` pipeline, frames (`frames.rs`), internal pages |
| `crates/bb-js` | placeholder for our own JS engine (M10) |
| `crates/bb-ui` | chrome widget toolkit, themes, original icons, text field |
| `crates/bannedbrowser` | the binary: CLI, headless mode, GUI (`gui.rs`), tab workers (`tab.rs`) |
| `resources/` | UA stylesheets, internal pages (`about:*`, error pages), app icons |
| `tests/` | reftests, snapshot pages, GUI tests, test HTTP(S) server and certs, retro/frames test pages |
| `scripts/` | check-deps, spec-suite fetch, reftests, GUI smoke, packaging, archived-site snapshots |

## Tests

- **Spec suites** (fetched into `third_party/`, which is git-ignored):
  - Run `scripts/fetch-spec-tests.sh`, then `cargo test -p bb-url --test wpt_urltestdata -- --nocapture`
    and `cargo test -p bb-html --test html5lib -- --nocapture`.
- **Reftests:**
  - `tests/reftests/NAME.html` carries `<link rel="match" href="NAME-ref.html">`.
  - Run them with `cargo test -p bb-engine --test reftests`.
  - A new reftest should fail without your fix; check that.
- **Snapshot regressions:**
  - The pages are in `tests/pages/snapshots/`; the approved PNGs are in `approved/`.
  - The comparison only runs when the installed fonts match `approved/FONTS.txt` (fixing that is a v0.2 item).
  - Re-approve with `BB_UPDATE_SNAPSHOTS=1 cargo test -p bb-engine --test snapshots`, but **only
    after viewing every changed PNG**.
- **GUI tests:**
  - `scripts/gui-smoke.sh` runs `tests/gui/*.sh` under Xvfb + a window manager, driven with xdotool.
  - The tests assert on the JSON state file the browser writes when `BB_TEST_STATE` is set.
  - Packages they need: `xvfb fluxbox xdotool wmctrl imagemagick libxkbcommon-x11-0`.
  - Use `key_until` (in `tests/gui/lib.sh`) only for idempotent keys.
- **Archived sites:**
  - `scripts/archived-sites.sh` (also the CI workflow "Archived sites") renders the sites listed in
    `tests/pages/archived-sites.txt` live and snapshots them.
  - It needs a network that can reach theoldnet.com and web.archive.org.
- **Network tests:** `cargo test -p bb-net --test http_integration`. They spawn
  `tests/server/testserver.py` (HTTP + HTTPS with a test-only CA in `tests/certs/`).

## Workflow

- Small conventional commits: `feat(html): …`, `fix(net): …`, `test: …`, `docs: …`, `ci: …`.
- Follow the maintainer's instructions on branches and pull requests. `main` is the default branch.
- At the end of a session:
  1. Update PROGRESS.md: status, numbers, gaps, next steps.
  2. Commit and push.
- Stuck three times on the same failure? Write down what you tried in PROGRESS.md and move on.
- Don't put model or agent identifiers in code, commits or docs.

## Releasing

1. Bump `version` in the root `Cargo.toml`. The User-Agent, `--version` and `about:bannedbrowser`
   follow it.
2. In `CHANGELOG.md`, turn the `## [Unreleased]` section into a dated
   `## [X.Y.Z] — "Name" — YYYY-MM-DD` section. The release workflow reads that exact heading.
   Between releases, record changes under `[Unreleased]` as you go, and keep the
   version at `X.Y.0-dev`.
3. Publish in either of two ways:
   - Push a `vX.Y.Z` tag.
   - Push a commit whose message contains **`[release]`**. GitHub creates the tag when it
     publishes the release.
4. CI then runs the tests on all three OSes, builds the Linux `.tar.gz`, Windows `.zip` and macOS
   `.app` zip (stripped `dist` profile), and creates the GitHub Release with the CHANGELOG section as notes.
5. `[package]` in a commit message test-builds the packages without releasing.
6. Pre-release versions (`-dev`) and versions that already have a release are refused.
7. To move an existing release to another commit on `main` (e.g. after a history rewrite),
   run the **"Re-publish release"** workflow manually with the tag and the full commit SHA.
   It checks that the commit is on `main` and that its `Cargo.toml` has that version,
   rebuilds the packages from that commit, and replaces the release and tag.

## Environment notes and lessons learned

- **Proxies:** bb-net honours `HTTPS_PROXY`/`HTTP_PROXY`/`NO_PROXY` and an extra CA bundle from
  `SSL_CERT_FILE` or `--extra-ca`.
- **theoldnet.com:**
  - It returns "Hello World" to non-browser user agents.
  - It redirects modern browsers to web.archive.org.
  - Its `/get?…&url=<image>` proxy re-encodes binary data as UTF-8, so it's unusable for images.
- **Linux GUI runtime:** winit `dlopen`s `libxkbcommon-x11` at runtime. Without it, the window never appears.
- **macOS cross-check:** `cargo check --target aarch64-apple-darwin` can't build `ring` without
  the macOS SDK; CI's macOS job covers it.
- **Text:**
  - A word split across inline elements (`D<font>OWNLOAD</font>`) must be fit-checked as one unit.
  - Text boxes are never out-of-flow, even when their parent floats.
- **Fonts:** CI installs `fonts-noto-cjk`. Tests that need a specific script must check coverage
  first (`FontSystem::any_face_covers`) instead of assuming a font.
