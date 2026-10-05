# Alliance Platform PRD

Sep 23, 2026 · Jay Nejati

## Summary

A free, self-hosted EVE Online alliance platform that installs with one command, looks modern, and lets plugins add features without ever touching ESI tokens. It replaces SeAT and Alliance Auth for groups that want less setup pain and better security.

**v1 means** a real alliance and a small multiboxing corp run on it daily for login, access control, Discord role sync and member audit, with SeAT and Alliance Auth turned off.

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

1. A new alliance member logs in with EVE SSO and gets Member access and Discord roles within a minute, with no officer action.
2. A member leaves the alliance; within one affiliation sync they drop to Guest and lose their Discord roles.
3. A multiboxer links 12 characters across 4 accounts and sees a combined skills and assets view.
4. An officer checks whether a recruit's alts are linked and what they've been flying.
5. An admin installs the platform on a fresh VPS and has SSO working without running a shell command after `docker compose up`.
6. An admin installs a plugin from the admin panel and its pages appear without a restart.
7. A moon pops: members get a Discord ping right away, and allies see it on the old-moon list 4 hours later, never earlier. (Optional since 2026-09-27, when Jay chose AA's rules: aa-moonmining does neither, so a manager turns them on.)

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
- A central plugin registry in v1. Apps install straight from their GitHub repos, and the first-party apps come with Tether (for app developers, a development build also installs an uploaded .zip). A curated catalog comes after 1.0 (milestone 4).
- Email of any kind: no email login, verification or notifications.
- Wormhole mapping, killboards or market tools. These stay with existing specialized tools.
- Importing data from SeAT or Alliance Auth beyond groups and role mappings.

## Functional requirements

Every requirement below is in v1; the phase column sets build order.

| ID | Requirement | Phase |
| --- | --- | --- |
| F1 | Log in with EVE SSO; no passwords or email anywhere | 0 |
| F2 | One account holds many characters; add alts by logging in with them; change main | 0 |
| F3 | First login on a fresh install becomes the first superuser (AA's superusers: any number, each holding every permission, made and unmade by superusers; Jay, 2026-09-27) | 0 |
| F4 | Access states, Alliance Auth style: Member, Blue and Guest by default, and more that admins create, each with a priority. Each lists the corporations, alliances and individual characters it applies to, set by admins and audited (Blue is a manual list, not standings). An account's state comes from its main: the highest-priority match wins; a state marked public (AA's) matches any main; no match is Guest. Guest is identity-only | 0 (states: 2) |
| F5 | Groups: open, request-to-join, admin-assigned | 0 |
| F6 | Permissions assigned to states, groups and single users, as AA's (Jay, 2026-09-27); every change audit-logged | 0 |
| F7 | Admin CLI: users, states, jobs, trigger sync, `doctor` | 0 |
| F8 | First-run web wizard: ESI app credentials, alliance selection, callback URL check. The first step requires the setup token from `.env` (also printed to the logs at startup as a fallback); once a superuser exists the wizard is disabled permanently and the token is ignored | 0 |
| F9 | Encrypted token vault with automatic refresh and revocation handling | 1 |
| F10 | ESI scheduler honoring cache expiry, ETags, error and rate limits, with a shared cache | 1 |
| F11 | Affiliation sync re-evaluates every account's state on a schedule, and compliance with it, Alliance Auth style: each non-Guest state requires scopes (Member: the corporation member list scope and admin additions; Blue and others: set by admins; an admin can require an app of a state in one click, as AA's Member Audit compliance groups: every character registered for the app, with its scopes; Jay, 2026-09-27) on every character of the account, main and alts. An account with a character not registered with them keeps its state but is flagged (a revoked token still counts with the scopes it carried, as Member Audit's registration; its owner is asked to log in with it again, and the character leaves the account a day later unless they do): officers see it, its owner is prompted to register each character after login, and it leaves compliance groups (as Member Audit's: Internal groups admins designate, each limited to its allowed states), which admins can grant permissions and Discord roles to. Corp Stats (each covered corporation's member list, read with any registered Member character in it) shows members of covered corporations who never registered | 1 (compliance: 2) |
| F12 | Discord account linking, state and group role sync, nickname template (off by default, as AA's `DISCORD_SYNC_NAMES`); members no longer in the server are unlinked and notified, as AA | 1 |
| F13 | Fleet ping broadcasts to Discord channels with role targeting | 1 |
| F14 | Admin dashboard: ESI health, job queue, error budget, audit log, available platform updates | 1 |
| F15 | Plugin install from a GitHub repo URL or an uploaded .zip package (development builds only, for app developers: 2026-09-26): fetch the latest release or read the upload, verify its signature (publisher key pinned on first install), show capabilities, admin approves, migrate, activate. The first-party apps come with every deployment instead (bundled into the image, unsigned since they are as trusted as the image, and reviewed and approved the same way; their ids are reserved. A newer image's update or same-version rebuild of an app installed from the bundle is applied at startup, as the system, when the review would list nothing, since it asks for nothing the admin didn't approve already, and not after an admin rolled back from it: 2026-09-27) | 2 |
| F16 | Two kinds of plugin ESI scopes: data-source scopes linked once by their own characters that holders of one of the app's `add_…` permissions (not its `manage`, as in AA) or app admins add from the app's page, Alliance Auth style (AA's Add Owner, as AA's `add_refinery_owner`, `add_structure_owner` and aa-afat's `add_fatlink`; one EVE login, in use at once, no admin approval; app admins see and remove any, owners withdraw their own, every add and removal audited; Jay, 2026-09-26) (such as a Station Manager for corp mining data), and user scopes, AA style (Jay, 2026-09-27): whoever holds one of the app's permissions, whatever their state, registers characters for it from the app (one EVE login granting its user scopes, the host's Register Character; registering is the consent), and the app reads only its characters: those registered for it (as aa-memberaudit reads only characters added to it; pilots unregister them from the same page, and a character leaving its account leaves every app), on accounts holding one of its permissions, whose token carries every one of its user scopes. The upgrade registered for each app the characters it read until then. No state requires an app unless an admin says so (F11); then registration counts, not the scopes alone. The Dashboard shows each character's granted scopes and which apps use them | 2 |
| F17 | Plugins get a private database schema, background jobs, schedules, and pages described declaratively and rendered by the host with the shared components | 2 |
| F18 | Plugin update checks against GitHub releases; one-click upgrade with pre-migration snapshot and one-step rollback; updates must be signed by the pinned key | 2 |
| F19 | Personal access tokens with explicit scopes and expiry, for bots and scripts | 2 |
| F20 | Plugin: Member Audit, with characters, skills, assets, wallets and combined multibox views | 3 |
| F21 | Plugin: Moon Mining (AA's name; was Moon Tracker), the first plugin built. Extraction timers from owner (data-source) characters; optionally (off by default, as aa-moonmining has neither: Jay, 2026-09-27) a Discord ping to Members at each pop, and fresh moons visible to Members only, then on the old-moon list for Blue after a configurable window; viewer watermark on app pages (the host's); per-pilot mining totals. Plus an extraction planner for characters with the in-game Station Manager role: a chosen pop cadence (such as one moon a day at a set EVE time) turned into the duration to set at each Athanor, accounting for auto-fracture, flagging gaps, overlaps and idle drills (ESI can't set extractions, so it only advises) | 3 |
| F22 | Plugin: Fleet Operations, AA parity (optimer) | 3 |
| F23 | Alliance Auth parity: every admin and audit feature of AA core and the AA apps alliances use for administration, under AA's names, per `docs/AA_PARITY.md` (which lists what's done, missing and deliberately skipped) | 2–3 |
| F24 | App catalog, after 1.0 (Jay's plan): a GitHub organization holds the first-party apps and a registry repository; publishers open pull requests to list their apps, and the maintainers review them. Tether reads the registry through `api.github.com` (no new outbound host) and shows a "Browse apps" page with one-click installs. The registry lists each app's repository and publisher key, so an install trusts the reviewed key rather than the first one it sees, and the registry index is signed with a key built into Tether. Corps may donate ISK; nothing is gated behind it | 4 |

## Non-functional requirements

The top rule: a fresh `docker compose up` must produce a working instance with zero shell commands. Anything that needs a manual step is a bug.

| ID | Area | Requirement |
| --- | --- | --- |
| N1 | Install | Two containers, host and Postgres, plus Caddy unless the admin chose their own reverse proxy (nginx on the host, Traefik in Docker, or any other) at install. The host is a published image, pulled, never compiled on the server; installing needs only the deploy files, not a clone. `.env` holds only the domain, a generated database password, a generated setup token, a generated encryption key, the pinned image and the reverse proxy choice |
| N2 | Install | Core migrations run automatically on startup; no manual migrate, collect or create-user steps |
| N3 | Install | `doctor` checks DNS, ports 80 and 443 from outside, TLS, database, ESI credentials and callback, Discord token, and prints a fix for each failure, fitted to the reverse proxy in use (port 80 is required only with Caddy) |
| N4 | Platforms | Published multi-arch images for amd64 and arm64 (`ghcr.io`), each built natively on its own architecture by CI once its checks pass |
| N5 | Opsec | Outbound calls only to ESI, EVE SSO, CCP's image server, Discord, GitHub (plugin installs and update checks) and Let's Encrypt (Caddy's certificates only, when Caddy is the proxy; no other CA. With the admin's own nginx, Traefik or other proxy, certificates are that proxy's business, outside Tether), plus the hosts an instance's admin approves for each plugin (never these core ones). No telemetry or CDNs; fonts and assets are bundled; update checks can be turned off. Exception: dev-only tooling (such as Scalar at `/docs`) may load from a CDN, because it is compiled out of release builds |
| N6 | Opsec | Admins sign in like everyone else: EVE SSO on the public domain, with no separate admin login, listener or private network. Admin pages and API endpoints are gated by permissions, checked on the server for every request. Superuser-only and sensitive actions (sensitive permission grants, making or unmaking superusers, app install, upgrade, rollback, uninstall, key re-pin and secrets, account deactivation, access tokens, setup and Discord settings, a Restricted group's flag and direct membership, letting people into groups that grant sensitive permissions, and Change Main, Add Character and deleting the main's token for accounts holding such powers) also need an EVE login with the account's main in the last 15 minutes (sudo mode, as GitHub's; see docs/ARCHITECTURE.md) |
| N7 | Security | Refresh tokens and secrets encrypted at rest (key from `.env`, never stored in the database); backups encrypted (milestone 2, with the snapshots) |
| N8 | Security | Plugins never receive tokens; on every ESI call the host checks the plugin's approved scopes and either that the character is one of the app's (registered for the app, its account holds one of the app's permissions, and its token carries every one of the app's user scopes), or a data source (an app owner) added by a holder of the app's add permission, still on that holder's account and in the corporation it was added for (Jay, 2026-09-26: AA's model, no admin approval) |
| N9 | Security | Each plugin's database role is limited to its own schema, with a statement timeout |
| N10 | Security | Every admin action and every plugin data access is written to the audit log |
| N11 | Performance | Host under 300 MB RAM idle; whole stack comfortable on 1 OCPU and 6 GB for 500 characters |
| N12 | Performance | Pages load in under 500 ms on that box, excluding waits on ESI |
| N13 | Reliability | ESI and Discord outages delay work through retries instead of losing it |
| N14 | Upgrades | Upgrade by changing the image tag (`install.sh --version X.Y.Z`), or from the console's System page through the updater container (Jay, 2026-09-28); rollback is the previous tag plus one command using the automatic pre-migration snapshot, or one button that does both |
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
| TLS | Caddy with automatic certificates by default; the admin's nginx, Traefik or other proxy instead, chosen at install | Zero-config HTTPS, without a second proxy where one already runs |
| Build speed | Cargo workspace split by crate, mold linker on Linux, sccache; Cranelift only as an optional nightly extra | Keeps incremental builds fast as the codebase grows |

## Milestones and acceptance criteria

Five milestones to 1.0, about 510 to 870 hours in total. A short plugin-runtime spike goes first because it's the riskiest piece; an alliance can switch its login over after milestone 1.

| Milestone | Scope | Accepted when | Est. hours |
| --- | --- | --- | --- |
| S. Plugin spike | Throwaway prototype | A WASM plugin calls one host function, fetches one ESI endpoint through the host, and renders one page | 20–40 |
| 0. Foundations | F1–F8, N1–N4 | Fresh VPS to working SSO login with no shell commands; `doctor` passes; the test alliance's characters land in Member. **Accepted locally** (real SSO login, owner claim, the test alliance in Member, `doctor` ok); the VPS parts are deferred to the pre-launch checklist below | 140–220 |
| 1. ESI and Discord | F9–F14, N5–N7, N13 | A character leaving the alliance loses Member and Discord roles with no admin action; ESI error budget never exceeded in a week of staging | 80–120 |
| 2. Plugin runtime | F15–F19, N8–N10, N14 | A signed plugin installs from GitHub, requests consent, runs jobs and renders pages with no restart; upgrade and rollback both work | 150–250 |
| 3. First-party plugins | F20–F22, N11, N12, N15 | The pilot alliance and the multiboxing corp run on it daily; SeAT and Alliance Auth are shut down | 120–240 |
| 4. App catalog (after 1.0) | F24 | An admin browses the catalog in Tether and installs a listed app in one click; a publisher gets an app listed through a reviewed pull request | not estimated |

The host API is marked unstable until milestone 3 ends, then frozen as v1.

## Build plan

Start with the plugin spike on its own branch, then build milestone 0 in the order below. Each task ends with passing tests and a commit.

**Repository layout**

```text
alliance-platform/
  Cargo.toml          # workspace
  .cargo/config.toml  # linker settings
  crates/
    core/     # domain: accounts, characters, states, groups, permissions
    db/       # sqlx pool, repositories
    esi/      # wraps eve-esi-client: token vault, scheduler, cache
    jobs/     # Postgres job queue and workers
    web/      # axum routes, auth middleware, OpenAPI
    cli/      # admin CLI and doctor
    server/   # binary that wires everything together
  migrations/
  templates/  # askama templates: Basecoat markup + htmx
  assets/     # Tailwind input, vendored Basecoat CSS, htmx, fonts, icons
  deploy/     # Dockerfile, docker-compose.yml (+ proxy overrides), Caddyfile, install.sh
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
- [x] Opsec: one outbound HTTP client enforcing the allowed destinations, checked by `doctor` (N6 later changed: admins sign in through EVE SSO like everyone else, with admin pages gated by permissions; no separate admin listener)

**Milestone 2 tasks, in order**

Decisions from the milestone 2 kickoff are folded into the tasks below. New crates: `zip` (packages), `toml` (manifests), `minisign-verify` (signatures); `aead-stream` for encrypted snapshots (chacha20poly1305 0.11 has no `stream` feature: the construction moved to that crate). For tests only (`tether-plugins`' `testing` feature): `blake2`, and `ed25519-dalek` (already in the build), to sign packages the way minisign does.

- [x] Plugin runtime core (`tether-plugins`): Wasmtime trimmed to the features used, an instance per call, epoch interruption, a memory cap and a call deadline per plugin; tests that an infinite loop, a memory bomb or a trap can't hurt the host or other plugins
- [x] Host API v1 as WIT (`tether:plugin@1`; `host_api = "1"` is the WIT major version), the guest SDK crate with an `AGENTS.md`, a real example plugin; CI builds and lints guests for `wasm32-wasip2`
- [x] Packages: `plugin.toml` parsing and validation (plugin ids limited to the characters link paths allow); safe .zip reading (size caps, no path escapes); minisign signatures with the publisher key pinned on first install; key rotation, where the old key signs a statement endorsing the new one; an admin can re-pin a plugin's key after a confirmation step; both audited
- [x] Plugin lifecycle: install from an uploaded .zip, verify, capability approval screen, migrate, activate with no restart; enable, disable, uninstall; all audited
- [x] Plugin storage (N9): a schema and a login role per plugin (generated credentials, stored encrypted) that can only touch that schema: `search_path` locked to it, no privileges on core, other plugins' schemas or `public`, no CREATEROLE, CREATEDB or BYPASSRLS; CONNECTION LIMIT, `statement_timeout`, `lock_timeout` and `idle_in_transaction_session_timeout` on the role; a small pool per plugin in the host; parameterised SQL through the host API with caps on rows and bytes returned; plugin migrations; tests proving a plugin role can't read core tables or another plugin's schema
- [x] Plugin jobs, declared schedules and logs (shown in the admin panel), on the core job queue. Schedules are fixed intervals (`every = "30m"`), not cron. Plugins can also queue one-off jobs with a `run_at` timestamp (e.g. a Moon Tracker ping at a chunk's exact arrival), under a plugin-chosen key so re-queueing replaces rather than duplicates, and cancellable by that key (extractions get rescheduled or cancelled); capped per plugin in count and in how far ahead (at least 90 days, beyond EVE's 56-day extractions); a job that comes due late (after downtime or a restart) still runs and is told its scheduled time
- [x] Plugin permissions (`plugin.<id>.<name>`, granted like core ones), declarative pages rendered by the host's templates (links always emitted as absolute `/plugins/<id>/<path>`; the incoming request path checked like a link path, and the query string capped before it reaches the plugin; plugin error text never shown to users), navigation entries, and forms that call back into the plugin through htmx
- [x] ESI, identity and Discord host interfaces: data-source characters designated by an admin, per-user scope consent on the profile page (revocable), admin approval and user consent checked on every call, plugins never see a token, every access audited (F16, N8, N10). Per-plugin consent is interim: the compliance task below replaces it. Since 2026-09-26 (Jay: AA's permissions and behaviour) data sources need no admin approval: holders of the app's add permission add their own characters (F16)
- [x] States, Alliance Auth style (F4): tiers become states (Member, Blue and Guest by default; admins create more), each with a priority and manual lists of corporations, alliances and characters, audited; "Allied" becomes "Blue" throughout; the main's highest-priority match wins, re-evaluated on every sync; permission grants, Discord role mappings and the plugin identity interface use states
- [x] Scope compliance (F11, F16, N8), Alliance Auth style: required scopes per non-Guest state (Member: the corporation member list scope, every installed plugin's user scopes, admin additions; others admin-set); every character on the account registered with them, main and alts; a prompt after login to register each character; non-compliant accounts keep their state but are flagged for officers and leave the Tether-managed Compliant group; the Dashboard shows granted scopes per character and which plugins use them; plugin user-scope ESI calls require a Member character registered with the scope, replacing per-plugin consent (since 2026-09-27, AA's model instead: a character registered for the app by a holder of one of its permissions, in any state; Member no longer requires apps' scopes by itself, and the upgrade kept the ones it required as Member's own); Corp Stats, Alliance Auth style: Tether reads each covered corporation's member list daily with any registered Member character in it (nobody offers or approves one), and officers see which members of covered corporations never registered
- [x] Login and ownership, AA behaviour (F2, F9, F23; security): only the main signs in, with AA's message; alts only through Add Character; a character linked to another account moves on a fresh SSO login; owner hash checked at login, on every token refresh and by an ownership check every 4 hours (every token), with ownership records and returning owners re-attached; a sold main or one with no valid token clears the main (Guest, services off, Dashboard and Change Main only) and such an alt leaves the account, nothing promoted silently; Change Main needs a valid token; Deactivate account (admin, audited); sessions 14 days; names refreshed by the affiliation sync. See `docs/AA_PARITY.md` Behaviour
- [x] AA names (F23): the renames in `docs/AA_PARITY.md` across UI, docs and code (Profile → Dashboard, Group Management, Fleet Pings, Corporation Stats, Compliance Report and Compliance Group, Register Character, Change Main, Name Formatter fields; plugins are "Apps" in the UI, "plugin" in the SDK and code); permission names aligned with AA's, with grants migrated
- [x] States, AA behaviour (F4, F23): priorities 100/50/0 as editable numbers (the buttons stay); Member and Blue can be renamed and deleted, only Guest protected, with plugins and scopes following the built-in role; names at most 32 characters; a state change removes groups the state doesn't allow (with Groups parity) and notifies
- [x] Groups, AA parity (F5, F23): Internal, Hidden (direct join link), Open, Public and Restricted flags; allowed states, removed on a state change; Group Leaders and Group Leader Groups; leave requests with an auto-leave setting (off by default); the users' Groups page (Available Groups); Group Management with Group Requests, Group Membership and a per-group Audit Log; reserved group names; a `request_groups` permission; Compliance Groups as in Member Audit (admins mark Internal groups, each limited to its allowed states, replacing the single Compliant group). Rules in `docs/AA_PARITY.md` Behaviour
- [x] Notifications (F23): in-app, levels, at most AA's `NOTIFICATIONS_MAX_PER_USER` per account (50 unless changed on System; oldest go first), open marks read, delete one, mark all read, delete all read, unread count live over SSE; AA's messages for state changes and group decisions, opt-in request notices to leaders; plus compliance changes, Discord access removed (sent by Services, which adds losing access), and characters lost to a sale. Rules in `docs/AA_PARITY.md` Behaviour
- [x] Token Management (F23): every stored token with its scopes, delete and refresh (ownership checks are in the login and ownership task)
- [x] Services and Name Formatter (F12, F23): a Services page; Discord access by permission (granted to Member and Blue by default), re-checked on state, permission and group changes; losing access kicks the member from the server and unlinks them (bot gains Kick Members), as does unlinking; an optional setting removes every unmapped role except Discord-managed roles and reserved names; everyone is synced again every five minutes (catching what no trigger sees), which refreshes the stored username; one name format per state with AA's fields and format specs, default `{character_name}`
- [x] Moon Mining plugin (F21), brought forward from milestone 3 at Jay's request, with the Station Manager extraction planner. The pop pings and the Members-only window are optional and off by default since 0.2.2 (aa-moonmining's rules); the planner stays
- [x] Member Audit plugin (F20), brought forward from milestone 3 at Jay's request, under AA's page names
- [x] Member Audit at aa-memberaudit and SeAT day-one parity (F20): the full character sheet (skills and the live queue by group, skill sets, attributes, assets by location, wallet journal and transactions, market orders, contracts and their items, loyalty points, clones and implants, industry jobs, blueprints, mining ledger, planets, contacts and NPC standings, corporation history, roles and titles, killmails, bio), portraits and logos throughout, Update now; mail on audited pages, for whoever may open the sheet (since 0.3.0, 2026-09-27, as aa-memberaudit: no `view_mail`); My Characters as a card grid with Register Character first; the Character Finder with a search box and aa-memberaudit's scope permissions; each section synced on its own clock within the ESI budget. Since 2026-09-27 (Jay: AA's permissions and settings), aa-memberaudit's permission names (0.2's grants carried over), `reports_access`, `view_skill_sets`, character sharing (`share_characters`, `view_shared_characters`) and its settings on the app's Settings page; pilots holding one of its permissions, in any state, register characters for it, and admins may require its scopes of a state. The Dashboard leads with My Characters when Member Audit is installed and the viewer has its basic access (Jay, 2026-09-26: "The Dashboard should be what Character Audit is"). Data Export (Jay, 2026-09-27), as aa-memberaudit's: CSV files of every character's contracts, contract items and wallet journal with AA's columns, for `exports_access`, rebuilt daily and on request at most once an hour, through the host's downloads
- [x] Auto Groups (F23): automatic corporation and alliance groups for chosen states (prefix, full name or ticker, space replacement)
- [x] Corporation Stats, AA parity (F11, F23): its own page with Mains, Members and Unregistered tabs, search, Update Now (checking the viewer may see that corporation), view permissions per corporation, alliance or state; each list read with any registered member's token, the one that worked last first, one that fails skipped
- [x] Permissions Audit (F23): every permission with counts of states, groups and accounts holding it, and who
- [x] Users (F23), as AA's admin site Users: an admin page to find any account by character, see its characters, state, groups and permissions, and Deactivate or Reactivate it (`admin.users`; the API and CLI came with the login task)
- [x] Dashboard (F14, F23): admin panels on the Dashboard (version, task queue, ESI status) and widgets plugins can add. The admin panels have since moved to the top of Administration's overview (2026-09-26), leaving the Dashboard to pilots
- [x] States cover factions (F4, F23), as AA's Member Factions
- [x] Fleet Pings, AA parity (F13, F23): aa-fleetpings' fields (target, pre-ping, fleet type, FC, fleet name, formup location and time, comms, doctrine, SRP, additional information) and copy-paste text; channels, targets, fleet types and doctrines limited to groups or states, checked on the server; a setting to turn off @here and @everyone
- [x] The accent colour setting from DESIGN.md
- [x] Menu customization (F23), as AA's Menu: admins reorder and hide sidebar items, group them in folders and add custom links; apps' links included
- [x] Structure Timers plugin (F23), as AA's timerboard: timers with structure type, objective (friendly, hostile, neutral), system, planet or moon, EVE time and details, important and corporation-only flags, visible and editable by permission. Timers other apps publish (Structures) are listed with them as automatic, read-only timers
- [x] Fleet Activity Tracking plugin (F23), as AA's fleetactivitytracking with aa-afat's additions (Jay: alliances track FATs): FCs create a FAT link for a fleet with a fleet type and an expiry, members click it to record their character's attendance, or the FC's ESI fleet is tracked while it runs and everyone who joins gets a FAT; stats per alliance, corporation, pilot and month. ESI-tracked fleets through the `fleet-members` endpoint: as aa-afat, an FC with `add_fatlink` logs in with the fleet boss from Create FAT Link (the app's data source, AA style, no approval: Jay, 2026-09-26), and only FCs are asked for the scope. aa-afat's rules and settings (Jay, 2026-09-27: "take AA's permissions and their settings"): `log_view`; `add_fatlink` changes any link; a link reopens once, within the reopen grace time, for the reopen duration; manual FATs within 24 hours and before a reopen; registering needs the character online (the app's location scopes as its user scopes; system and ship recorded); the default expiry, reopen grace and duration and log duration are settings; with aa-afat's `use_doctrines_from_fittings_module` (a setting, off by default) Create FAT Link offers the doctrines Fittings shares that the FC may see
- [x] Ship Replacement plugin (F23), as AA's srp: SRP fleets, members submit losses by killmail link, reviewers approve, reject or adjust payouts, totals per fleet (killmail values from zKillboard, `zkillboard.com` approved by Jay as an outbound destination). Built as `plugins/ship-replacement`; losses come from ESI's public killmail endpoint, added to the plugin ESI catalogue as its first public (tokenless) entry, `killmail`. AA's permissions and rules (Jay, 2026-09-27): `srp_management` (AA's `auth.srp_management`, replacing `manage_srp` and `change_srpuserrequest`) also adds fleets; every `access_srp` holder sees open fleets' Total ISK Cost and any fleet's requests; managers may decide their own requests; payouts update at any status without changing it; zKillboard links only; removing a fleet frees its losses to be requested again
- [x] Fittings plugin (F23), as allianceauth-fittings, at Jay's request (2026-09-26: "a 'Fits' and 'Doctrines' app, so Corps/Alliance Doctrine designers can upload EFTs and have them listed"): fits pasted as EFT (name from the header, errors line by line), each shown by slot with type icons, Copy EFT, notes, its doctrines and its required skills; doctrines with an icon hull and their fits; AA's categories limited to groups and AA's permissions (`access_fittings`, `manage`) and visibility rules exactly. Built as `plugins/fittings`; item names become ids through two new public catalogue entries, `universe-ids` and `universe-type` (esi.evetech.net, no new destination), kept in the app's storage; group limits through two new WIT functions, `identity.groups` and `identity.all-groups` (approved by Jay), for apps approved for the new `groups` capability. Parity with allianceauth-fittings (Jay, 2026-09-27): Copy Buy All (the multibuy list); Save to EVE through the WIT `esi.post` (approved by Jay: the one write, a fitting to the pilot's own registered character at their click); Can I fly it from the pilot's own registered characters' skills (Fittings' own `esi-skills.read_skills.v1`, Jay's choice over Member Audit's); its doctrines shared with Fleet Pings and FAT (the WIT `doctrines` interface)
- [x] HR Applications plugin (F23), as AA's hrapplications: application forms per corporation with questions, pilots apply, recruiters review with comments and approve or reject, applicants see their status. AA's rules (Jay, 2026-09-27): comments need `add_applicationcomment`, applicants withdraw until the decision, reviewers see their own applications; applying needs only a main, as AA (pages a manifest marks `signed_in`; Jay approved the option, 2026-09-27), and what AA shows superusers only (every corporation's applications, deciding any) goes by the viewer being a superuser (`identity.superuser`, approved the same day)
- [x] Fleet Operations plugin (F22), brought forward from milestone 3 as AA's optimer (there is no separate Fleet Ops PRD; Jay: build to AA parity, with AA's permissions and behaviour and better pages): upcoming and past operations with name, doctrine, form-up system, start in EVE time with a countdown, duration, FC, type, description and the character's portrait; Create Operation, Edit and Delete (asking first) for `optimer_management`, viewing for `optimer_view`; operation types made as they're used, as AA's; the Dashboard's Upcoming Fleets (the next five). Built as `plugins/fleet-operations`
- [x] Character ownership re-checked every 4 hours (F23), as AA: each character's owner hash compared with EVE's through its token, alongside the daily token check; a changed owner hash acts at once, as at login. Already built with Login and ownership (`ownership.check`: hourly runs, each token refreshed every 4 hours; tests in `crates/web/tests/it/ownership.rs`); this line came from a stale parity row
- [x] Blacklist and Pilot Log (F23), as AA's blacklist: admins note pilots, corporations and alliances (reason, added by, when); a blacklisted main's account goes to a Blacklist state above every other, with its services removed; notes stay on the Users page
- [x] Secure Groups (F23), as aa-securegroups (AA's "Smart Groups"): groups whose members Tether keeps by filters (state, corporation or alliance, character age, other groups, compliance, and app-provided filters such as Member Audit's skills and assets), auto or on request, a grace period with notifications before removal, and a log; app-provided filters need a read-only plugin capability (a WIT change: propose first). Built: smart groups with auto join or requests, grace periods with notifications, the audit log, and the core filters (state, main or any character's corporation or alliance, character age, other groups, compliant, each reversible). App-provided filters through the WIT `filters` interface (approved by Jay): apps declare `[[filters]]` and report per-character values from an hourly job. Member Audit offers `skill` (a skill trained to a level), `skill_set` (can use one of its skill sets) and `asset` (an item type in the assets); Fleet Activity Tracking offers `fats` (FATs in the last N days, added up across an account)
- [x] Secure Groups, AA's rules (F23; Jay, 2026-09-27): allianceauth-secure-groups' `access_sec_group` (the Secure Groups page: check yourself, join, request, leave) and `audit_sec_group` (Secure Group Audit, with Group Management over the group); its settings (enabled, include in updates, can grace, notify on add, remove and grace), a grace period per filter, filter expressions, exemptions, the faction and Discord service filters; AA's update webhooks as the bot posting run summaries to a ping channel. Services Tether doesn't run have no filters
- [x] Blacklist, AA's rules (F23; Jay, 2026-09-27): allianceauth-blacklist's 16 permissions (own-corporation notes, restricted and ultra restricted tiers, comments; grants of the old three moved); a blacklisted note is how anyone is blacklisted; blacklisting goes by the main and only applies the Blacklist state (its grants, groups that don't exclude it, services it may use); NPC corporations can be blacklisted. Kept as platform guards: the Blacklist state holds no sensitive permission, and a blacklisted account can't Change Main
- [x] Fleet Pings, AA's rules (F23; Jay, 2026-09-27: "take AA's permissions and their settings"): aa-fleetpings' `fleetpings.basic_access` replaces `fleet.ping` (grants and personal access tokens moved); aa-fleetpings' settings: default ping targets, default fleet types (Roaming, Home Defense, StratOP, CTA) and the default embed colour. Doctrines from Fittings (aa-fleetpings' `use_doctrines_from_fittings_module`, off by default): the doctrines Fittings shares through the host (the WIT `doctrines` interface, approved by Jay 2026-09-27) that the pilot may see, linking to their pages, instead of the configured ones
- [x] Structures plugin (F23), as aa-structures: owners add corporations through a data source with the Station Manager role; structures with fuel, services and state; low fuel, attack and state notifications to chosen Discord channels; structure timers feed the Structure Timers plugin. Built: the plugin (`plugins/structures`) with owners, the structure list (fuel, services, state, reinforce hour, region), notifications relayed once each (attacks, reinforcements, fuel, services, power, anchoring, moon drills) and low-fuel alerts at chosen thresholds, with 403 back-off. Its timers (armor, hull, anchoring, unanchoring) are published after every sync through the WIT `timers` interface and shown in Structure Timers as automatic, read-only timers from "Structures", friendly, and corporation-only when a manager turns on "Timers are corporation-only" (aa-structures' STRUCTURES_TIMERS_ARE_CORP_RESTRICTED, off by default). Then 1:1 with aa-structures (Director reads through new catalogue entries): starbases with fuel and reinforcement, customs offices with taxes and the public list, Orbital Skyhooks (from assets), Metenox magmatic gas, fittings, tags with the tag filter, and per-owner Discord routing (aa-structures' webhooks per owner); ESI's gaps are in `docs/AA_PARITY.md`. aa-structures' rules (Jay, 2026-09-27): `view_all_unanchoring_status`; notification types filtered per owner, with pings by severity (state roles stand in for @everyone and @here); fuel alert configs, any number; up to 10 sync characters per owner, rotated. customs office tax for pilots outside the owner's corporation and alliance as aa-structures (the neutral rate, marked uncertain; Jay chose this over reading contacts); and every aa-structures notification type (Jay, 2026-09-27): sovereignty and bills, wars, members and projects on their own channels, alliance-wide ones through the alliance main owner, sov timers, refuelled notices and jump fuel alerts
- [x] Time Zones plugin, as aa-timezones (Jay, 2026-09-27, the first of the community apps he picked by usage): EVE time, the pilot's own zone and the panels (aa-timezones' ten defaults until `manage` sets its own), adjusted for a timer or a planned fleet on a page to share; open to anyone signed in; daylight saving time from the IANA database (`chrono-tz`, approved for this app)
- [x] Sovereignty Timer plugin, as aa-sov-timer (Jay, 2026-09-27): sovereignty campaigns with their defender, ADM, start and progress, filtered into upcoming and active, synced every 30 seconds (pages read only storage); the host's public catalogue gains `sovereignty-campaigns` and `universe-constellation`, and `sovereignty-systems` its ADM
- [x] ESI Status plugin, as aa-esi-status (Jay, 2026-09-27): ESI's status route by route (OK, Degraded, Down, Recovering, Unknown), the compatibility date and 24 hours of history, open to anyone signed in; the host's public catalogue gains `esi-status` (ESI's /meta/status)
- [x] Bulletin Board plugin, as aa-bulletin-board (Jay, 2026-09-27): bulletins for everyone or members of some groups, AA's two permissions, plain-text paragraphs
- [x] Contacts plugin, as aa-contacts (Jay, 2026-09-27, with its data-source scopes `esi-alliances.read_contacts.v1` and `esi-corporations.read_contacts.v1`): alliance and corporation contacts, standings, labels, notes and server links, for those in them; apps may name their owner permissions (`owner_permissions`)
- [x] Freight plugin, as aa-freight (Jay, 2026-09-27, with its data-source scope `esi-contracts.read_corporation_contracts.v1`): the contract handler's courier contracts in aa-freight's four operation modes, priced routes with the reward calculator, contracts checked against their pricing, My contracts, statistics, locations, and pilot and customer notices on Discord; the host's catalogue gains `corporation-contracts` and the public `character-affiliation`
- [x] Contracts plugin (Jay, 2026-09-29, what Bastion's contract notices did; its data-source scopes `esi-contracts.read_corporation_contracts.v1` and `esi-universe.read_structures.v1`, and `janice.e-351.com` as its approved host): every contract assigned to an owner's corporation, a corporation's first read taken as its backlog; Discord cards (the corporation above, kind and place as the title, who did what, Location, Details, Expires with a countdown, the items) when one comes in, completes, or expires or is rejected, cancelled or deleted, each switchable; the Janice appraisal a description links read once with the admin's API key (the app's secret) and the price checked against its buy total within a set tolerance (1% by default), and "No ISK asked" said so; two catalogue endpoints for it (`corporation-contract-items`, `source-structure`)
- [x] Blueprints plugin, as aa-blueprints (Jay, 2026-10-04, from a member's request: BPO owners list their library and members request copies): corporate owners (data sources: blueprints, corporation jobs and assets, structures) and personal owners (registered characters: the same for a character), the library with ME, TE, runs and where each is, requests for copies the owners' builders take, fulfil, re-open or cancel, with aa-blueprints' eight permissions; requesters and approvers told in Tether's notifications through the new notify interface (approved the same day), new requests also on Discord; three catalogue endpoints for it (`corporation-blueprints`, `corporation-industry-jobs`, `corporation-asset-places`)
- [x] Removing a character from an account at once (Jay, 2026-09-30): pilots their own alts (Characters card), admins any account's from the user's page (`admin.users`, sudo mode, only accounts holding nothing the admin doesn't); never the main (pick another first) or the last character; the character leaves as a sold one does (tokens, app registrations and data sources with it), audited as `character.ownership_lost` with reason `removed`, and its owner notified
- [x] Change character (Jay, 2026-09-30): the account menu lists the account's characters to act as; apps get the chosen one from `identity.acting` (a WIT addition, approved) where they act for the pilot (FAT's ticked character, Fittings' first pilot, SRP's FC), while `viewer.main` and every access scope stay the real main, per browser (a cookie holding only a character id, honoured only if it's one of the account's own); permissions and state stay the account's, as Alliance Auth's; the sidebar says "Acting as". App settings open from each app's Administration page; app tables page 25 rows at a time; every link and form is boosted (no page reloads)
- [x] Plugin HTTP capability: exact HTTPS hosts declared in the manifest and approved by the admin at install (again on upgrade if they change); no redirects outside them; per-plugin rate limits and response size caps; every call audited; named plugin secrets (such as an API key) entered by the admin, stored encrypted and injected by the host into requests to the declared host, never visible to the plugin, which can't set its own Authorization or cookie headers; `doctor` lists the approved hosts. Built as the WIT `http` interface (`crates/web-core/src/plugin_http.rs`): what was approved is stored (`core.plugin_http_hosts`, `core.plugin_http_secrets`) and a host or secret the running package declares but nobody approved is refused. Upgrades don't exist yet: the upgrade task below must show changed hosts and secrets on its review and call `plugin_http::approve` when the admin approves
- [x] Install from a GitHub repo URL; daily plugin update checks; one-click upgrade after a schema snapshot, one-step rollback; updates must be signed by the pinned key (F15, F18). The upgrade review shows what the new version asks for beyond the old, HTTP hosts and secrets included, and approving it records them (`plugin_http::approve`); also new shared-timer access and Secure Groups filters (Member Audit, Structures and Structure Timers gained them after their first release)
- [x] Personal access tokens with explicit scopes and expiry, stored hashed, managed on the Dashboard (F19)
- [x] Snapshots, rollback and backups (N14, N7): `pg_dump` from `postgresql-client-16` in the app image into a snapshots volume, encrypted (streamed XChaCha20-Poly1305 with the instance key), free disk checked first, the last 5 kept per kind (core, each plugin); taken before pending core migrations and plugin upgrades; `tether rollback` shows the snapshot's time, warns that newer data is lost and asks to confirm; restores wrap `timescaledb_pre_restore()` / `timescaledb_post_restore()` and refuse a TimescaleDB version mismatch; nightly encrypted backups reuse it; a test snapshots, migrates, rolls back and checks the data
- [x] Core rules as Alliance Auth's (Jay, 2026-09-27: "take AA's permissions and their settings"; Tether beats AA on the platform and deployment): no sign-in to an account without a main (`tether users main` sets one from the shell); any number of superusers, made and unmade by superusers with a fresh login, audited, the last one kept; permissions granted to single users (a user's page, Permissions, the Permissions Audit); only permissions the granter holds are granted or revoked, to anyone, and nobody grants themselves; public states; Restricted groups and Group Leaders as AA's code; a revoked token keeps its registration for compliance; `NOTIFICATIONS_MAX_PER_USER` on System; `DISCORD_SYNC_NAMES` off by default for new instances; members no longer in the Discord server unlinked and notified. Platform guards stay (sudo mode, sensitive permissions never to Guest, public states or Open groups, no handing out what you don't hold). Rules in `docs/AA_PARITY.md` Behaviour

Deferred past milestone 2: `platform plugin dev` (mock ESI, hot reload), from ARCHITECTURE.md. Also deferred until a plugin needs one: daily wall-clock schedules ("daily at HH:MM EVE", e.g. after downtime) as a simple extra form next to intervals.

**Pre-launch checklist** (deferred from milestone 0's acceptance; everything stays local until the project is further along, and these must pass before the first alliance goes live)

- [x] Reverse proxy choice at install (N1): `deploy/install.sh --proxy caddy|nginx|traefik|none`, asked interactively. Caddy stays the default; nginx gets a generated server block (installed and reloaded when run as root), Traefik gets labels on its network, `none` publishes the app on 127.0.0.1 with documented requirements (`deploy/README.md`). X-Forwarded-For is believed only from loopback and private peers; `doctor` fits its checks to the proxy; CI starts the image through compose without Caddy and checks `/health`
- [x] Published images (N1, N4, N14): CI publishes `ghcr.io/<owner>/tether` after every check passes (main as `:edge` and `:sha-<commit>`, `vX.Y.Z` tags as `:X.Y.Z`, `:X.Y`, `:latest`), amd64 and arm64 built natively and joined by digest. `docker-compose.yml` pulls `TETHER_IMAGE`, which install.sh pins (newest release, else `:edge`) and moves only on `--version` or `--build`. `docker-compose.build.yml` builds from a clone, for CI and development. Installs work from the deploy files alone. Done: published from main on every push since, and the package pulls without logging in
- [x] Upgrades from the console (N14; Jay, 2026-09-28): an updater container (`docker:28.5.2-cli`, pinned by digest, `deploy/updater.sh`) holds the Docker socket, not the app; no network, no ports. The System page asks it for a published tag of the install's image (a newer release, or the newest edge) or one step back (restoring the pre-upgrade snapshot when the upgrade migrated, the version typed to confirm), in sudo mode and audited; the updater checks every request itself, reports progress back, and install.sh turns it on. `doctor` says whether it runs; a `--build` install upgrades on the server
- [ ] Fresh VPS with a real domain: `deploy/install.sh <domain>`, then only the browser wizard; no other shell commands
- [ ] Register the production callback URL (`https://<domain>/auth/callback`) on the EVE application
- [ ] Caddy obtains a Let's Encrypt certificate for the domain (or, with `--proxy nginx`/`traefik`, the admin's proxy serves one)
- [ ] `doctor` passes on the VPS, including DNS, ports 80 and 443, TLS and the public URL checks; confirm ports from outside too, since `doctor` checks from the server itself
- [ ] Milestone 1's "ESI error budget never exceeded in a week of staging": run a staging instance for a week and review the dashboard

## Open questions

None of these block the spike or milestone 0; each has a latest point where it must be decided.

- [ ] Project name, before the first public repo
- [x] License: GPL-2.0-or-later (Jay: the same family as Alliance Auth's GPLv2; "or later" because Apache-2.0 dependencies such as Wasmtime can't be combined with GPLv2 alone). `LICENSE` holds the GPLv2 text; every crate says `GPL-2.0-or-later` except the app SDK (`crates/plugin-sdk`) and the app interface (`wit/`), which are MIT OR Apache-2.0 so apps can use any license
- [x] Discord library: twilight (REST only). Members are added to the server with their roles when they link; leaving the server isn't tracked, only ESI affiliation drives changes
- [x] Whether Guests may join the Discord server through Tether: decided by permission, as in AA ("Can access the Discord service", granted to Member and Blue by default; admins may grant it to Guest). Decided 2026-09-25
- [x] Plugin database access: raw SQL in the plugin's own schema, through a per-plugin login role Postgres confines to it (details in milestone 2)
- [x] Make reqwest's TLS backend a feature in `eve-esi-client` so the host can drop `aws-lc-sys` (Jay): done in eve-esi-client 0.5.1 (released as a patch; it carries the 0.6.0 changes). Tether uses rustls with ring, installed as the process-wide provider at startup, and CI fails if aws-lc returns to `Cargo.lock`
- [x] `eve-esi-client` follow-ups (Jay): re-export its oauth2 types and allow overriding SSO URLs (so `EveSso` can be tested against wiremock), and a pluggable cache hook plus public budget accessors, so the host can back ESI responses with a shared Postgres cache that survives restarts: done in eve-esi-client 0.5.1 (released as a patch; it carries the 0.6.0 changes), and Tether uses all of them (`core.esi_cache`)
- [ ] Whether the WASM component model holds up, or plugins should start on Extism or Deno instead, after the spike
- [x] Which AA apps to port first: Member Audit and Moon Mining, then (for 1:1 parity with AA core) Structure Timers, Fleet Activity Tracking, SRP and HR Applications; Structures and other community apps stay off the plan until asked. Decided 2026-09-25
- [x] Licenses of those AA plugins, checked before porting any code: Alliance Auth GPL-2.0; aa-memberaudit, aa-moonmining, aa-structures, aa-structuretimers, allianceauth-secure-groups and allianceauth-blacklist MIT; aa-fleetpings, allianceauth-afat, aa-srp and allianceauth-fittings GPL-3.0. Tether copies no code, only a few short labels and sentences (MIT notices in `NOTICE.md`); details in `docs/LEGAL.md`. Checked 2026-09-27
- [x] Re-read CCP's current developer license for data retention and sharing rules, before the first alliance goes live: no retention limit or sharing rule beyond consent (2.3(c)) and privacy law (9.5); its proprietary notice (7.1) is on every page; non-commercial (4.1). Details in `docs/LEGAL.md`. Checked 2026-09-27
- [x] Plugin signing: minisign, publisher key pinned on first install, rotation endorsed by the old key, admin re-pin with confirmation
