# Contributing to BannedBrowser

Thanks for helping build a browser from scratch! All project rules are in
[`AGENTS.md`](../AGENTS.md) (they apply to people and coding agents alike). This guide gets you
set up and explains how changes are tested. Please also read the
[code of conduct](../CODE_OF_CONDUCT.md). Security problems go through
[SECURITY.md](../SECURITY.md), not public issues.

## The rules in one minute

- **No borrowed engines.** We write the web platform ourselves: URL, HTTP, HTML, DOM, CSS,
  layout, painting, text layout and the UI toolkit. Crates are only for platform glue.
  `scripts/check-deps.sh` enforces the banned list, and every new crate needs a justification
  in [`DEPENDENCIES.md`](../DEPENDENCIES.md).
- **No `unsafe`.** The only exception is `crates/bb-media-os` (OS video decoder FFI).
- **Never panic on web content.** Every parser gets randomized robustness tests.
- **Cite the spec** above non-trivial algorithms: `// HTML §13.2.6.4.7 "in body"`.
- **Never fake progress.** No skipped, excluded or loosened tests, and no hard-coded
  expectations. If something is partial, say so in `PROGRESS.md`.

## Setup

You need stable Rust via [rustup](https://rustup.rs); `rust-toolchain.toml` picks the
toolchain, plus Python 3 for the test server and spec scripts.

**Linux** (Debian/Ubuntu names; other distros have equivalents):

```bash
sudo apt-get install -y build-essential pkg-config libxkbcommon-dev libxkbcommon-x11-0 \
  libwayland-dev libx11-dev libxcursor-dev libxrandr-dev libxi-dev fontconfig \
  fonts-dejavu-core fonts-liberation2 fonts-noto-core fonts-noto-cjk
```

`libxkbcommon-x11-0` is needed at runtime: without it the window never appears. For the GUI
tests you also need:

```bash
sudo apt-get install -y xvfb fluxbox xdotool wmctrl imagemagick
```

**Windows:** install rustup with the MSVC toolchain, plus the *Visual Studio Build Tools*
(C++ workload; `ring` compiles a little C). Nothing else.

**macOS:** install the Xcode Command Line Tools (`xcode-select --install`).

## Build and run

```bash
cargo run -p bannedbrowser -- https://example.com
```

Some useful flags:
- `--dark` / `--light` force the color scheme.
- `--private-window` opens a private window.
- `--headless --screenshot out.png URL` renders a page to a PNG without a window.
- `--dump-dom`, `--dump-style` and `--dump-layout` print the pipeline's intermediate results.
- `--user-agent STR` overrides the User-Agent.
- `RUST_LOG=debug` gives a detailed log.

## Tests

Run these before every pull request:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
scripts/check-deps.sh --self-test && scripts/check-deps.sh
```

| Layer | How | Notes |
|---|---|---|
| Unit and robustness tests | `cargo test --workspace` | Every crate. |
| Spec suites | `scripts/fetch-spec-tests.sh`, then `cargo test -p bb-url --test wpt_urltestdata -- --nocapture` and `cargo test -p bb-html --test html5lib -- --nocapture` | WPT `urltestdata` and html5lib-tests, downloaded into `third_party/`. |
| Network | `cargo test -p bb-net --test http_integration` | Starts `tests/server/testserver.py`, which serves HTTP and HTTPS. Its CA in `tests/certs/` is test-only. |
| Reftests | `scripts/run-reftests.sh` | `tests/reftests/NAME.html` + `<link rel="match" href="NAME-ref.html">`. Diffs go to `tests/reftests/out/`. |
| Snapshot regressions | `cargo test -p bb-engine --test snapshots` | Saved real pages compared with `tests/pages/snapshots/approved/`. Rendered with only the bundled fonts in `tests/fonts/`, so results are the same on every OS. |
| GUI | `scripts/gui-smoke.sh` | Linux with Xvfb. Each test drives the real window with xdotool and checks the state file the browser writes when `BB_TEST_STATE` is set. |

### Writing a reftest

Every rendering fix needs a reftest that **fails without your change**:

```html
<!DOCTYPE html>
<link rel="match" href="my-test-ref.html">
<style>body { margin: 0 }</style>
…the feature under test…
```

`my-test-ref.html` renders the same pixels by simpler means. Then look at the actual output
yourself with `--headless --screenshot`.

### Updating snapshots

Run `BB_UPDATE_SNAPSHOTS=1 cargo test -p bb-engine --test snapshots` only after looking at
every changed image in `tests/pages/snapshots/out/`. Say in the pull request why they changed.

## Pull requests

- Keep them small and focused.
- Use conventional commit messages: `feat(layout): …`, `fix(net): …`, `test(html): …`, `docs: …`.
- Update `PROGRESS.md` (status, numbers, gaps) and add a line under `[Unreleased]` in
  `CHANGELOG.md`.
- The PR template has the checklist. CI runs everything on Linux, Windows and macOS.

## Good places to start

These are self-contained pieces from the gap list in `PROGRESS.md`, each testable with reftests:

- **Tables:** `border-collapse: collapse` conflict resolution; captions outside the table border.
- **Inline layout:** inline-block baseline (should be the *last* line); `font-variant: small-caps`.
- **CSS:** counters (`counter-reset`, `counter-increment`, `counter()`); `lab()`/`lch()`/`oklch()`/`color-mix()`;
  `background-origin`/`background-clip`.
- **Images:** GIF animation.
- **Frames:** frame navigations in session history.
- **Find in page:** search inside frames.

Bigger areas (JavaScript, HTTP/2 and HTTP/3, extensions, video) are the v0.2 plan; check
`PROGRESS.md` and open an issue to coordinate before starting one of those.
