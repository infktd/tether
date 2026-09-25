# Alliance Platform PRD

Sep 23, 2026 · Jay Nejati

## Summary

A free, self-hosted EVE Online alliance platform that installs with one command, looks modern, and lets plugins add features without ever touching ESI tokens. It replaces SeAT and Alliance Auth for groups that want less setup pain and better security.

**v1 means** New Miner's Union and a small multiboxing corp run on it daily for login, access control, Discord role sync and member audit, with SeAT and Alliance Auth turned off.

The detailed design lives in `docs/ARCHITECTURE.md`; this document defines what to build, in what order, and how to know it's done.

No part of the project will ever charge real money.

## Users and use cases

Four kinds of users, with members as the largest group and the instance admin as the most demanding.

| User | Who | Needs |
| --- | --- | --- |
| Member | Any pilot in the alliance, around 500 characters today | Log in with EVE SSO, link alts, get the right Discord roles automatically |
| Multiboxer | One person running many characters across several EVE accounts | Link every character once, see skills, assets and wallets across all of them in one view |
| Officer | Directors, FCs, recruiters | Audit members and their alts, manage groups, send fleet pings |
| Admin | Whoever hosts the instance | Install in minutes, configure through the browser, install plugins, see ESI and job health |

Key scenarios for v1:

1. A new NMU member logs in with EVE SSO and gets Member access and Discord roles within a minute, with no officer action.
2. A member leaves the alliance; within one affiliation sync they drop to Guest and lose their Discord roles.
3. A multiboxer links 12 characters across 4 accounts and sees a combined skills and assets view.
4. An officer checks whether a recruit's alts are linked and what they've been flying.
5. An admin installs the platform on a fresh VPS and has SSO working without running a shell command after `docker compose up`.
6. An admin installs a plugin from the admin panel and its pages appear without a restart.
7. A moon pops: NMU members get a Discord ping right away, and allies see it on the old-moon list 4 hours later, never earlier.

## Goals and non-goals

The four goals are fixed; anything that doesn't serve one of them waits until after v1.

**Goals**

1. **Doesn't look like ass.** A consistent design system that every page and plugin uses, with EVE-native data display.
2. **Easily self-hostable, with opsec.** One command to install, zero shell commands after, and no outbound traffic beyond ESI, Discord and GitHub for plugins and update checks.
3. **Easy plugins to develop.** A typed SDK, a CLI with scaffolding and hot reload, and mock ESI for offline development.
4. **Secure scoped API.** Plugins never see tokens; every data access is checked against admin approval and user consent.

**Non-goals for v1**

- Charging money, hosted plans or a paid tier, ever.
- A multi-tenant hosted service run by the project.
- A central plugin registry. Plugins install straight from their GitHub repos or an uploaded .zip package.
- Email of any kind: no email login, verification or notifications.
- Wormhole mapping, killboards or market tools. These stay with existing specialized tools.
- Importing data from SeAT or Alliance Auth beyond groups and role mappings.

## Functional requirements

Every requirement below is in v1; the phase column sets build order.

| ID | Requirement | Phase |
| --- | --- | --- |
| F1 | Log in with EVE SSO; no passwords or email anywhere | 0 |
| F2 | One account holds many characters; add alts by logging in with them; change main | 0 |
| F3 | First login on a fresh install becomes the owner account | 0 |
| F4 | Access states, Alliance Auth style: Member, Blue and Guest by default, and more that admins create, each with a priority. Each lists the corporations, alliances and individual characters it applies to, set by admins and audited (Blue is a manual list, not standings). An account's state comes from its main: the highest-priority match wins; no match is Guest. Guest is identity-only | 0 (states: 2) |
| F5 | Groups: open, request-to-join, admin-assigned | 0 |
| F6 | Permissions assigned to states and groups only; every change audit-logged | 0 |
| F7 | Admin CLI: users, states, jobs, trigger sync, `doctor` | 0 |
| F8 | First-run web wizard: ESI app credentials, alliance selection, callback URL check. The first step requires the setup token from `.env` (also printed to the logs at startup as a fallback); once an owner exists the wizard is disabled permanently and the token is ignored | 0 |
| F9 | Encrypted token vault with automatic refresh and revocation handling | 1 |
| F10 | ESI scheduler honoring cache expiry, ETags, error and rate limits, with a shared cache | 1 |
| F11 | Affiliation sync re-evaluates every account's state on a schedule, compliance included: each non-Guest state requires scopes (Member: core scopes, every installed plugin's user scopes, and admin additions; Blue and others: set by admins), on every character of the account, main and alts. Missing or revoked tokens make the account non-compliant: it drops to Guest until fixed, and officers see it flagged. After login, eligible users are prompted to register each character with the required scopes | 1 (compliance: 2) |
| F12 | Discord account linking, state and group role sync, nickname template | 1 |
| F13 | Fleet ping broadcasts to Discord channels with role targeting | 1 |
| F14 | Admin dashboard: ESI health, job queue, error budget, audit log, available platform updates | 1 |
| F15 | Plugin install from a GitHub repo URL or an uploaded .zip package: fetch the latest release or read the upload, verify its signature (publisher key pinned on first install), show capabilities, admin approves, migrate, activate | 2 |
| F16 | Two kinds of plugin ESI scopes: data-source scopes linked once by characters their owners offer and an admin approves (such as a Station Manager for corp mining data), and user scopes, which join the Member state's required scopes (F11): registering characters with the state's scopes is the consent. The profile shows each character's granted scopes and which plugins use them; there is no per-plugin opt-out. Guests grant no scopes | 2 |
| F17 | Plugins get a private database schema, background jobs, schedules, and pages described declaratively and rendered by the host with the shared components | 2 |
| F18 | Plugin update checks against GitHub releases; one-click upgrade with pre-migration snapshot and one-step rollback; updates must be signed by the pinned key | 2 |
| F19 | Personal access tokens with explicit scopes and expiry, for bots and scripts | 2 |
| F20 | Plugin: Member Audit, with characters, skills, assets, wallets and combined multibox views | 3 |
| F21 | Plugin: Moon Tracker, the first plugin built. Extraction timers from data-source characters; Discord ping to Members at each pop; fresh moons visible to Members only, then on the old-moon list for Blue after a configurable window (default 4 hours); viewer watermark on fresh-moon pages; per-pilot mining totals | 3 |
| F22 | Plugin: Fleet Ops, per the existing Fleet Ops PRD | 3 |

## Non-functional requirements

The top rule: a fresh `docker compose up` must produce a working instance with zero shell commands. Anything that needs a manual step is a bug.

| ID | Area | Requirement |
| --- | --- | --- |
| N1 | Install | Three containers: host, Postgres, Caddy. `.env` holds only the domain, a generated database password, a generated setup token and a generated encryption key |
| N2 | Install | Core migrations run automatically on startup; no manual migrate, collect or create-user steps |
| N3 | Install | `doctor` checks DNS, ports 80 and 443 from outside, TLS, database, ESI credentials and callback, Discord token, and prints a fix for each failure |
| N4 | Platforms | Images for amd64 and arm64 |
| N5 | Opsec | Outbound calls only to ESI, EVE SSO, CCP's image server, Discord, GitHub (plugin installs and update checks) and Let's Encrypt (Caddy's certificates only; no other CA). No telemetry or CDNs; fonts and assets are bundled; update checks can be turned off. Exception: dev-only tooling (such as Scalar at `/docs`) may load from a CDN, because it is compiled out of release builds |
| N6 | Opsec | Admin routes can be bound to a separate private interface, such as Tailscale |
| N7 | Security | Refresh tokens and secrets encrypted at rest (key from `.env`, never stored in the database); backups encrypted (milestone 2, with the snapshots) |
| N8 | Security | Plugins never receive tokens; on every ESI call the host checks the plugin's approved scopes and either the character's account compliance (user scopes) or an admin-approved data source |
| N9 | Security | Each plugin's database role is limited to its own schema, with a statement timeout |
| N10 | Security | Every admin action and every plugin data access is written to the audit log |
| N11 | Performance | Host under 300 MB RAM idle; whole stack comfortable on 1 OCPU and 6 GB for 500 characters |
| N12 | Performance | Pages load in under 500 ms on that box, excluding waits on ESI |
| N13 | Reliability | ESI and Discord outages delay work through retries instead of losing it |
| N14 | Upgrades | Upgrade by changing the image tag; rollback is one command using the automatic pre-migration snapshot |
| N15 | Design | All core pages and plugin pages use the shared component library and design tokens |

## Technical decisions

Rust for the host and the server-rendered UI, WASM for v1 plugins. The plugin runtime is hidden behind the SDK, so a TypeScript authoring tier can be added later without breaking any plugin.

| Area | Decision | Why |
| --- | --- | --- |
| Host language | Rust, stable toolchain | Strongest correctness for ESI's edge cases; the compiler makes AI-written code reliable; reuses `eve-esi-client` |
| Web framework | axum on tokio | Mature, fast, fits tower middleware for auth and scopes |
| Database | Postgres 16 with TimescaleDB, via sqlx | One dependency for data, time series and the job queue |
| Job queue | Postgres table with `FOR UPDATE SKIP LOCKED`, workers as tokio tasks | No Redis or broker to run |
| Plugins (v1) | WASM components on Wasmtime, Rust SDK | Top-tier isolation; first-party plugins are written by us |
| Plugins (later) | TypeScript tier in Deno sandboxes, same manifest and SDK surface | Opens authoring to web developers, decided in phase 4 |
| API spec | OpenAPI generated with utoipa, served with Scalar in dev | Documents the JSON API for bots, scripts and testing |
| Frontend | Server-rendered Rust templates (askama) with Basecoat components and htmx; CSS built with Tailwind's standalone CLI | One binary, no JS framework or npm, shadcn look per docs/DESIGN.md |
| Discord | In-process, REST only (twilight-http); no gateway connection | One process, retries through the job queue; members join through OAuth linking, and ESI alone drives role changes |
| TLS | Caddy with automatic certificates | Zero-config HTTPS |
| Build speed | Cargo workspace split by crate, mold linker on Linux, sccache; Cranelift only as an optional nightly extra | Keeps incremental builds fast as the codebase grows |

## Milestones and acceptance criteria

Five milestones, about 510 to 870 hours in total. A short plugin-runtime spike goes first because it's the riskiest piece; NMU can switch its login over after milestone 1.

| Milestone | Scope | Accepted when | Est. hours |
| --- | --- | --- | --- |
| S. Plugin spike | Throwaway prototype | A WASM plugin calls one host function, fetches one ESI endpoint through the host, and renders one page | 20–40 |
| 0. Foundations | F1–F8, N1–N4 | Fresh VPS to working SSO login with no shell commands; `doctor` passes; NMU test characters land in Member. **Accepted locally** (real SSO login, owner claim, NMU in Member, `doctor` ok); the VPS parts are deferred to the pre-launch checklist below | 140–220 |
| 1. ESI and Discord | F9–F14, N5–N7, N13 | A character leaving the alliance loses Member and Discord roles with no admin action; ESI error budget never exceeded in a week of staging | 80–120 |
| 2. Plugin runtime | F15–F19, N8–N10, N14 | A signed plugin installs from GitHub, requests consent, runs jobs and renders pages with no restart; upgrade and rollback both work | 150–250 |
| 3. First-party plugins | F20–F22, N11, N12, N15 | NMU and the multiboxing corp run on it daily; SeAT and Alliance Auth are shut down | 120–240 |

The host API is marked unstable until milestone 3 ends, then frozen as v1.

## Build plan

Start with the plugin spike on its own branch, then build milestone 0 in the order below. Each task ends with passing tests and a commit.

**Repository layout**

```text
alliance-platform/
  Cargo.toml          # workspace
  .cargo/config.toml  # linker settings
  crates/
    core/     # domain: accounts, characters, tiers, groups, permissions
    db/       # sqlx pool, repositories
    esi/      # wraps eve-esi-client: token vault, scheduler, cache
    jobs/     # Postgres job queue and workers
    web/      # axum routes, auth middleware, OpenAPI
    cli/      # admin CLI and doctor
    server/   # binary that wires everything together
  migrations/
  templates/  # askama templates: Basecoat markup + htmx
  assets/     # Tailwind input, vendored Basecoat CSS, htmx, fonts, icons
  deploy/     # Dockerfile, docker-compose.yml, Caddyfile
  tests/hurl/ # API smoke tests
  docs/       # PRD, architecture, spike report
  CLAUDE.md
```

Plugin crates (`plugin-host`, `plugin-sdk`, `wit/`) are added in milestone 2. The spike lives in `spike/` on its own branch and is not merged.

**Spike tasks** (branch `spike/plugin-runtime`)

- [x] Minimal axum host embedding Wasmtime with one WIT interface exposing a host function
- [x] Rust guest plugin compiled to a WASM component that calls it
- [x] Host function fetches one public ESI endpoint through the host on the plugin's behalf
- [x] Plugin returns data the host renders as one HTML page
- [x] Write `docs/SPIKE_REPORT.md`: friction points, build and startup times, binary sizes, and whether the component model feels right

**Milestone 0 tasks, in order**

- [x] Workspace scaffold, CI running fmt, clippy with warnings as errors, and tests
- [x] Multi-arch Dockerfile, compose file and Caddyfile; `docker compose up` serves a health endpoint over HTTPS
- [x] Config from env, Postgres pool, migrations run automatically at startup
- [x] Minimal job queue with retries and a dead-letter state
- [x] EVE SSO login and callback with Postgres-backed sessions
- [x] Accounts, characters, alt linking, main switching, owner bootstrap on first login
- [x] Tier evaluation from the main's corp and alliance at login
- [x] Groups, permissions and audit log
- [x] First-run wizard API: ESI credentials, alliance selection, callback check
- [x] `dev-login` feature flag for fixture sessions, excluded from release builds
- [x] Admin CLI and `doctor`
- [x] OpenAPI via utoipa, Scalar at `/docs` in dev builds
- [x] UI shell per docs/DESIGN.md with askama, Basecoat and htmx: layout, login, profile and wizard pages

**Milestone 1 tasks, in order**

- [x] Token vault: refresh tokens encrypted at rest with granted scopes, access tokens cached and refreshed before expiry, `invalid_grant` marks the token revoked and prompts re-linking
- [x] Verify SSO tokens against CCP's JWKS (signature, issuer, audience, expiry); detect character transfers by owner hash and unlink instead of refusing
- [x] Recurring schedules on the Postgres job queue
- [x] ESI layer: eve-esi-client's shared in-process cache (Expires, ETag) and limits, plus error and rate budget state for the dashboard, interactive requests ahead of bulk work, and a Postgres cache of entity names
- [x] Affiliation sync on a schedule re-evaluates every account's tier
- [x] Admin pages per docs/DESIGN.md: groups, permissions and tier rules
- [x] Discord setup (secrets in the vault) and account linking via OAuth, adding the member to the server with roles
- [x] Discord role sync for tiers and groups, and the nickname template, through the job queue with retries (including taking back a role whose mapping was removed)
- [x] Fleet pings to Discord channels with role targeting
- [x] Admin dashboard: ESI health, job queue, error budget, audit log, available platform updates (switchable off)
- [x] Opsec: one outbound HTTP client enforcing the allowed destinations, checked by `doctor` (admin routes on a separate listener, N6, moved to the pre-launch checklist)

**Milestone 2 tasks, in order**

Decisions from the milestone 2 kickoff are folded into the tasks below. New crates: `zip` (packages), `toml` (manifests), `minisign-verify` (signatures); `chacha20poly1305`'s `stream` feature for encrypted snapshots. For tests only (`tether-plugins`' `testing` feature): `blake2`, and `ed25519-dalek` (already in the build), to sign packages the way minisign does.

- [x] Plugin runtime core (`tether-plugins`): Wasmtime trimmed to the features used, an instance per call, epoch interruption, a memory cap and a call deadline per plugin; tests that an infinite loop, a memory bomb or a trap can't hurt the host or other plugins
- [x] Host API v1 as WIT (`tether:plugin@1`; `host_api = "1"` is the WIT major version), the guest SDK crate with an `AGENTS.md`, a real example plugin; CI builds and lints guests for `wasm32-wasip2`
- [x] Packages: `plugin.toml` parsing and validation (plugin ids limited to the characters link paths allow); safe .zip reading (size caps, no path escapes); minisign signatures with the publisher key pinned on first install; key rotation, where the old key signs a statement endorsing the new one; an admin can re-pin a plugin's key after a confirmation step; both audited
- [x] Plugin lifecycle: install from an uploaded .zip, verify, capability approval screen, migrate, activate with no restart; enable, disable, uninstall; all audited
- [x] Plugin storage (N9): a schema and a login role per plugin (generated credentials, stored encrypted) that can only touch that schema: `search_path` locked to it, no privileges on core, other plugins' schemas or `public`, no CREATEROLE, CREATEDB or BYPASSRLS; CONNECTION LIMIT, `statement_timeout`, `lock_timeout` and `idle_in_transaction_session_timeout` on the role; a small pool per plugin in the host; parameterised SQL through the host API with caps on rows and bytes returned; plugin migrations; tests proving a plugin role can't read core tables or another plugin's schema
- [x] Plugin jobs, declared schedules and logs (shown in the admin panel), on the core job queue. Schedules are fixed intervals (`every = "30m"`), not cron. Plugins can also queue one-off jobs with a `run_at` timestamp (e.g. a Moon Tracker ping at a chunk's exact arrival), under a plugin-chosen key so re-queueing replaces rather than duplicates, and cancellable by that key (extractions get rescheduled or cancelled); capped per plugin in count and in how far ahead (at least 90 days, beyond EVE's 56-day extractions); a job that comes due late (after downtime or a restart) still runs and is told its scheduled time
- [x] Plugin permissions (`plugin.<id>.<name>`, granted like core ones), declarative pages rendered by the host's templates (links always emitted as absolute `/plugins/<id>/<path>`; the incoming request path checked like a link path, and the query string capped before it reaches the plugin; plugin error text never shown to users), navigation entries, and forms that call back into the plugin through htmx
- [x] ESI, identity and Discord host interfaces: data-source characters designated by an admin, per-user scope consent on the profile page (revocable), admin approval and user consent checked on every call, plugins never see a token, every access audited (F16, N8, N10). Per-plugin consent is interim: the compliance task below replaces it
- [ ] States, Alliance Auth style (F4): tiers become states (Member, Blue and Guest by default; admins create more), each with a priority and manual lists of corporations, alliances and characters, audited; "Allied" becomes "Blue" throughout; the main's highest-priority match wins, re-evaluated on every sync; permission grants, Discord role mappings and the plugin identity interface use states
- [ ] Scope compliance (F11, F16, N8): required scopes per non-Guest state (Member: core scopes, every installed plugin's user scopes, admin additions; others admin-set); every character on the account registered with them, main and alts; a prompt after login to register each character; non-compliant accounts drop to Guest until fixed and are flagged for officers; the profile shows granted scopes per character and which plugins use them; plugin user-scope ESI calls require a compliant account and the scope on the character's token, replacing per-plugin consent
- [ ] Plugin HTTP capability: exact HTTPS hosts declared in the manifest and approved by the admin at install (again on upgrade if they change); no redirects outside them; per-plugin rate limits and response size caps; every call audited; named plugin secrets (such as an API key) entered by the admin, stored encrypted and injected by the host into requests to the declared host, never visible to the plugin, which can't set its own Authorization or cookie headers; `doctor` lists the approved hosts
- [ ] Install from a GitHub repo URL; daily plugin update checks; one-click upgrade after a schema snapshot, one-step rollback; updates must be signed by the pinned key (F15, F18)
- [ ] Personal access tokens with explicit scopes and expiry, stored hashed, managed on the profile page (F19)
- [ ] Snapshots, rollback and backups (N14, N7): `pg_dump` from `postgresql-client-16` in the app image into a snapshots volume, encrypted (streamed XChaCha20-Poly1305 with the instance key), free disk checked first, the last 5 kept per kind (core, each plugin); taken before pending core migrations and plugin upgrades; `tether rollback` shows the snapshot's time, warns that newer data is lost and asks to confirm; restores wrap `timescaledb_pre_restore()` / `timescaledb_post_restore()` and refuse a TimescaleDB version mismatch; nightly encrypted backups reuse it; a test snapshots, migrates, rolls back and checks the data

Deferred past milestone 2: `platform plugin dev` (mock ESI, hot reload), from ARCHITECTURE.md. Also deferred until a plugin needs one: daily wall-clock schedules ("daily at HH:MM EVE", e.g. after downtime) as a simple extra form next to intervals.

**Pre-launch checklist** (deferred from milestone 0's acceptance; everything stays local until the project is further along, and these must pass before NMU goes live)

- [ ] Fresh VPS with a real domain: `deploy/install.sh <domain>`, then only the browser wizard; no other shell commands
- [ ] Register the production callback URL (`https://<domain>/auth/callback`) on the EVE application
- [ ] Caddy obtains a Let's Encrypt certificate for the domain
- [ ] `doctor` passes on the VPS, including DNS, ports 80 and 443, TLS and the public URL checks; confirm ports from outside too, since `doctor` checks from the server itself
- [ ] Milestone 1's "ESI error budget never exceeded in a week of staging": run a staging instance for a week and review the dashboard
- [ ] Admin routes on a private interface such as Tailscale (N6), deferred from milestone 1 until the VPS and Tailscale setup are real. The catch: EVE SSO only returns to the one registered callback on the public domain, and session cookies are per host. Options: a separate admin listener with a one-time login hand-off from the public site, or admin pages refusing clients outside configured private networks (admins reach the normal domain over Tailscale)

## Open questions

None of these block the spike or milestone 0; each has a latest point where it must be decided.

- [ ] Project name, before the first public repo
- [ ] License: AGPL or MIT/Apache, before the first public repo
- [x] Discord library: twilight (REST only). Members are added to the server with their roles when they link; leaving the server isn't tracked, only ESI affiliation drives changes
- [ ] Whether Guests may join the Discord server through Tether: deferred at the milestone 1 review. Only Member and Blue pilots can link; Guests (recruits, visitors) usually have the server's invite link before they ever reach Tether
- [x] Plugin database access: raw SQL in the plugin's own schema, through a per-plugin login role Postgres confines to it (details in milestone 2)
- [ ] Make reqwest's TLS backend a feature in `eve-esi-client` so the host can drop `aws-lc-sys` (Jay). Accepted as a build-time cost until then; CI builds each architecture natively
- [ ] `eve-esi-client` follow-ups (Jay): re-export its oauth2 types and allow overriding SSO URLs (so `EveSso` can be tested against wiremock), and a pluggable cache hook plus public budget accessors, so the host can back ESI responses with a shared Postgres cache that survives restarts
- [ ] Whether the WASM component model holds up, or plugins should start on Extism or Deno instead, after the spike
- [ ] Which three AA plugins to port first, confirmed with NMU leadership, before milestone 3
- [ ] Licenses of those AA plugins, checked before porting any code
- [ ] Re-read CCP's current developer license for data retention and sharing rules, before NMU goes live
- [x] Plugin signing: minisign, publisher key pinned on first install, rotation endorsed by the old key, admin re-pin with confirmation
