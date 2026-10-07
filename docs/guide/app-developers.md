# App developer guide

This guide is for people who build apps for Tether. It walks through making a small app, trying it in a development build, signing it and publishing it on GitHub. The full reference, every field, call and limit, is [crates/plugin-sdk/AGENTS.md](../../crates/plugin-sdk/AGENTS.md). The machine-readable contract is [wit/plugin.wit](../../wit/plugin.wit).

Tether's UI says "app"; the SDK and the code say "plugin". They're the same thing.

The SDK and the app interface are licensed MIT or Apache-2.0, so your app can use any license you like.

## What an app is

An app is a Rust library compiled to a WebAssembly component. Tether loads it at runtime, without a restart, and runs each call in a sandbox. An app:

- describes its pages as data, and Tether draws them with its own templates, so every app looks like the rest of Tether and no app code runs in a browser;
- can only use what the host offers: its own database schema, background jobs, ESI through the host, Discord messages, HTTPS to hosts the admin approved, and a few shared interfaces;
- never sees an ESI token, a secret, another app's data or Tether's own tables.

Before an app is installed, the admin sees everything it asks for and approves it. Every new version is reviewed too, showing what it asks for beyond the last.

Good examples to read:

- `examples/hello-plugin`: the code of a complete example, with every kind of section and value.
- `plugins/*`: the apps that come with Tether, each with its `plugin.toml`, built with the same SDK. `plugins/esi-status` is small; `plugins/member-audit` and `plugins/structures` use most of the SDK.

## What you need

- Rust (stable) with the WebAssembly target: `rustup target add wasm32-wasip2`.
- [minisign](https://jedisct1.github.io/minisign/) to sign packages, and `zip`.
- A GitHub repository to publish releases from.
- For trying your app before publishing: a clone of Tether, and Docker for its development database.

## 1. Start the crate

`Cargo.toml`:

```toml
[package]
name = "hello"
version = "0.1.0"
edition = "2024"

[lib]
crate-type = ["cdylib"]

[dependencies]
tether-plugin-sdk = { git = "https://github.com/infktd/tether" }
```

`src/lib.rs`:

```rust
use tether_plugin_sdk::{Page, PageError, Plugin, Request};

struct Hello;

impl Plugin for Hello {
    fn render(request: Request) -> Result<Page, PageError> {
        match request.path.as_str() {
            "" => Ok(Page::new("Hello").text("o7")),
            _ => Err(PageError::NotFound),
        }
    }
}

tether_plugin_sdk::export!(Hello);
```

`render` gets the path below your app's pages and returns a page description. Pages are read-only: changes happen when someone posts one of your forms (`submit`) or in a background job (`run_job`). AGENTS.md covers both.

## 2. Write plugin.toml

Every package carries a `plugin.toml`: who the app is, what it asks for, and who may open its pages.

```toml
[plugin]
id = "acme.hello"          # yours: lowercase letters, digits and single . - _
name = "Hello"
version = "0.1.0"
host_api = "1"
icon = "flag"              # one of Tether's icons (AGENTS.md, The app shell)

[publisher]
key = "RWQ..."             # the second line of your minisign .pub file (step 4)

[permissions]
view = "See Hello"

[permission_notes]
view = "Usually Member."

[[pages]]
path = ""                  # every page...
permission = "view"        # ...for holders of this permission

[[navigation]]
label = "Hello"
path = ""

[[views]]
label = "Overview"
path = ""
```

A few things to know:

- **Pick your own id prefix**, such as your name or organisation. The ids of the apps that come with Tether, such as `tether.moon-mining`, are reserved, and a package using one is refused.
- **Every page needs a `[[pages]]` rule.** A page no rule covers is for admins only. Admins grant your permissions to states and groups, as `plugin.<id>.<name>`.
- **Ask only for what you use.** Each capability (`storage`, ESI scopes, Discord, HTTP hosts, secrets, schedules) is shown to the admin before they approve. Unknown fields are refused, so a typo fails loudly.
- **ESI comes in two kinds.** User scopes read pilots' own characters, once they register them for your app. Data-source scopes read corporation data through a character someone adds as a data source, as Alliance Auth's Add Owner. AGENTS.md, ESI, lists every endpoint Tether offers apps.
- **Tether draws the app's frame**: the header, the views bar (`[[views]]`), one primary action (`[action]`) and the Manage pages (`[[manage]]`). Your pages return only their content.

## 3. Build

```bash
cargo build --target wasm32-wasip2 --release
cargo clippy --target wasm32-wasip2 -- -D warnings
```

The component is `target/wasm32-wasip2/release/hello.wasm` (the crate name, with `-` as `_`). Always build for `wasm32-wasip2`: a missing `export!` only fails when the component is linked.

If your app keeps data, set `storage = true` under `[capabilities]` and put SQL migrations in `migrations/` (`0001_create_notes.sql`, `0002_...`). A migration can't change once it has run anywhere, so fix mistakes in a new one.

## 4. Package and sign

A package is a `.zip` with `plugin.toml`, the component as `plugin.wasm`, and your `migrations/` and `ui/` images if you have them. Nothing else is allowed in it.

Make a signing key once, and put the public key's second line in `plugin.toml` under `[publisher]`:

```bash
minisign -G -p acme.pub -s acme.key
```

Then for each version:

```bash
rm -rf package && mkdir package
cp plugin.toml package/
cp target/wasm32-wasip2/release/hello.wasm package/plugin.wasm
cp -R migrations package/          # if you have them
(cd package && zip -X -r ../acme.hello-0.1.0.zip .)
minisign -S -s acme.key -m acme.hello-0.1.0.zip   # writes acme.hello-0.1.0.zip.minisig
```

The first install of your app pins your key for its id. Every later version must be signed with the same key, so keep a backup of it. AGENTS.md, Packaging and signing, explains how to move to a new key.

## 5. Try it in a development build

Release builds of Tether install apps only from GitHub. A development build can also install a signed package from a file, and has test logins, so you can try a version before publishing it.

From a clone of Tether:

```bash
docker compose -f deploy/docker-compose.dev.yml up -d   # Postgres on 127.0.0.1:5433
export DATABASE_URL=postgres://tether:tether@127.0.0.1:5433/tether
export DOMAIN=localhost PUBLIC_URL=http://localhost:8080
export ENCRYPTION_KEY=$(openssl rand -hex 32) SKIP_MIGRATION_SNAPSHOT=true
cargo run -p tether-server --features dev
```

Keep the same `ENCRYPTION_KEY` between runs: it encrypts what Tether stores.

Then:

1. Open `http://localhost:8080/dev/login/owner` to sign in as a test superuser. `/dev/login` lists the other test pilots (a Member, a Blue and a Guest).
2. Go to **Administration › Apps**. Under **Install from a file (development build)**, choose your `.zip` as **Package** and the `.minisig` as **Signature**, and press **Upload and check**.
3. Read the review: it's what admins will see. Press **Approve and install**.
4. Grant your permissions to a state on **Permissions**, and open your app from the sidebar.

To try a new version, bump `version` in `plugin.toml`, build, package and upload it again. Tether shows the upgrade review.

The test pilots have no EVE login, so anything that reads ESI for them won't work. To try ESI, set up an EVE application in the setup wizard and log in with a real character.

Your app's **Activity** page (under its **Manage** button) shows its ESI and HTTPS calls, its schedules with **Run now**, its jobs and its log. Write to the log with `log::info` and friends.

## 6. Publish on GitHub

Make a GitHub release (not a draft or a pre-release) and attach both files, named exactly:

- `<app id>-<version>.zip`, such as `acme.hello-0.1.0.zip`
- `<app id>-<version>.zip.minisig`

The version in the name must match `version` in `plugin.toml`. One repository can publish several apps: Tether reads the 30 newest releases and takes the highest version of each app that has both files.

Admins install it from **Administration › Apps**, under **Install from GitHub**: they enter your repository's address, and your app id if the repository publishes more than one. Tether downloads the package, checks the signature and shows the review. Nothing installs until they approve.

Tell admins what to do after installing, in your README:

- which permissions to grant to whom (your `[permission_notes]` say it on the Permissions page too);
- which ESI scopes to enable on their EVE application (the app's page lists them under **ESI access**);
- which Discord channels to give it, and any secret, such as an API key, to enter on its page.

## 7. Ship updates

Tether looks for newer versions in your repository once a day, while the instance's update checks are on. Admins review each one before it installs.

- Bump `version` for every release, and publish it the same way.
- Keep every migration that already ran, unchanged, and keep `storage` once your app has data.
- An upgrade that asks for anything new (a scope, a host, a secret, a permission, a page opened to more people) shows it to the admin. Until they approve, the old version keeps running.
- A permission you drop takes its grants with it. To rename one and keep its grants, use `[renamed_permissions]`.
- Admins can roll back one version. If your new migrations ran, the app's data goes back to the snapshot taken before them.

## Before you publish

AGENTS.md ends with a checklist. In short:

- It builds for `wasm32-wasip2` and passes clippy with warnings as errors.
- Every `render` returns quickly; long work belongs in jobs.
- Pages stay under Tether's limits with your biggest real data.
- No `std::fs`, `std::net` or `std::env`: a component that imports them is refused at load.
