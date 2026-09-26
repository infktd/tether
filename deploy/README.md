# Deploying Tether

```bash
deploy/install.sh alliance.example.com
```

That writes `deploy/.env` (the domain and generated secrets, once), starts
the stack and prints the setup token for the browser wizard. Nothing else
runs by hand. Re-running it is safe: secrets are never replaced, and the
reverse proxy settings only change when you pass `--proxy` or another proxy
flag. `deploy/install.sh --help` lists the flags; with a terminal attached,
it asks for what's missing.

## Choosing a reverse proxy

Something must serve `https://DOMAIN` and pass requests to the app. Pick
one with `--proxy` (asked on a first install with a terminal attached):

| `--proxy` | TLS and certificates | Containers | App reachable at |
| --- | --- | --- | --- |
| `caddy` (default) | Bundled Caddy, Let's Encrypt, automatic | app, db, caddy | Caddy only (not published) |
| `nginx` | Your nginx on the host, your certificates (certbot) | app, db | `127.0.0.1:TETHER_PORT` |
| `traefik` | Your Traefik in Docker, its certificate resolver | app, db | Traefik's Docker network |
| `none` | Your own proxy, your certificates | app, db | `127.0.0.1:TETHER_PORT` |

The choice lives in `deploy/.env` as `TETHER_PROXY`, with `COMPOSE_FILE`
adding `docker-compose.host-proxy.yml` (nginx, none) or
`docker-compose.traefik.yml` (Traefik) to `docker-compose.yml`. Those
overrides turn Caddy off and publish the app on 127.0.0.1 or attach it to
Traefik's network. Editing `.env` by hand works too; the top of
`docker-compose.yml` shows the lines.

Run `docker compose` from this directory (`cd deploy`), as install.sh does:
`COMPOSE_FILE` in `.env` only applies there, and `up` with
`-f deploy/docker-compose.yml` would start Caddy instead of your proxy.
`exec`, `logs` and `run` work either way.

Switching later: `deploy/install.sh --proxy <choice>`. It rewrites only the
proxy lines, removes the Caddy container when leaving Caddy (it would hold
ports 80 and 443), and restarts the app. When leaving nginx it prints how
to take Tether's server block out of nginx; it doesn't do that itself.

With nginx, Traefik or your own proxy, the certificates are that proxy's
business, outside Tether: Tether itself talks only to the hosts in
CLAUDE.md's Opsec list, and only Caddy, when you use it, talks to Let's
Encrypt.

### caddy (default)

1. Point DNS at the server and open ports 80 and 443.
2. `deploy/install.sh alliance.example.com`
3. Open `https://alliance.example.com/` and enter the setup token.

Caddy gets and renews the certificate by itself.

### nginx

For a server that already runs nginx on ports 80 and 443.

1. Point DNS at the server.
2. `deploy/install.sh --proxy nginx alliance.example.com` (`--port` picks
   the app's port on 127.0.0.1; 8080 by default).
3. The script writes `deploy/nginx-alliance.example.com.conf`. Run as root
   (or with `--install-nginx`), it offers to copy it to
   `/etc/nginx/sites-available/` (or `conf.d/`), link it into
   `sites-enabled/`, run `nginx -t` and reload nginx. Otherwise it prints
   those commands. An existing copy that differs (certbot edited it, say)
   is left alone.
4. TLS is yours. With no certificate yet the block listens on HTTP only and
   answers everything with 403 (so the setup token is never sent in the
   clear), while certbot's challenge still gets through. Get one:
   `sudo certbot --nginx -d alliance.example.com`, which adds HTTPS to the
   block and turns HTTP into a redirect. When a certificate already exists
   in `/etc/letsencrypt/live/<domain>/`, the block is written with it.
5. Open `https://alliance.example.com/` and enter the setup token.

The server block sets:

- `Host`, `X-Forwarded-Proto`, `X-Forwarded-Host`, and `X-Forwarded-For`
  set to `$remote_addr`, replacing anything the client sent;
- for `/notifications/stream` (server-sent events): `proxy_buffering off`,
  `proxy_cache off`, `proxy_read_timeout 1h`;
- `client_max_body_size 41m`, for app packages (up to 40 MiB plus a
  signature) uploaded on the Apps page;
- no security headers: Tether sends Content-Security-Policy,
  Strict-Transport-Security, X-Frame-Options, X-Content-Type-Options and
  Referrer-Policy itself, and adding them in nginx would duplicate them.

### traefik

For Traefik already running in Docker with its Docker provider.

1. Point DNS at the server.
2. `deploy/install.sh --proxy traefik --traefik-network <network>
   --traefik-certresolver <resolver> alliance.example.com`
   - `--traefik-network`: the Docker network Traefik is attached to
     (`docker network ls`); it must exist.
   - `--traefik-certresolver`: the certificate resolver's name in Traefik's
     static configuration (`certificatesResolvers.<name>`).
   - `--traefik-entrypoint`: the HTTPS entry point (default `websecure`).
3. Open `https://alliance.example.com/` and enter the setup token.

The app joins that network with labels for a router named `tether`
(``Host(`DOMAIN`)``, TLS with the resolver) and a service on port 8080.
Nothing is published on the host. Traefik streams server-sent events and
has no body size limit; its entry point's `respondingTimeouts.readTimeout`
(60 s by default in Traefik v3) bounds how long an upload may take. Traefik
replaces a client's `X-Forwarded-For` unless its `forwardedHeaders` trust
that client. Two Tether instances behind one Traefik need different router
names: edit `docker-compose.traefik.yml`.

Every container on that network can reach the app directly, from a private
address, so Tether believes their `X-Forwarded-For`. Keep only trusted
containers on it; at worst one could dodge the setup wizard's rate limit,
which guards a 256-bit token.

### none

Your own proxy, of any kind.

1. `deploy/install.sh --proxy none alliance.example.com` (`--port` as for
   nginx).
2. The app listens on `http://127.0.0.1:TETHER_PORT` (8080 by default).
   Point your proxy at it.
3. Open `https://alliance.example.com/` and enter the setup token.

What the proxy must do:

- Serve `https://DOMAIN` with a valid certificate. The public URL is always
  `https://DOMAIN`: EVE SSO's callback, cookies (HTTPS-only, `__Host-`)
  and the same-origin check on forms all depend on it.
- Set `X-Forwarded-For` to the client's address: replace the header, or
  append to it so the client's address is the last entry. Tether reads the
  last entry, and only from a loopback or private address (your proxy,
  through Docker's port forwarding); from a public address it uses the
  connection's own. It uses the address to rate-limit the setup wizard.
- Pass server-sent events on `/notifications/stream` unbuffered (Tether
  sends `X-Accel-Buffering: no`, which nginx honours), with a read timeout
  above 5 s (keep-alives come every 5 s; a stream lasts up to an hour).
- Allow request bodies up to 41 MiB (app uploads on `/admin/plugins`).
- Add no security headers of its own; Tether sends them.
- Stay the only way in: keep the app on 127.0.0.1, as the override does.

## Checking an install

```bash
docker compose -f deploy/docker-compose.yml exec app tether doctor
```

`doctor` knows the proxy from `TETHER_PROXY`: it names who terminates TLS,
checks ports 80 and 443 and HTTPS against the public URL, and fits its
fixes to the proxy (port 80 is only required with Caddy).
