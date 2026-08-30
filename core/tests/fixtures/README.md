# Test-only RSA fixtures

`test_only_rsa_private.pem` / `test_only_jwks.json` are a throwaway RSA-2048
keypair used ONLY by tests (unit tests and the E2E harness) to mint RS256
tokens and serve a JWKS. They are intentionally committed. Never reuse them
for any real issuer.
