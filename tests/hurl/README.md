# Hurl smoke tests

End-to-end checks of every page and endpoint against a running stack
behind Caddy. CI runs them in the `docker` job on amd64 and arm64 right
after `deploy/install.sh localhost`.

They expect a **fresh** instance: `04_setup_flow.hurl` walks the wizard's
first steps. Locally:

```bash
docker compose -f deploy/docker-compose.yml down -v   # wipes local data
deploy/install.sh localhost
hurl --test --insecure --jobs 1 --variable base=https://localhost \
  --variable setup_token="$(sed -n 's/^SETUP_TOKEN=//p' deploy/.env)" tests/hurl/*.hurl
```

Behind another proxy (`deploy/install.sh --proxy none|nginx|traefik`),
point `base` at the domain that proxy serves; the tests send
`Origin: {{base}}`, so it must match `https://DOMAIN`. For a proxy on
another local port, keep `base=https://localhost` and add
`--connect-to localhost:443:127.0.0.1:<port>`.

Setup unlock is rate-limited to 5 attempts a minute per IP and the suite
makes two, so a quick third run in the same minute hits a 429.

Signed-in flows need a real EVE SSO login and are covered by the Rust
integration tests (`crates/web/tests`) with a fake SSO provider instead.
