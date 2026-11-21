#!/usr/bin/env bash
# Render every reftest pair headless and compare pixels (fuzzy).
# Failures leave test/ref/diff PNGs in tests/reftests/out/.
set -euo pipefail
cd "$(dirname "$0")/.."
cargo test -p bb-engine --test reftests -- --nocapture "$@"
