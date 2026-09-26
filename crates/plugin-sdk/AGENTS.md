# Writing a Tether plugin

This is the guide for anyone (person or AI agent) building a plugin with `tether-plugin-sdk`. The machine-readable contract is `wit/plugin.wit` in the Tether repository; this file explains how to use it.

## What a plugin is

A Rust library compiled to a WebAssembly component. Tether loads it at runtime, with no restart, and calls it inside a sandbox. A plugin:

- describes pages as data; the host draws them with its own templates, so every plugin looks native and no plugin code runs in a browser;
- can only call what the host API offers (see "What the host offers" below);
- never sees an ESI token, a secret, another plugin's data or Tether's own tables.

## Minimal plugin

`Cargo.toml`:

```toml
[package]
name = "my-plugin"
version = "0.1.0"
edition = "2024"

[lib]
crate-type = ["cdylib"]

[dependencies]
# Pin to a release tag once Tether publishes them.
tether-plugin-sdk = { git = "https://github.com/infktd/tether" }
```

`src/lib.rs`:

```rust
use tether_plugin_sdk::{Page, PageError, Plugin, Request, log};

struct MyPlugin;

impl Plugin for MyPlugin {
    fn render(request: Request) -> Result<Page, PageError> {
        match request.path.as_str() {
            "" => Ok(Page::new("My plugin").text("Hello.")),
            _ => Err(PageError::NotFound),
        }
    }
}

tether_plugin_sdk::export!(MyPlugin);
```

Build it:

```bash
rustup target add wasm32-wasip2
cargo build --target wasm32-wasip2 --release
# target/wasm32-wasip2/release/my_plugin.wasm is the component
```

No other tooling is needed: `wasm32-wasip2` produces a component directly. Forgetting `export!` only fails when linking the `.wasm`, so always build for `wasm32-wasip2` (and run `cargo clippy --target wasm32-wasip2`), not just for your own machine.

`examples/hello-plugin` in the repository is a complete example with every kind of section.

## plugin.toml

Every package carries a `plugin.toml`. Unknown fields are refused, so a typo fails loudly instead of silently dropping a capability.

```toml
[plugin]
id = "nmu.mining-ledger"   # 3-50 lowercase letters, digits and single . - _, starting with a letter
name = "Mining ledger"     # up to 60 characters
version = "0.3.1"          # MAJOR.MINOR.PATCH, no leading zeros
host_api = "1"
description = "Moon mining for the corp"                  # optional, up to 300 characters
repository = "https://github.com/example/mining-ledger"   # optional

[publisher]
key = "RWQ..."             # the second line of your minisign .pub file

[capabilities]             # all optional; ask only for what you use
storage = true
discord = ["send_message"]
http = ["janice.e-351.com"]         # exact HTTPS hostnames, at most 10

[capabilities.esi]
user = ["esi-wallet.read_character_wallet.v1"]              # Member requires these of every character
data_source = ["esi-industry.read_corporation_mining.v1"]  # characters an admin designates

[[capabilities.schedules]]
name = "sync_mining"
every = "30m"              # 5m to 7d: m, h or d

[capabilities.secrets.janice_api_key]   # at most 10
host = "janice.e-351.com"  # one of `http`
header = "X-ApiKey"        # the header it's sent in
# prefix = "Bearer "       # optional, put before the value

[permissions]              # granted like core ones, as plugin.<id>.<name>
view = "View the mining ledger"
manage = "Manage the mining ledger"
```

The admin sees every capability before approving an install. Secrets are values like API keys that the admin enters. Each goes to one declared host in one header (not `Cookie`, `Host` or headers that frame the request); the host adds it to your requests there, and your plugin never sees it. Names and descriptions can't contain control characters or invisible formatting (bidi overrides, zero-width characters).

## Packaging and signing

A package is a `.zip` with exactly these entries (anything else, symlinks, encryption, repeated names or an archive comment gets it refused):

| Entry | Limit |
| --- | --- |
| `plugin.toml` | 64 KiB |
| `plugin.wasm` | 32 MiB |
| `migrations/0001_<name>.sql`, `0002_...`, with no gaps; `<name>` is lowercase letters, digits and `_` | 100 files, 256 KiB each |
| `ui/<name>.png`, `.jpg`, `.jpeg`, `.webp` or `.gif` (really that type; no SVG) | 32 files, 1 MiB each |
| `rotation.txt` and `rotation.txt.minisig`, only when changing keys | 4 KiB each |

The whole package is at most 40 MiB. Compress with deflate or store uncompressed.

Sign the finished zip with [minisign](https://jedisct1.github.io/minisign/) and ship the signature next to it:

```bash
minisign -G -p tether.pub -s tether.key     # once; put the .pub's key line in plugin.toml
zip -X -r my-plugin.zip plugin.toml plugin.wasm migrations ui
minisign -S -s tether.key -m my-plugin.zip  # writes my-plugin.zip.minisig
```

The first install pins your key for your plugin id. Every later version must be signed with the same key. To move to a new key, put the new key in `plugin.toml`, sign the package with the new key, and include a statement signed with the old key:

```bash
printf 'tether-key-rotation v1\nplugin: %s\nold: %s\nnew: %s\n' nmu.mining-ledger "$OLD_KEY" "$NEW_KEY" > rotation.txt
minisign -S -s old.key -m rotation.txt      # writes rotation.txt.minisig
```

Keep the rotation files in later packages or drop them; either works once installs have moved to the new key. If you lose your key, admins have to re-pin it by hand, so keep a backup.

## Who sees what

A plugin's pages live at `/plugins/<id>/<path>`, for signed-in users only. `plugin.toml` says who may open which:

```toml
[permissions]
view = "See the mining ledger"
manage = "Change ledger settings"

[[pages]]              # the longest matching path wins
path = ""              # every page...
permission = "view"
[[pages]]
path = "settings"      # ...except settings/...
permission = "manage"

[[navigation]]         # sidebar links, shown to whoever may open them
label = "Mining"
path = ""
```

- Permissions are granted like Tether's own, to states and groups, as `plugin.<id>.<name>`.
- A page no `[[pages]]` rule covers is for admins only (`admin.plugins`), never for everyone. Declare a rule for every page people should see.
- Someone who may not open a page gets the same "nothing here" as for a page that doesn't exist; your plugin isn't called.
- Paths are link paths (see below). The query string is capped at 2 KiB and 20 pairs; `_tab` is the host's (which tab is showing) and never reaches you. Each person can open 120 of a plugin's pages a minute.

## Pages

`render` gets a `Request` (the path below the plugin's pages, and the query string) and returns a `Page` or a `PageError`.

- `PageError::NotFound` and `PageError::Forbidden` show the usual pages; `PageError::Failed(text)` shows a generic error to the user, and `text` to admins in the plugin's log.
- A page has a title, an optional one-line description, sections, and optional tabs (each with its own sections).
- Sections: a row of stats (`stats`, at most 8), a `table`, a `card` of label/value fields, a paragraph of `text`, or a `form`.
- Values are typed so the host formats them consistently: `Value::Text`, `Value::Number` (counts, IDs), `isk(amount)` (abbreviated in tables), `time(rfc3339)` (EVE time), `badge(label, tone)`, and `link(label, path)` to another page of the same plugin.
- Use `Tone::Accent` for the single most important thing on a screen, and nothing else.

Builders keep this short:

```rust
use tether_plugin_sdk::{Column, Stat, Table, Tone, badge, isk, time};

let table = Table::new(vec![Column::text("Moon"), Column::numeric("Value"), Column::numeric("Pops")])
    .title("Extractions")
    .empty("No extractions yet.")
    .row(vec!["1DQ1-A I - Moon 1".into(), isk(1.24e9), time("2026-09-24T18:00:00Z")]);

Page::new("Moons")
    .description("Extractions from our structures")
    .stats(vec![Stat::new("Ready", badge("3", Tone::Accent))])
    .table(table)
```

The host refuses a page (and logs why, for admins) if it breaks these rules:

| Rule | Limit |
| --- | --- |
| Title | not empty |
| Sections, counting those in tabs | 40 |
| Tabs | 10 |
| Stats in a row | 8 |
| Table columns | 1 to 20; every row has exactly one value per column |
| Table rows | 500 (paginate with links and the query string) |
| Card fields | 40 |
| Values (stats, cells, card fields) on a page | 10,000 |
| Any one piece of text | 2 KiB |
| The whole page: all text, link paths and times, plus 16 bytes per value | 1 MiB |
| ISK | a finite number |
| Times | a real instant in RFC 3339, e.g. `2026-09-24T18:00:00Z`, at most 40 bytes |
| Link paths | at most 200 bytes of relative segments made of ASCII letters, digits, `-`, `_`, `.`: no leading or trailing `/`, no empty segments, no `.` or `..` segments, no scheme, query or fragment |

## Storage

A plugin that declares `storage = true` gets its own Postgres schema, and SQL through `tether_plugin_sdk::storage`. Create tables with migrations in the package (`migrations/0001_create_notes.sql`, `0002_...`). The host runs each migration once, in its own transaction, as your plugin's database role; a migration can't be changed once applied, so fix mistakes in a new one.

```rust
use tether_plugin_sdk::storage::{self, Statement, Value};

storage::execute("INSERT INTO notes (body, at) VALUES ($1, $2)", &["o7".into(), Value::timestamp("2026-09-24T18:00:00Z")])?;
let rows = storage::query("SELECT id, body FROM notes ORDER BY id DESC LIMIT $1", &[20.into()])?;
for row in &rows.rows {
    let body = row[1].as_text().unwrap_or("");
}
storage::transaction(&[
    Statement::new("UPDATE stock SET qty = qty - $1 WHERE item = $2", vec![5.into(), "Veldspar".into()]),
    Statement::new("INSERT INTO moves (item, qty) VALUES ($1, $2)", vec!["Veldspar".into(), (-5).into()]),
])?;
```

- Always pass data as `$1`, `$2`... parameters, never by formatting it into SQL. One call runs one statement (`transaction` runs several, all or none).
- Your schema is on the search path, so use plain table names. You can't see Tether's tables, other plugins' schemas or `public`, can't make temporary tables or other schemas, and can't use advisory locks.
- Values: `Null`, `Boolean`, `Integer` (int2/4/8), `Float` (float4/8), `Text` (text, varchar, char; dates read as `YYYY-MM-DD`), `Bytes` (bytea), `Timestamp` (RFC 3339; timestamptz, and timestamp read as UTC) and `Json` (json, jsonb). Cast anything else in SQL: `amount::text` for numeric, `id::text` for uuid.
- `rows.columns` names the columns; it's empty when there are no rows.
- Errors: `NotApproved` (no `storage = true`), `Invalid` (a limit or a type), `Database` (Postgres's SQLSTATE code, e.g. `23505` for a duplicate, and message), `Timeout`, `TooLarge`.

| Limit | Value |
| --- | --- |
| One statement | 5 s (`statement_timeout`); waiting for a lock, 2 s |
| SQL per statement | 64 KiB |
| Parameters | 100 per statement, 1 MiB per call in all |
| Statements in a `transaction` | 50 |
| A result | 5,000 rows and 4 MiB (text, bytes and JSON, plus 16 bytes per value) |
| Memory per sort or hash | 16 MB (`work_mem`); temporary files 256 MB |
| One migration | 60 s per statement, 2 minutes in all |

Setting these yourself (`SET`, `set_config`, `ALTER ROLE`) doesn't lift them: the host puts them back before every statement. Uninstalling a plugin deletes its schema and everything in it.

## Jobs

Work that shouldn't wait for a page view, such as syncing from ESI or a ping at a set time, runs as a job. There are two kinds, and both arrive at `run_job`:

- **Schedules**, declared in `plugin.toml` (`[[capabilities.schedules]]`, `every = "30m"`, from 5 minutes to 7 days). They run while the plugin is enabled.
- **One-off jobs**, queued from a form submission or another job with `jobs::enqueue`. Give one a key to be able to move or cancel it: queuing under the same key replaces the queued job, and `jobs::cancel(key)` removes it.

```rust
use tether_plugin_sdk::jobs::{self, Job, JobError, NewJob};
use tether_plugin_sdk::{Page, PageError, Plugin, Request};

struct Moons;

impl Plugin for Moons {
    fn render(request: Request) -> Result<Page, PageError> { /* ... */ }

    fn run_job(job: Job) -> Result<(), JobError> {
        match job.name.as_str() {
            "sync" => {
                // For each extraction: a ping when the chunk arrives. Queuing
                // again after a reschedule moves it.
                jobs::enqueue(
                    NewJob::new("ping")
                        .key("moon:40161234")
                        .payload(r#"{"moon": 40161234}"#)
                        .at("2026-09-30T18:05:00Z"),
                ).map_err(|e| JobError::Retry(format!("{e:?}")))?;
                Ok(())
            }
            "ping" => Ok(()),
            other => Err(JobError::Permanent(format!("no job {other}"))),
        }
    }
}
```

- `job.scheduled_at` is when it was meant to run. After downtime, a restart or retries it runs late, so check it when timing matters (a ping for a chunk that arrived hours ago may not be worth sending).
- Return `JobError::Retry` for trouble that may pass (retried with backoff, up to 5 tries) and `JobError::Permanent` for a job that will never work. Hitting a limit or trapping counts as a retry.
- A job whose plugin is disabled or still loading waits for it without using up its tries; uninstalling removes all of the plugin's jobs.
- A time in the past means "as soon as possible": it runs after jobs already waiting, and `scheduled_at` still says the time you gave. One job of a plugin runs at a time, on workers apart from Tether's own and from page views.

| Limit | Value |
| --- | --- |
| One job run | 60 s in all, 10 s of CPU, 64 MiB of memory |
| Queued and running jobs | 1,000 per plugin (replacing a queued one by key still works at the limit) |
| Jobs created | 5,000 per plugin per day, finished ones included; finished jobs are kept a day |
| How far ahead | 120 days |
| Payload | JSON, at most 64 KiB |
| Name | lowercase letters, digits and `_`, at most 40 |
| Key | ASCII letters, digits and `. _ : -`, at most 100 |
| `enqueue` and `cancel` calls | 100 per form submission or job run (pages can't queue) |

## Forms

A form is part of a page. When someone posts it, the host checks the values against the form as your page draws it right then, and only then calls your `submit`:

```rust
use tether_plugin_sdk::{Field, Form, Page, PageError, Plugin, Request, SubmitResult, Submission};

fn render(request: Request) -> Result<Page, PageError> {
    Ok(Page::new("Settings").form(
        Form::new("threshold", "Save")
            .field(Field::number("isk", "Ping above (ISK)").range(Some(0.0), None, true).required())
            .field(Field::select("ore", "Ore", vec![("ubiquitous".into(), "Ubiquitous".into()), ("r64".into(), "R64".into())]))
            .field(Field::checkbox("enabled", "Pings on", true)),
    ))
}

fn submit(submission: Submission) -> Result<SubmitResult, PageError> {
    // Checked and written one way by the host: a whole number here.
    let isk: i64 = submission
        .value("isk")
        .parse()
        .map_err(|_| PageError::Failed("isk wasn't a number".into()))?;
    let on = submission.checked("enabled");
    // ... store it ...
    Ok(SubmitResult::Redirect("".into())) // or SubmitResult::Page(...) to show something
}
```

- Pages are read-only: `render` runs on plain page views, which a link on another site can trigger, so storage refuses writes there and `jobs::enqueue`/`cancel` fail. Change things in `submit` (checked for coming from Tether's own pages) or in jobs.
- Build a form's limits and options from what you've stored, never from the request: the host checks a post against the form your page draws for that same request.
- The host refuses, before `submit` is called: unknown fields, a field twice, missing required fields, text over its `max_length`, numbers that aren't numbers, out of range or (for `integer`) not whole, and select values that aren't one of the options. You get one value per field in the form's order: checkboxes as `true`/`false` (a required one must be ticked), empty optional fields as `""`, numbers written plainly (`100`, not `1e2`; whole numbers for `integer`).
- Posting needs the page's permission, comes from Tether's own pages only, and is limited to 30 a minute per person per plugin. No file uploads.
- Field names and form ids are lowercase letters, digits and `_`, starting with a letter. Up to 30 fields per form, 100 options per select, 10,000 characters per text field; a post is at most 64 KiB.
- Return `SubmitResult::Redirect(path)` to go to another of your pages (a link path), or `SubmitResult::Page(page)` to show a page there and then, for example the form again with a note about what to fix.

## Who's looking

`identity::viewer()` says who is looking at a page or posting a form: their account id, main, all their characters (with corporation and alliance), access state (`viewer.state.name`, and `viewer.is_member()` / `viewer.is_guest()`; admins can add states above Member, such as a leadership state, so `is_member()` is false for them: gate on your own permissions rather than on state where you can), and which of your plugin's permissions they hold (`viewer.can("manage")`). Jobs have no viewer.

## ESI

Plugins never see a token or build an ESI URL. You name an endpoint and whose token to use; the host checks, on every call, that:

- the endpoint is one Tether offers plugins (below) and its scope is declared in your `plugin.toml` and was approved;
- for a **user** scope (`capabilities.esi.user`): the character is a Member's and its token carries the scope. Installing your plugin makes Member require your user scopes, so Members register every character with them (those who haven't are flagged for officers): there's no per-plugin opt-in or opt-out. Only scopes a character endpoint below uses are accepted. Keep the list short; each scope asks every member for more;
- for a **data-source** scope (`capabilities.esi.data_source`): the character was offered as your data source by its owner and approved by an admin. Corporation endpoints read that character's corporation.

```rust
use tether_plugin_sdk::esi::{self, Subject};

// Corporation data, through each approved data source (e.g. a Station Manager).
for source in esi::data_sources() {
    for body in esi::get_all("corporation-mining-extractions", Subject::DataSource(source.id), &[])? {
        let extractions: Vec<serde_json::Value> = serde_json::from_str(&body)?;
    }
}
// Members' own data: every Member character registered with your scopes.
for character in esi::characters() {
    let skills = esi::get("character-skills", Subject::Character(character.id), &[], None)?;
}
let names = esi::names(&[40161234, 30000142])?;
```

| Endpoint | Scope | Subject | Paged | Params |
| --- | --- | --- | --- | --- |
| `corporation-mining-extractions` | `esi-industry.read_corporation_mining.v1` | data source | yes | |
| `corporation-mining-observers` | `esi-industry.read_corporation_mining.v1` | data source | yes | |
| `corporation-mining-observer` | `esi-industry.read_corporation_mining.v1` | data source | yes | `observer_id` |
| `corporation-structures` | `esi-corporations.read_structures.v1` | data source | yes | |
| `corporation-roles` | `esi-corporations.read_corporation_membership.v1` | data source | no | |
| `universe-moon` | `esi-industry.read_corporation_mining.v1` | data source | no | `moon_id` |
| `character-skills` | `esi-skills.read_skills.v1` | character | no | |
| `character-assets` | `esi-assets.read_assets.v1` | character | yes | |
| `character-wallet` | `esi-wallet.read_character_wallet.v1` | character | no | |
| `character-wallet-journal` | `esi-wallet.read_character_wallet.v1` | character | yes | |
| `character-clones` | `esi-clones.read_clones.v1` | character | no | |
| `character-implants` | `esi-clones.read_implants.v1` | character | no | |
| `character-location` | `esi-location.read_location.v1` | character | no | |

- The body is ESI's JSON, at most 4 MiB; `pages` says how many pages a paged endpoint has. At most 100 ESI calls per submit or job run, 20 per page render.
- Errors: `NotAllowed` (endpoint or scope), `NotRegistered` (not a Member's character, or its token lacks the scope), `NotADataSource`, `Token` (the character must log in again), `Status(code)` from ESI, `Invalid`, `TooLarge`, `Unavailable`. Plan for `NotRegistered` and `Token`: people leave, and revoke tokens.
- Corporation endpoints also need the character to hold the in-game role CCP requires (Station Manager for extractions and structures, Accountant for observers, Director or Personnel Manager for roles); without it ESI answers 403. `universe-moon` is public data, read through a data source only because `names` doesn't cover moons.
- Every call is recorded in your plugin's access log, which admins see. An admin must also enable your scopes on Tether's EVE application.

## Discord

With `discord = ["send_message"]`, a plugin can post to the channels an admin assigned it (`discord::channels()`):

```rust
use tether_plugin_sdk::discord::{self, Mention};
let channel = discord::channels().first().map(|c| c.id.clone());
discord::send(&channel.unwrap(), "Moon popped at 1DQ1-A I", Mention::State("Member".into()))?;
```

- Mentions are only the Discord role Tether maps to a state, named like `Mention::State("Member".into())`; typed `@everyone` and `@here` are defused, and nobody else can be pinged.
- From `submit` and jobs only, not pages. At most 1,500 characters, 5 messages per call and 20 a minute per plugin.

## Logging

`log::debug`, `log::info`, `log::warn` and `log::error` write to the plugin's log, which admins see on the plugin's page (the newest 1,000 lines are kept). The host keeps the first 100 lines per call, each cut to 1,024 characters, with control characters and invisible formatting characters replaced. The text of `PageError::Failed` is treated the same way. Never log anything personal you don't need.

## Limits

Every call runs in a fresh sandbox; nothing is kept between calls (state goes in storage). Per call:

| Limit | Default |
| --- | --- |
| Memory, all of it | 64 MiB |
| CPU time | 2 s |
| Whole call, including host calls | 10 s |
| Calls of one plugin at once | 2 (more wait) |
| Component size | 32 MiB |
| Data copied out of the plugin per call (its page, its log lines) | 16 MiB; past it, the call fails |

There is no filesystem, no network access of your own, no environment variables and no stdio (writes to stdout and stderr are discarded; use `log`). Clocks and random numbers work. A plugin that imports the filesystem or sockets (for example by using `std::fs` or `std::net`) is refused at load, as is one that defines its own component resource types.

Hitting a limit ends that call only; the next call starts clean. Admins see which limit was hit.

## What the host offers

API version 1 (`host_api = "1"` in `plugin.toml`, WIT package `tether:plugin@1.0.0`) is unstable until Tether's milestone 3 ends. After that, nothing in 1.x changes: new things arrive as new types, functions or interfaces, so a plugin built against an earlier 1.x keeps loading. Today it has:

- `log`: write to the plugin's log;
- pages: `render`, and forms: `submit`;
- `storage`: SQL in the plugin's own schema (see Storage);
- `jobs`: schedules and one-off jobs (see Jobs);
- `identity`, `esi` and `discord` (see above).

Coming during milestone 2: outbound HTTP to hosts an admin approved.

## Checklist before publishing

- Builds with `cargo build --target wasm32-wasip2 --release` and passes `cargo clippy --target wasm32-wasip2 -- -D warnings`.
- Every `render` path returns quickly; long work belongs in background jobs.
- Pages stay under the limits above for your biggest real data.
- No `std::fs`, `std::net` or `std::env`.
