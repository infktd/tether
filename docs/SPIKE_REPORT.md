# Plugin runtime spike report

Sep 24, 2026 · branch `spike/plugin-runtime` · code in `spike/` (not for merge)

## Summary

The spike met its acceptance criterion. A Rust plugin compiled to a WASM component calls a host function, has the host fetch ESI `/status` for it, and returns a declarative page that the host renders as HTML with the DESIGN.md tokens. Everything worked on the first real attempt. The friction was small, local, and fixable in an afternoon.

**Recommendation: continue with the WASM component model on Wasmtime.** Don't switch to Extism or Deno for v1. The reasoning and the risks that milestone 2 should retire first are at the end.

## What was built

```text
spike/
  wit/plugin.wit        # tether:spike@0.1.0: log, esi, page interfaces; `plugin` world
  plugin/               # guest: wit-bindgen, cdylib, built for wasm32-wasip2
  host/src/runtime.rs   # Engine + Linker, per-call Store, host impls of log and esi
  host/src/page.rs      # validation of plugin page descriptions
  host/src/web.rs       # axum: /health, /plugins/{id}
  host/templates/       # askama: base shell, plugin page, error page
  host/tests/           # integration tests; wiremock ESI with recorded fixture
```

The WIT contract:

| Interface | Direction | Shape |
| --- | --- | --- |
| `log` | import | `write(level, message)`, forwarded to `tracing` with the plugin id, and returned with each call so tests and an admin panel can see it |
| `esi` | import | `get-server-status() -> result<server-status, esi-error>`, typed record; the plugin never sees a URL, client, header or token |
| `page` | types | `page { title, description, sections: list<section> }`, where `section` is `stats(list<stat>)` or `table(table)` and columns carry a `numeric` flag |
| world export | export | `render-page() -> result<page, string>` |

The request path for `GET /plugins/spike.example`:

1. The host creates a fresh `Store` with an empty WASI context: no preopens, env, stdio or sockets.
2. It instantiates the precompiled component and calls `render-page`.
3. The guest calls `esi.get-server-status`. The host serves it through a shared `eve_esi_client::Client`, so every plugin shares one rate limiter and response cache.
4. The guest returns a `page`. The host validates it (title, row widths, size caps) and renders it with askama, which escapes all plugin text.
5. If the plugin reports an ESI failure, the host returns a 502 page. A trap or an invalid page gives a 500, and an unknown plugin id gives a 404.

Tests (15, all passing): the host/guest round trip including log records, per-call isolation, a typed ESI error on a 503, HTML rendering, escaping of hostile ESI data, a check that the HTML contains no `http(s)://` references, plus the 502, 404 and page-validation cases. The integration tests build the guest themselves, so `cargo test --workspace` is self-contained.

## Measurements

Apple M1 Pro (10 cores), macOS, rustc 1.98.1, wasmtime 49, no sccache or alternate linker.

**Build times**

| Build | Time |
| --- | --- |
| Guest, clean, release | 11.2 s |
| Guest, incremental (edit `lib.rs`) | 0.8 s |
| Host, clean, debug | 79.0 s |
| Host, clean, release | 132.6 s |
| Host, incremental debug (edit `web.rs`) | 4.4 s |
| Host, incremental debug (edit a template) | 2.6 s |
| Host, incremental release (edit `web.rs`) | 14.3 s |
| Host, `cargo check` warm | 1.2 s |
| `cargo test --workspace`, warm | 2.0 s |

Editing the `.wit` file correctly triggers a rebuild of exactly one crate on each side.

The clean build is dominated by a few crates (seconds, debug / release):

| Crate | Debug | Release | Pulled in by |
| --- | --- | --- | --- |
| `aws-lc-sys` (C build script) | 39.4 | 76.1 | reqwest 0.13 → rustls → aws-lc-rs, via eve-esi-client |
| `cranelift-codegen` | 21.2 | 74.2 | wasmtime |
| `eve-esi-client` (188k generated lines) | 20.4 | 31.3 | us |
| `wasmtime-wasi` | 14.9 | 27.5 | us |
| `wast` (WAT text parser) | 7.5 | 27.9 | wasmtime default `wat` feature, unused |
| `zstd-sys` | 11.4 | n/a | wasmtime default `cache` feature, unused |

The spike's `target/` directory reached **16 GiB** before cleaning (debug, release and wasm builds together). That matters for CI cache size and for anyone building on a small VPS.

**Sizes**

| Artifact | Size |
| --- | --- |
| Guest component, default release profile | 97 KB (74 KB before it used ESI and pages) |
| Guest, `opt-level="z"` + LTO + strip | 68 KB |
| Guest, default release, gzipped | 35 KB |
| Host binary, release | 33 MB |
| Host binary, release, stripped | 24 MB |

**Startup and runtime (release host, live ESI)**

| Measure | Value |
| --- | --- |
| Compile the plugin component at load (Cranelift) | 24 ms |
| Process start to first `/health` response, warm | 57–73 ms |
| First plugin page, including live ESI round trip | 444 ms |
| Plugin page, ESI served from cache, sequential (n=300) | p50 0.24 ms, p95 6.2 ms |
| Same, 16 concurrent clients (n=400) | p50 0.24 ms, p95 1.7 ms |
| RSS idle | 32 MB |
| RSS after 700 page requests | 37 MB |

Each page timing covers a fresh instance, the guest call, the host ESI call, validation and template render. The first launch of a freshly built binary took 813 ms to reach `/health`; that was macOS scanning a new executable, not the host. I didn't investigate the sequential p95 tail. It is most likely the ESI client revalidating `/status`, which has a 30 s max-age.

These numbers leave plenty of headroom against N11 (under 300 MB idle) and N12 (under 500 ms pages).

## What worked

- **No extra tooling.** `cargo build --target wasm32-wasip2` emits a component directly, so there's no `cargo-component`, `wasm-tools` or adapter step. That's one `rustup target add` and nothing else, which suits a plugin SDK aimed at people and AI agents alike.
- **WIT as the contract.** Records, variants, `option` and `result` map cleanly to Rust on both sides. The plugin writes `esi::get_server_status()?` and gets a typed struct. This is the machine-readable contract ARCHITECTURE.md wants for an AI-friendly SDK.
- **Async host, sync guest.** Setting `imports: { default: async }` in `bindgen!` lets host functions await network I/O while the guest sees an ordinary blocking call. No component-model-async (WASIp3) features were needed.
- **Instance per request is cheap.** At about 0.24 ms per full page, we can instantiate fresh for every call and never reason about state leaking between users.
- **Capability-based by default.** The guest links only what we add to the `Linker`. An empty `WasiCtx` gives the plugin no filesystem, env, stdio or network; it can still read clocks and get random bytes. Every ESI call already passes through one host function that knows the plugin id: the natural place for the consent and audit checks in N8 and N10.
- **Declarative pages.** A plugin returning data, with the host owning HTML, felt natural and made escaping and opsec easy to test.
- **eve-esi-client** worked first try, including the compatibility-date header, User-Agent enforcement, response caching, and a base URL override for wiremock.

## What fought back

None of these blocked progress for long, but each is worth knowing before milestone 2.

1. **wasmtime 49 has its own `Error` type**, no longer an alias for `anyhow::Error`. `?` still converts, but `anyhow::Context` doesn't apply directly, so errors need `.map_err(anyhow::Error::from)` first. Wasmtime ships a major version roughly monthly with real API churn, so pin it and upgrade deliberately.
2. **WIT has one namespace per interface.** A record and a function can't share a name (`server-status`), hence `get-server-status`.
3. **`use page.{page}` in a world implicitly imports the `page` interface.** The host needs an empty `impl page::Host`, and `Page` is generated at the world root rather than in the `page` module, on both host and guest. It's non-obvious, but the compiler error pointed to it.
4. **A missing `export!` fails at link time, not compile time.** A native build or clippy only shows dead-code warnings, and the real error ("failed to find export of function `render-page`") comes from `wasm-component-ld`. CI must build and lint guests for `wasm32-wasip2`, not just the host target.
5. **Two clippy passes.** The guest needs `cargo clippy --target wasm32-wasip2` in addition to the workspace run.
6. **eve-esi-client loses the status code** when an error response body isn't valid JSON. An empty 503 or a proxy's HTML 502 becomes `InvalidResponsePayload` with no status. Real ESI 5xx responses carry JSON, so it's minor, but worth fixing in the crate.
7. **Build weight.** `aws-lc-sys` is the single slowest crate and is a C build, which matters for the multi-arch Docker builds in N4 (arm64 and amd64 need a working C toolchain in the builder). Wasmtime's default features also pull in `wast` and `zstd`, which we don't need.

## Recommendation

**Continue with the WASM component model on Wasmtime for v1.**

The spike's main question was whether the component model is too immature or too fiddly to build a platform on. It isn't. The toolchain is now plain Cargo, the typed contract is the strongest part of the design, and performance is a non-issue at our scale.

Compared with the alternatives (this comparison is from their design and what I know of them, not measured in this spike):

- **Extism** runs on Wasmtime too, so it gives no isolation benefit. Its host/guest boundary is bytes in, bytes out, with serialization per call and a schema layer on top. We would trade WIT's compiler-checked contract, the main thing that makes the SDK safe for AI-written plugins, for easier multi-language plugin kits we don't need in v1, since first-party plugins are Rust.
- **Deno (V8 isolates)** is the right tool for the TypeScript tier planned in phase 4, and nothing here changes that. As the v1 runtime it would add a much heavier dependency, slower cold starts, higher memory per isolate, and a larger attack surface, for no gain while all plugins are Rust.

### Risks for milestone 2 to retire first

The spike didn't exercise these. Each should get a test before the plugin host API is built out.

1. **Resource limits.** Epoch interruption or fuel against an infinite-loop guest, a memory cap via `StoreLimits`, and the per-call overhead of each. This is the biggest untested piece.
2. **Versioning.** How `host_api = "1"` maps to WIT package versions, and whether the host can link two world versions at once so old plugins keep working across host upgrades.
3. **Hot install.** `Component::from_file` at runtime takes 24 ms and needs no restart. Caching compiled code with `Component::serialize` and swapping a plugin while requests are in flight still need a test.
4. **Storage interface.** This is the open question in the PRD: raw SQL in the plugin's schema, or a narrower query API. The host-function pattern here works for either.
5. **Build hygiene.** Trim wasmtime to the features we use (at least drop `wat`, `cache`, `profiling`). Check whether eve-esi-client can use `ring` or expose TLS backend features to avoid the `aws-lc-sys` C build in multi-arch images. Configure sccache or mold per CLAUDE.md before the workspace grows.

### Notes for milestone 1 (ESI layer)

eve-esi-client's in-memory cache and rate limiter are per `Client`. ARCHITECTURE.md wants a Postgres `esi_cache` shared across restarts, and per-plugin budget shares. The crate already has `.http_cache(false)`. Per-plugin budgets will need a hook, or a wrapper around the client that knows the calling plugin, which the host function naturally provides.

## How to run the spike

```bash
cd spike
cargo build -p spike-plugin --target wasm32-wasip2 --release
cargo run -p spike-host
# open http://127.0.0.1:3000/plugins/spike.example
```

`SPIKE_ADDR`, `SPIKE_PLUGIN` and `SPIKE_USER_AGENT` override the listen address, the component path and the ESI User-Agent.
