# Synthetic loopback TLS fixture

These are test-only certificates, unrelated to Slack or any deployed service.
`ca.der` is the explicit test CA; `server.der` is its signed leaf for IP127.0.0.1
with serverAuth usage, valid2026-10-22 through2036-10-19. `server-key.der` is the
synthetic PKCS#8 fixture key, intentionally public test data. The issuer private
key was discarded after generation. Never use these files in production.

The actual worker fixture uses ordinary certificate and server-name validation
with this single CA supplied explicitly. Production uses its WebPKI root set;
no invalid-certificate or hostname-verification bypass is installed.
Generated using OpenSSL req/x509/pkcs8, RSA2048, SHA256, CA:TRUE/keyCertSign issuer,
CA:FALSE/digitalSignature/keyEncipherment/serverAuth leaf and IP SAN127.0.0.1.
