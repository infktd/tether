# Architecture

A single Rust binary renders the web UI on the server, runs the ESI scheduler and job workers, and hosts plugins in a Wasmtime sandbox. Postgres is the only required dependency. If this file conflicts with `docs/PRD.md`, the PRD wins.

## Overview

```mermaid
flowchart LR
    U[Browser] --> C[Caddy<br/>auto HTTPS]
    C --> H[Host binary<br/>Rust + axum]
    H --> P[(Postgres<br/>+ TimescaleDB)]
    H --> W[Wasmtime<br/>plugins]
    H --> E[ESI layer<br/>tokens + scheduler]
    E --> ESI[EVE ESI / SSO]
    H --> D[Discord bot]
    H --> G[GitHub releases<br/>plugins + updates]
    W -. host calls .-> E
    W -. host calls .-> P
```

Plugins reach ESI, storage and Discord only through host functions, never directly.

| Component | Role | Tech |
| --- | --- | --- |
| Host binary | HTTP API, auth, admin, plugin lifecycle | Rust, axum, tokio, sqlx |
| Plugin runtime | Runs plugin code in a sandbox | Wasmtime, component model (WIT) |
| ESI layer | Token vault, scheduling, shared cache, budgets | `eve-esi-client` crate |
| Job workers | ESI syncs and plugin background tasks | tokio tasks on a Postgres queue |
| UI | Server-rendered pages, navigation, plugin pages | askama templates, Basecoat, htmx |
| Database | Core data, per-plugin schemas, time series | Postgres 16+, TimescaleDB |
| Reverse proxy | TLS, HTTP/2; certificates from Let's Encrypt only | Caddy |

## Plugin model

Plugins are WebAssembly components installed at runtime from a GitHub repo URL or an uploaded .zip package, with no restart and no image rebuild. Each runs in its own Wasmtime instance and can only call host functions its manifest declares.

**Package contents** (a .zip, published as a release asset on the plugin's GitHub repo or uploaded directly):

- `plugin.toml`: identity, version, host API version, declared capabilities and permissions
- `plugin.wasm`: the component, built against the host's WIT interfaces
- `migrations/`: SQL files for the plugin's own schema, `0001_<name>.sql` onward with no gaps
- `ui/`: optional images (PNG, JPEG, WebP, GIF) referenced by the plugin's page descriptions
- `rotation.txt` and `rotation.txt.minisig`: only when the publisher changed keys (below)
- a detached minisign signature over the whole .zip, next to it (`<package>.zip.minisig`)

Everything else in a package is refused, as are symlinks, encrypted entries, repeated names and archive comments.

```toml
[plugin]
id = "nmu.mining-ledger"          # up to 50 lowercase letters, digits, single . - _
name = "Mining ledger"
version = "0.3.1"
host_api = "1"
repository = "https://github.com/example/mining-ledger"

[publisher]
key = "RWQ..."                    # minisign public key, pinned on first install

[capabilities]
storage = true
discord = ["send_message"]
http = ["janice.e-351.com"]

[capabilities.secrets.janice_api_key]   # entered by the admin; the host sends it
host = "janice.e-351.com"                # to this declared host only,
header = "X-ApiKey"                      # in this header

[capabilities.esi]
user = []                          # scopes each user consents to
data_source = ["esi-industry.read_corporation_mining.v1"]  # characters an admin designates

[[capabilities.schedules]]
name = "sync_mining"
every = "30m"                      # fixed intervals, 5m to 7d

[permissions]
view = "View mining ledger"
manage = "Manage mining ledger"
```

Unknown fields are refused, so a typo can't hide a capability.

**Publisher keys:** the key in the first installed package is pinned for that plugin id and kept even after uninstall. A package signed by another key is refused unless it carries `rotation.txt`, the exact statement `tether-key-rotation v1`, `plugin: <id>`, `old: <pinned key>`, `new: <new key>` (one per line), signed by the pinned key. A publisher who lost their key can't rotate; an admin can re-pin the plugin's key after typing its id to confirm. Pinning, rotating and re-pinning are audited.

**Install flow:** admin pastes a repo URL (host fetches the latest release) or uploads a .zip → verifies the signature and pins the publisher key on first install → shows declared capabilities → admin approves → host applies migrations → component loads. Later updates must be signed by the pinned key or they are rejected.

```mermaid
stateDiagram-v2
    [*] --> Downloaded
    Downloaded --> Verified: signature ok
    Verified --> Approved: admin accepts capabilities
    Approved --> Migrated: schema applied
    Migrated --> Active: component loaded
    Active --> Disabled: admin disables
    Disabled --> Active
    Active --> Migrated: upgrade
    Disabled --> [*]: uninstall
```

Upgrades snapshot the plugin's schema first, so a failed upgrade rolls back to the previous version and data.

**Host API** (versioned WIT interfaces, unstable until the end of milestone 3):

| Interface | What a plugin can do |
| --- | --- |
| `esi` | Request ESI data for a character or corp, within approved and consented scopes |
| `storage` | Query and write its own Postgres schema only |
| `identity` | Read the current user, characters, corp, alliance, roles |
| `jobs` | Enqueue background work and run declared schedules |
| `discord` | Send messages and read role mappings, if declared |
| `http` | Outbound requests to hosts declared in the manifest only |
| `log` | Structured logs shown in the admin panel |

**Consent, three layers:**

1. Admin approves the plugin's declared capabilities at install.
2. Each user consents to the plugin's ESI scopes on their profile page and can revoke at any time.
3. The host checks both on every call. Plugins never see a token.

**AI-friendly SDK:** WIT files are the machine-readable contract. The plugin template ships an `AGENTS.md` describing the SDK, capabilities and patterns, plus complete example plugins. `platform plugin dev` runs against mock ESI with hot reload. Errors are actionable: a call outside declared scopes names the exact manifest entry to add.

**Resource limits:** Wasmtime fuel or epoch interruption plus a memory cap per plugin. Each plugin's database role has a `statement_timeout`.

## ESI layer

The host is the only thing that talks to ESI. It owns every token, schedules every sync, and fetches each piece of data once however many plugins want it.

- **Token vault:** refresh tokens encrypted at rest with a key from the environment; access tokens cached in memory and refreshed before expiry; each token records its granted scopes; `invalid_grant` marks the token revoked and prompts the user to re-link.
- **Scheduler:** next fetch comes from the `Expires` header; conditional requests with `ETag` / `If-None-Match`; identical requests from different plugins are merged and served from a shared cache; interactive requests jump ahead of bulk syncs.
- **Budgets:** tracks ESI error-limit and rate-limit headers and slows down before hitting them; per-plugin shares, so one misbehaving plugin is throttled and flagged instead of getting the whole instance blocked.
- **User-Agent:** includes the admin contact, set once at the host level.

## Storage and jobs

Postgres holds everything, including the job queue.

- `core` schema: users, characters, tokens, corps, alliances, tiers, groups, permissions, audit log.
- `plugin_<id>` schemas: one per plugin with `storage = true`, owned by Tether's role. The plugin's own login role (random password, sealed in `core.secrets`) can use and create objects only there: no rights on `core`, `public` or other plugins' schemas, no temporary tables, no advisory locks. The host reaches it through a small pool per plugin, and sets timeouts, memory and `search_path` before every statement, since a role can change its own defaults. Uninstalling drops the schema and role.
- TimescaleDB hypertables are for Tether's own time series for now: plugin roles have no access to `public`, where TimescaleDB's functions live. If a plugin needs one, the host can offer it through a host call.
- `esi_cache`: shared cached responses with expiry, host-only.
- Job queue: a `jobs` table polled with `SELECT … FOR UPDATE SKIP LOCKED`; exponential backoff; dead-letter state; visible in the admin panel. Workers are tokio tasks inside the host; the count is a config value.
- Core migrations are embedded in the binary and run on startup. Plugin migrations run on install and upgrade, each in its own transaction as the plugin's role, recorded with a checksum so an applied one can't change; upgrades take a snapshot first (task 12).
- Nightly encrypted `pg_dump` to a local directory, optionally to S3-compatible storage (deferred to the pre-launch checklist).

## Identity and permissions

EVE SSO is the only login. No email.

- One account holds many characters; the first linked is the main and can be changed. Alts are added by logging in with them while signed in.
- The first account to log in on a fresh install becomes the owner.
- Tiers: **Member** (main in a listed alliance or corp), **Allied** (main in a listed blue entity), **Guest** (everyone else). Re-evaluated on every affiliation sync via ESI's bulk affiliation endpoint.
- Groups add access on top of tiers: open, request-to-join, admin-assigned.
- Permissions come from the core and from plugin manifests, and are assigned to tiers or groups only. Every change is audit-logged.
- Personal access tokens with explicit scopes and expiry, for bots and scripts.

## UI

Server-rendered HTML from Rust. No JS framework, no npm, no separate frontend build. The look is defined in `docs/DESIGN.md`.

- **askama** templates, compiled into the binary and type-checked at build time.
- **Basecoat** supplies the shadcn-style components as plain CSS classes (`btn`, `card`, `input`, `table`, `tabs`). Its CSS is vendored into `assets/vendor/` and themed with the tokens from `DESIGN.md`.
- **htmx** handles interactivity without writing JavaScript: partial page swaps for tabs, filters, sorting, pagination and forms; server-sent events for live countdowns, pop alerts and job status.
- **Tailwind's standalone CLI** (a single binary) builds the CSS. No Node or npm anywhere.
- Fonts (Geist, Geist Mono), icons and htmx are bundled and served by the host. The browser's only external requests are to CCP's image server.
- Navigation is built from core routes plus plugin manifests, filtered by the viewer's permissions.
- A small JSON API (documented with utoipa) serves bots, scripts and personal access tokens; the UI itself uses HTML over htmx.

**Plugin pages are declarative.** A plugin returns a page description (header, stat row, tables, cards, tabs, forms, badges, charts) as structured data. The host validates it and renders it with the same templates as core pages. Every plugin looks native, and no plugin code ever runs in members' browsers. Interactive elements in a description map to host-provided htmx actions that call back into the plugin.

## Discord

Built into the host, not a plugin. REST only (twilight-http): no gateway connection. One bot serves the core and every plugin. Members join the server through OAuth linking, which adds them with their roles; after that, only ESI affiliation drives role changes, and leaving the Discord server isn't tracked.

- Account linking via OAuth from the profile page.
- Tier and group to role mappings, applied automatically as membership changes.
- Optional nickname template, such as `[TICKER] Main Name`.
- Fleet ping broadcasts with channel and role targeting; plugins can send through the same path if declared.
- Role changes go through the job queue with retries.

## Deployment

Three containers: host, Postgres, Caddy. Images for amd64 and arm64.

```yaml
services:
  app:
    image: ghcr.io/<org>/alliance-platform:1
    env_file: .env
    depends_on: [db]
    restart: unless-stopped
  db:
    image: timescale/timescaledb:latest-pg16
    env_file: .env
    volumes: [pgdata:/var/lib/postgresql/data]
    restart: unless-stopped
  caddy:
    image: caddy:2
    ports: ["80:80", "443:443"]
    command: caddy reverse-proxy --from ${DOMAIN} --to app:8080
    restart: unless-stopped
volumes:
  pgdata:
```

- `.env` holds only the domain, a generated database password, a generated setup token and a generated encryption key (for tokens and secrets at rest; it never enters the database). Everything else is set in the first-run web wizard, which shows the exact EVE callback URL to register and tests it.
- The wizard's first step requires the setup token, which is also printed to the logs at startup as a fallback. Once an owner exists the wizard is disabled permanently and the token is ignored.
- `doctor` checks DNS, external reachability of 80 and 443, TLS, database, ESI credentials and callback match, and the Discord token, printing a fix for each failure.
- Upgrades: change the image tag and restart. Migrations run after an automatic snapshot; rollback is the previous tag plus that snapshot.
- Admin routes can be bound to a private interface such as Tailscale.

## Testing without a frontend

- OpenAPI via utoipa with Scalar at `/docs` in dev builds as a clickable test UI.
- Real SSO works with no frontend: visit `/auth/login`, the backend handles redirect and callback.
- `dev-login` feature for fixture sessions, compiled out of release builds.
- wiremock with recorded ESI fixtures, `#[sqlx::test]`, `tokio::time::pause`, insta snapshots, Hurl smoke tests.
- A staging instance on the Oracle box with real characters before anything reaches NMU.
