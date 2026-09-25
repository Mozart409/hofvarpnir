# otel-tls test fixtures

Test-only PKI for `tests/otel_export.rs`, standing in for step-ca:

- `ca.pem` — self-signed root ("hofvarpnir test root CA"). The child process
  gets it as `SSL_CERT_FILE`, its only trust anchor.
- `leaf.pem` / `leaf.key` — server certificate for `localhost` / `127.0.0.1`,
  signed by that root. Valid for 100 years.

The CA's private key was deleted after signing, so nothing else can be issued
from this root. The leaf key only ever protects a loopback test listener.
