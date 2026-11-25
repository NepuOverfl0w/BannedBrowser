# Testing BannedBrowser

| Layer | Command | Notes |
|---|---|---|
| Unit tests | `cargo test --workspace` | every crate |
| Spec suites | `scripts/fetch-spec-tests.sh` then `cargo test -p bb-url --test wpt_urltestdata -- --nocapture` and `cargo test -p bb-html --test html5lib -- --nocapture` and `cargo test -p bb-net --test psl -- --nocapture` and `cargo test -p bb-js --test test262 -- --nocapture` and `cargo test -p bb-js --test test262_exec -- --nocapture` (`TEST262_FILTER=substring` narrows it) | html5lib-tests tokenizer (HEAD), WPT tree-construction `.dat` files (`html/syntax/parsing/resources`, pinned to a WPT commit in the script), WPT `urltestdata.json`, the Public Suffix List's `test_psl.txt` (HEAD) against the list the build embedded, test262 `language`/`built-ins`/`annexB` (pinned; parse phase at 100 %, execution with a ratcheting floor, failures in `target/test262-parse-failures.txt` and `target/test262-exec-failures.txt`) |
| Web API tests | `cargo test -p bb-webapi --test wpt -- --nocapture` (after `scripts/fetch-spec-tests.sh`) | WPT testharness.js tests from the directories in `DIRS` in `crates/bb-webapi/tests/wpt.rs` (`dom/` nodes, collections, events, lists, abort, ranges and traversal, `domparsing/`, `FileAPI/`, `xhr/formdata/`, `workers/`, `html/webappapis/structured-clone/`, `webmessaging/`, `html/canvas/element/`, `url/`, `encoding/`, `fetch/api/headers/`, `fetch/api/request/`, `fetch/api/response/`, `fetch/api/body/`, `css/geometry/`, `custom-elements/`, `streams/`, `shadow-dom/`, `html/semantics/interactive-elements/the-details-element/`, `html/semantics/forms/constraints/`), served from the checkout at `http://web-platform.test/`. `.any.js` tests run in a window and in a dedicated worker (`X.any.js#worker`). Queued tasks and messages run before any time passes; animation frames run every 16 ms of virtual time; otherwise a virtual clock skips to the next timer (real time while workers run). idlharness tests read WPT's `interfaces/*.idl` (`/resources/WebIDLParser.js` is served from `resources/webidl2/lib/webidl2.js`, as wptserve does). Prints per-directory subtest numbers; failures go to `target/wpt-failures.txt`. `WPT_FILTER=dom/nodes/Node-` narrows, `WPT_VERBOSE=1` prints panics and console output, `WPT_THREADS=n` sets parallelism. Reftests, `-expected` files and `non-automated` tests are not run here. `cargo test -p bb-webapi --test tree_script_on` runs the `#script-on` tree-construction cases |
| Page scripts | `cargo test -p bb-engine --test scripts` | whole pages with scripts through the engine: DOM changes relayout, events, fetch, storage, workers, canvases painted |
| Network integration | `cargo test -p bb-net --test http_integration` and `--test h2_integration` | spawn `tests/server/testserver.py` (HTTP + HTTPS with a test-only CA; HTTP/2 via `tests/server/h2server.py`, which needs `pip install -r tests/server/requirements.txt`, the Python `h2` package). The HTTP cache tests use its `/cache` endpoint |
| Reftests | `scripts/run-reftests.sh` | `tests/reftests/*.html` with `<link rel="match">`; diffs land in `tests/reftests/out/` |
| Robustness | part of `cargo test` | proptest / seeded random input for URL, HTML, CSS, HTTP decoding, images |
| Headless screenshots | `bannedbrowser --headless --screenshot out.png --window-size 1024x768 URL` | look at them! |
| GUI | `scripts/gui-smoke.sh` | real OS input: xdotool (Linux, Xvfb + Fluxbox), SendInput (Windows), CGEvent (macOS); asserts on `BB_TEST_STATE` JSON |

## Writing a reftest

Create `tests/reftests/my-test.html`:

```html
<!DOCTYPE html><style>body{margin:0}</style>
<link rel="match" href="my-test-ref.html">
...feature under test...
```

and `my-test-ref.html` rendering the same pixels with simpler means. Optional
`<meta name="fuzzy" content="maxDifference=8;totalPixels=20">` loosens the
comparison for anti-aliasing differences (keep it tight).

## Cloud VM helpers

`vdisplay-start` (Xvfb `:99` + Fluxbox), `vshot out.png [title]`,
`vwait-window <title> [timeout]`. Export `DISPLAY=:99`.
