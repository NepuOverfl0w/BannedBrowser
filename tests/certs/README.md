# Test-only TLS certificates

**Never trust these outside tests.** `ca.pem` is a throwaway certificate
authority whose private key is committed on purpose; `server.pem`/`server.key`
is a leaf for `localhost` / `127.0.0.1` signed by it. The test HTTPS server
(`tests/server/testserver.py --https-port`) uses the leaf, and tests pass
`--extra-ca tests/certs/ca.pem` (or `ClientConfig::extra_ca_files`) to trust it.

Regenerate with `./generate.sh` (valid for 20 years).
