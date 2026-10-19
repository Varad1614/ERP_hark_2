Test-only RSA keys for the OIDC adapter tests (`oidc-test-key-a.pem` is published in
`oidc-test-jwks.json` as `kid=test-key-a`; key `b` is an untrusted signer / rotation key).
They protect nothing and must never be used outside tests.
