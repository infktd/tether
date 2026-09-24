# JWT test fixtures

`test_rsa.pem` and `test_ec.pem` are throwaway keys generated for these
tests only (`openssl genpkey`). They sign fake SSO tokens; `jwks.json` is
their public half in the shape CCP serves at
`https://login.eveonline.com/oauth/jwks` (same key ids and algorithms).
Nothing real is signed with them.
