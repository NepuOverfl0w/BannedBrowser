#!/usr/bin/env bash
# Serve tests/pages on http://127.0.0.1:8000 and https://127.0.0.1:8443
# (self-signed; trust with `--extra-ca tests/certs/ca.pem`).
cd "$(dirname "$0")/.."
exec python3 tests/server/testserver.py --port "${PORT:-8000}" --https-port "${HTTPS_PORT:-8443}" "$@"
