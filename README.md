# Tether

**Your alliance's home base, self-hosted.** EVE SSO, access states, Discord role sync, Secure Groups, and apps sandboxed so tightly they never see a single ESI token. One command to install; everything else happens in your browser.

[![License: GPL v2+](https://img.shields.io/badge/license-GPL--2.0--or--later-blue.svg)](LICENSE)
![Rust](https://img.shields.io/badge/built%20with-Rust-orange.svg)
![Self-hosted](https://img.shields.io/badge/self--hosted-yes-green.svg)

Tether runs the day-to-day of an EVE Online alliance or corporation: who's in, what they can see and do, who gets which Discord roles, and the tools your members use every day. It's free and will never charge real money.

## Why Tether

- **One command, then your browser.** `deploy/install.sh your.domain`, then a setup wizard walks you through connecting EVE and Discord. There's no `migrate`, no `collectstatic`, and no editing of config files by hand.
- **Apps that can't touch your tokens.** Apps run in a WebAssembly sandbox. They ask Tether for ESI data and Tether makes the call, checking each request against what you approved. Refresh tokens never leave the core.
- **Familiar to admins.** Tether uses the names alliance admins already know from existing auth tools: States, Groups, Secure Groups, Blacklist, Corporation Stats, Fleet Pings. The screens are cleaner, and it does less magic behind your back.
- **Light.** A single Rust binary next to Postgres. Most pages render in milliseconds, and the whole stack runs comfortably on a small VPS. There's no Redis or Celery, and no extra services to babysit.
- **Quiet on the network.** Tether talks only to ESI and EVE SSO, Discord, GitHub (for app installs and update checks, which you can switch off) and, through Caddy, Let's Encrypt. There's no telemetry, and there are no CDNs or remote fonts.

## What's inside

**Core**
- EVE SSO login. One account holds many characters, and the main decides the account's access.
- **States**: Member, Blue, Guest and your own, covering alliances, corporations, factions and characters, each with required ESI scopes and a compliance report.
- **Groups**: open, request-to-join, hidden and restricted groups, with group leaders, **Auto Groups** per corporation and alliance, and **Secure Groups**, which keep themselves in line using filters (state, affiliation, character age, skills, assets, FAT counts and more).
- **Permissions**: granted to states and groups, every change audited, plus a Permissions Audit showing exactly who holds what.
- **Discord**: role sync for states and groups, nickname formatting, and linking from the Services page.
- **Admin tools**: Users, Blacklist and Pilot Log, Corporation Stats, Fleet Pings, notifications, an audit log, personal access tokens for bots, and a customizable sidebar.
- **Safety nets**: an encrypted snapshot before every migration, nightly encrypted backups, and one-step rollback for Tether and each app.

**Apps included with Tether** (approve each one in a click from the Apps page)

| App | What it does |
| --- | --- |
| Moon Mining | Extraction timers, pop pings, mining ledgers, and a planner for a steady extraction cadence |
| Member Audit | Characters, skills, assets and wallets, with views for multiboxers |
| Structures | Upwell structures, starbases, customs offices, fuel alerts, fittings and tags |
| Structure Timers | A shared timerboard, filled automatically from Structures |
| Fleet Activity Tracking | FAT links, ESI-tracked fleets and participation stats |
| Ship Replacement | SRP fleets and requests, with killmail values from zKillboard |
| HR Applications | Application forms per corporation, and recruiter reviews |

More apps install straight from a GitHub repository. Tether reviews what each one asks for and shows it to you before anything runs.

## Install

You need a server with Docker (with the compose plugin) and a domain pointing at it.

```bash
git clone https://github.com/infktd/tether.git
cd tether
deploy/install.sh alliance.example.com
```

Then open `https://alliance.example.com/`, enter the setup token the script printed, and follow the wizard. It shows you exactly what to register on [developers.eveonline.com](https://developers.eveonline.com/) and checks that it works.

By default Tether brings its own Caddy with automatic HTTPS. If your server already runs **nginx** or **Traefik**, use `--proxy nginx` or `--proxy traefik` instead (Tether writes the config for you), or `--proxy none` to wire up your own proxy. See [deploy/README.md](deploy/README.md) for details.

```bash
docker compose -f deploy/docker-compose.yml exec app tether doctor
```

`doctor` checks DNS, ports, TLS, the database, EVE SSO and Discord, and tells you how to fix anything that's off.

## Building apps

Apps are Rust crates compiled to WebAssembly components against the Tether SDK. They get their own database schema, background jobs, pages built from Tether's components (so every app looks native), Discord messages, and ESI through the host. The SDK guide, [crates/plugin-sdk/AGENTS.md](crates/plugin-sdk/AGENTS.md), covers everything from the manifest to publishing a release on GitHub. The SDK is MIT or Apache-2.0, so your app can use any license you like.

## Developing Tether

```bash
docker compose -f deploy/docker-compose.dev.yml up -d   # Postgres for development and tests
echo DATABASE_URL=postgres://tether:tether@127.0.0.1:5433/tether > .env
cargo run -p tether-server --features dev                # /dev/login fixtures, API docs at /docs
cargo test --workspace
```

[CLAUDE.md](CLAUDE.md) holds the conventions and common commands. [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) explains how the pieces fit, and [docs/DESIGN.md](docs/DESIGN.md) covers the look and feel.

## License

Tether is licensed under the [GNU General Public License v2.0 or later](LICENSE). The app SDK (`crates/plugin-sdk`) and the app interface (`wit/`) are licensed under MIT or Apache-2.0, at your option.

EVE Online and all related logos and designs are trademarks or registered trademarks of CCP ehf. Tether is not affiliated with or endorsed by CCP.

o7
