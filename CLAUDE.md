# CLAUDE.md

Instructions for Claude Code working in this repository. Read this file first, every session.

## What this is

A free, self-hosted EVE Online alliance platform: EVE SSO login, alliance access control, Discord role sync, and sandboxed WASM plugins that never touch ESI tokens. It replaces SeAT and Alliance Auth for New Miner's Union (NMU) and a small multiboxing corp. It will never charge real money.

## Sources of truth

- `docs/PRD.md`: what to build, in what order, and acceptance criteria. **Wins on any conflict.**
- `docs/ARCHITECTURE.md`: how the pieces fit together.
- `docs/DESIGN.md`: the look and feel. Every page and plugin follows it; extend it before building anything it doesn't cover.
- This file: how to work.

If the docs disagree with each other or with reality, stop and ask. Do not silently pick one.

## The top rule

A fresh `docker compose up` must produce a working instance with **zero shell commands** afterward. No manual migrate, collectstatic, create-user or setup scripts. Anything that needs a manual step is a bug. This rule exists because Alliance Auth's install failed in exactly those places.

## How to work

- Work one checklist item at a time from `docs/PRD.md`, in order.
- Before writing code for an item, state a short plan: files you will touch, tests you will add.
- Every item ends with: tests passing, `cargo fmt`, `cargo clippy -- -D warnings` clean, one commit with a descriptive message, and the checkbox ticked in `docs/PRD.md`.
- Keep commits small and focused. Never mix refactors with features.
- After each milestone, stop and summarize what was built, what was deferred, and anything surprising. Wait for approval before starting the next milestone.
- When unsure about a product decision, ask. When unsure about an implementation detail, pick the simplest option and note it in the commit message.

## Subagents

Project subagents live in `.claude/agents/`. Use them to keep the main context clean.

- **explorer** (read-only): use it for dependency research: how eve-esi-client, its generated ESI client, wasmtime, sqlx and other crates work, with exact paths and signatures. Don't read large crate sources or generated code in the main context.
- **security-reviewer** (read-only): run it on every task that touches auth, sessions, tokens, secrets, plugins or outbound network calls, before committing. Write the task's diff to a file (`git diff > <scratch>/task.diff`, plus `git diff --cached` and new files if needed) and pass the path with a one-line description of the task. Fix every finding, or justify it explicitly in the commit message.

## Ask before

- Adding any new dependency crate or npm package. Say what it is for and why an existing one won't do.
- Adding any new outbound network destination (see Opsec).
- Changing a public API shape, a WIT interface, or a migration that has already been committed.
- Anything that adds a container, a service, or a manual setup step.

## Opsec

Allowed outbound destinations, and nothing else:

- ESI (`esi.evetech.net`) and EVE SSO (`login.eveonline.com`)
- CCP's image server (`images.evetech.net`)
- Discord API and gateway
- GitHub (`github.com`, `api.github.com`, release asset hosts) for plugin installs and update checks, which admins can switch off
- Let's Encrypt (ACME), from Caddy only, for TLS certificates, when Caddy is the chosen proxy. No other CA. With the admin's own nginx, Traefik or other proxy (`install.sh --proxy`), certificates are that proxy's business, outside Tether, and nothing in our stack contacts a CA

All HTTP from Tether's own code goes through `tether_net::Outbound`, which refuses any host or port not in `tether_net::ALLOWED` (checked before sending and at DNS; redirects are off unless a client opts in, and then stay on the list). Adding a host there is adding an outbound destination: ask first. `ALLOWED` stays core-only.

Plugin hosts are separate and per instance: a plugin's `capabilities.http` hosts are reachable only once that instance's admin approved them at install (again on upgrade if they change), each plugin through its own `Outbound` allowed exactly its approved hosts on 443 (`crates/web/src/plugin_http.rs`). Plugins can't declare Tether's own destinations, every request is logged, and `doctor` lists the approved hosts. A first-party plugin declaring a new host is still a new destination for this project: ask first (Jay approved `zkillboard.com` for Ship Replacement). eve-esi-client takes its HTTP clients from Tether (`Outbound::library_client`), so its ESI and SSO requests are checked against the list like everything else. twilight is the one library with its own connections; its Discord endpoint is checked against the list. `doctor` verifies the configured endpoints.

Build and deploy sources are separate. They're what the image build, CI and the admin's install fetch, never destinations of the running app. The notable ones:

- crates.io, the Rust toolchain (`static.rust-lang.org`), and Docker Hub for base images (`rust`, `debian`, `timescale/timescaledb`, `caddy`, the `docker/dockerfile` frontend)
- Debian's apt mirrors, and apt.postgresql.org, only while building the image, for `postgresql-client-16`
- GitHub releases of pinned, checksum-verified CI tools (Tailwind CLI, Hurl)
- GitHub's container registry (`ghcr.io`), approved by Jay: CI publishes the app image there, and Docker pulls it at install and upgrade
- `raw.githubusercontent.com` and `api.github.com`, from the admin's shell only: the deploy files for an install without a clone, and the newest release number when install.sh first pins the image (Jay asked for the install without a clone, 2026-09-26)

Inbound, the app's port is reachable only from the reverse proxy: Caddy's Docker network, Traefik's, or 127.0.0.1 on the host (never published on a public address). Tether believes X-Forwarded-For only from loopback and private peers (`crates/web/src/ratelimit.rs`).

No telemetry, no analytics, no CDNs, no Google Fonts. Fonts, icons and JS are bundled into the build. The one exception is dev-only tooling (such as Scalar at `/docs`), which may load from a CDN because it is compiled out of release builds. Keep this list in sync with the `doctor` checks and PRD requirement N5.

## Rust conventions

- Stable toolchain. Cargo workspace; crates live in `crates/`.
- `thiserror` for errors in library crates. `anyhow` only in the `server` and `cli` binaries.
- No `unwrap()` or `expect()` outside tests, except for provably impossible cases with a comment explaining why.
- sqlx with compile-time checked queries. Commit offline data (`cargo sqlx prepare --workspace`) so builds work without a database.
- `tracing` for all logging with structured fields. No `println!` outside the CLI's user-facing output.
- Secrets and tokens never appear in logs, errors or panic messages. Wrap them in a type whose `Debug` redacts.
- Every JSON API endpoint is documented with utoipa; every page and endpoint is covered by at least one Hurl test.
- Prefer boring, explicit code over clever abstractions.

## UI conventions

- Server-rendered HTML with askama templates in `templates/`. No SPA framework and no npm.
- Components are Basecoat classes, themed with the tokens in `docs/DESIGN.md`. Basecoat's CSS is vendored in `assets/vendor/`; update it deliberately, never pull it at runtime.
- Interactivity is htmx attributes plus server endpoints returning HTML fragments. Live updates use server-sent events. Hand-written JavaScript only when htmx genuinely can't do it, and it must be small and bundled.
- CSS is built with Tailwind's standalone CLI binary.
- Plugin pages are declarative descriptions rendered by the host's templates. Plugins never ship HTML, CSS or JavaScript to the browser.

## Build speed

- Split code across workspace crates so changes recompile narrowly.
- Linux: configure `mold` in `.cargo/config.toml` if installed. Other platforms: use the default or `lld`.
- The Cranelift backend requires a nightly toolchain, so it is optional and must never be required to build.
- Use `cargo check` while iterating; full builds only to run.

## Testing

- `#[sqlx::test]` for anything that touches the database; each test gets a fresh database.
- Test code uses the unchecked `sqlx::query(...)`, never the `query!` macros: `cargo sqlx prepare` doesn't cache queries that only exist in `#[cfg(test)]` code, so CI's offline build fails on them. Before pushing, check offline from scratch: touch the sources, then `SQLX_OFFLINE=true cargo clippy --workspace --all-targets` with `.env` moved aside.
- ESI is mocked with `wiremock` using recorded response fixtures in `tests/fixtures/esi/`. Tests never call real ESI or Discord.
- `tokio::time::pause()` for scheduler and job timing tests.
- `insta` snapshots for API response shapes.
- Hurl files in `tests/hurl/` for end-to-end smoke tests against a running stack.
- The snapshot and rollback tests (`crates/cli/tests/rollback.rs`) need Postgres 16's client tools (pg_dump, pg_restore, psql): `TETHER_TEST_PG_BIN`, PATH, or failing those, the ones in the running dev database container (through `docker exec`). With `CI` set, missing tools fail them instead of skipping.
- A `dev-login` Cargo feature creates fixture sessions for manual testing. It must be impossible to enable in release builds; add a test or CI check that proves it.

## Common commands

Keep this section updated as the project grows.

```bash
docker compose -f deploy/docker-compose.dev.yml up -d   # dev/test database on 127.0.0.1:5433
echo DATABASE_URL=postgres://tether:tether@127.0.0.1:5433/tether > .env
cargo install sqlx-cli --version 0.9.0 --locked --no-default-features --features postgres,rustls --root tools
tools/bin/sqlx migrate run
tools/bin/cargo-sqlx sqlx prepare --workspace   # after changing any query; commit .sqlx/
cargo check --workspace
cargo run -p tether-server --features dev   # /dev/login fixtures, Scalar at /docs and installing apps from a .zip (debug builds only)
cargo test --workspace
cargo test -p tether-web --test it groups::   # the web tests are one binary (crates/web/tests/it); filter by module
cargo clean   # safe any time; target/ grows with every feature set and toolchain, and a fresh test build is ~4 GB
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
cargo clippy -p hello-plugin -p moon-mining -p member-audit -p fleet-activity-tracking -p structure-timers -p hr-applications -p structures -p ship-replacement -p tether-plugin-sdk --target wasm32-wasip2 -- -D warnings   # plus the tether-plugins-test-guest* crates
cargo build -p hello-plugin --target wasm32-wasip2 --release   # the plugin tests build their guests themselves
scripts/package-plugin.sh plugins/moon-mining ~/.minisign/tether.key   # first-party plugins (plugins/*) -> dist/<id>-<version>.zip + .minisig
scripts/bundle-apps.sh dist/apps   # every plugins/* app, unsigned, as the image bundles them (deploy/Dockerfile)
BUNDLED_APPS_DIR=dist/apps cargo run -p tether-server --features dev   # offer them under "Included with Tether"
scripts/css.sh    # Tailwind standalone CLI (pinned, checksum-verified) -> static/app.css; commit the output
deploy/install.sh localhost    # writes deploy/.env once, pins the published image (newest release, else :edge), pulls it and starts the stack (bundled Caddy)
deploy/install.sh --build localhost   # build the image from this clone instead (tether:local, docker-compose.build.yml); what CI's install test does
deploy/install.sh --version 1.2.0   # move the pin to another published image (X.Y.Z, X.Y, latest, edge, sha-<commit>), pull and restart: upgrades
deploy/install.sh --proxy none localhost   # or nginx/traefik: the admin's own proxy, no Caddy; app on 127.0.0.1:8080 (deploy/README.md)
(cd deploy && docker compose pull && docker compose up -d)   # honours COMPOSE_FILE in deploy/.env (proxy and build overrides; a --build install rebuilds on up); `up -f deploy/docker-compose.yml` would not
git tag v1.2.0 && git push origin v1.2.0   # release: CI publishes ghcr.io/<owner>/tether:1.2.0, :1.2, :latest after its checks; then a GitHub release (deploy/README.md, Releasing)
docker compose -f deploy/docker-compose.yml exec app tether doctor   # also: users, states, jobs, sync
docker compose -f deploy/docker-compose.yml exec app tether rollback --list   # snapshots and nightly backups
docker compose -f deploy/docker-compose.yml stop app && docker compose -f deploy/docker-compose.yml run --rm app rollback   # restore core's pre-migration snapshot (asks first; --plugin <id>, --snapshot <name>, --yes)
SKIP_MIGRATION_SNAPSHOT=true cargo run -p tether-server --features dev   # locally, without Postgres 16's pg_dump, when a new migration is pending
# Hurl runs against a fresh stack (the setup flow expects no owner yet):
hurl --test --insecure --jobs 1 --variable base=https://localhost --variable setup_token="$(sed -n 's/^SETUP_TOKEN=//p' deploy/.env)" tests/hurl/*.hurl
```

## Do not

- Do not add Redis, Celery-style brokers, or extra containers. Postgres is the queue.
- Do not give plugins access to tokens, other plugins' data, or the core schema.
- Do not add email features of any kind.
- Do not generate code that requires the user to run commands after `docker compose up`.
- Do not mark a checklist item done if its tests are skipped or ignored.
