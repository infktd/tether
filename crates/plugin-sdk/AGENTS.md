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

## Pages

`render` gets a `Request` (the path below the plugin's pages, and the query string) and returns a `Page` or a `PageError`.

- `PageError::NotFound` and `PageError::Forbidden` show the usual pages; `PageError::Failed(text)` shows a generic error to the user, and `text` to admins in the plugin's log.
- A page has a title, an optional one-line description, sections, and optional tabs (each with its own sections).
- Sections: a row of stats (`stats`, at most 8), a `table`, a `card` of label/value fields, or a paragraph of `text`.
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

## Logging

`log::debug`, `log::info`, `log::warn` and `log::error` write to the plugin's log, which admins see. The host keeps the first 100 lines per call, each cut to 1,024 characters, with control characters and invisible formatting characters replaced. The text of `PageError::Failed` is treated the same way. Never log anything personal you don't need.

## Limits

Every call runs in a fresh sandbox; nothing is kept between calls (state goes in storage, when that arrives). Per call:

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
- pages: `render`.

Coming during milestone 2, in this order: storage (SQL in the plugin's own schema), background jobs and schedules, permissions and forms, ESI data (within approved and consented scopes), identity, Discord messages, and outbound HTTP to hosts an admin approved.

## Checklist before publishing

- Builds with `cargo build --target wasm32-wasip2 --release` and passes `cargo clippy --target wasm32-wasip2 -- -D warnings`.
- Every `render` path returns quickly; long work belongs in background jobs.
- Pages stay under the limits above for your biggest real data.
- No `std::fs`, `std::net` or `std::env`.
