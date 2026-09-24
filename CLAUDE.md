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
- Let's Encrypt (ACME), from Caddy only, for TLS certificates. No other CA

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
- ESI is mocked with `wiremock` using recorded response fixtures in `tests/fixtures/esi/`. Tests never call real ESI or Discord.
- `tokio::time::pause()` for scheduler and job timing tests.
- `insta` snapshots for API response shapes.
- Hurl files in `tests/hurl/` for end-to-end smoke tests against a running stack.
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
cargo run -p tether-server --features dev   # /dev/login fixtures and Scalar at /docs (debug builds only)
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
cargo clippy -p hello-plugin --target wasm32-wasip2 -- -D warnings
cargo build -p hello-plugin --target wasm32-wasip2 --release
tailwindcss -i assets/app.css -o static/app.css --minify
deploy/install.sh localhost    # writes deploy/.env once, then starts the stack
docker compose -f deploy/docker-compose.yml exec app tether doctor   # also: users, tiers, jobs, sync
docker compose -f deploy/docker-compose.yml up --build
hurl --test tests/hurl/*.hurl
```

## Do not

- Do not add Redis, Celery-style brokers, or extra containers. Postgres is the queue.
- Do not give plugins access to tokens, other plugins' data, or the core schema.
- Do not add email features of any kind.
- Do not generate code that requires the user to run commands after `docker compose up`.
- Do not mark a checklist item done if its tests are skipped or ignored.
