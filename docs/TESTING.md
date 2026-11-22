# Testing BannedBrowser

| Layer | Command | Notes |
|---|---|---|
| Unit tests | `cargo test --workspace` | every crate |
| Spec suites | `scripts/fetch-spec-tests.sh` then `cargo test -p bb-url --test wpt_urltestdata -- --nocapture` and `cargo test -p bb-html --test html5lib -- --nocapture` and `cargo test -p bb-net --test psl -- --nocapture` and `cargo test -p bb-js --test test262 -- --nocapture` | html5lib-tests tokenizer (HEAD), WPT tree-construction `.dat` files (`html/syntax/parsing/resources`, pinned to a WPT commit in the script), WPT `urltestdata.json`, the Public Suffix List's `test_psl.txt` (HEAD) against the list the build embedded, test262 `language`/`built-ins`/`annexB` (pinned; parse phase for now, failures in `target/test262-parse-failures.txt`) |
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
