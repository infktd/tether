#![allow(clippy::unwrap_used, clippy::expect_used)] // test code

//! A misbehaving guest can't hurt the host: runaway loops, memory bombs
//! and panics each end the one call, cleanly, and other calls carry on.

use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

mod common;

use common::build_guest;
use tether_plugins::{CallError, PluginLimits, Runtime, Sandbox};
use wasmtime::component::{Component, Linker};

wasmtime::component::bindgen!({
    world: "limits",
    path: "test-guest/wit",
    exports: { default: async },
});

fn guest() -> &'static [u8] {
    static BYTES: OnceLock<Vec<u8>> = OnceLock::new();
    BYTES.get_or_init(|| build_guest("tether-plugins-test-guest"))
}

struct Host {
    runtime: Runtime,
    component: Component,
    linker: Linker<Sandbox<()>>,
}

async fn host() -> Arc<Host> {
    // An explicit overall limit, so the tests behave the same on a 2-core
    // CI runner as on a laptop.
    let runtime = Runtime::new().unwrap().with_call_limit(8);
    let component = runtime.compile(guest().to_vec()).await.unwrap();
    let linker = runtime.linker::<()>().unwrap();
    Arc::new(Host {
        runtime,
        component,
        linker,
    })
}

/// Calls one export of the test guest as `plugin`.
macro_rules! call {
    ($host:expr, $plugin:expr, $limits:expr, $method:ident $(, $arg:expr)*) => {{
        let host = &$host;
        let store = host.runtime.store((), $limits);
        host.runtime
            .run($plugin, store, $limits, async |store| {
                let guest = Limits::instantiate_async(&mut *store, &host.component, &host.linker).await?;
                guest.$method(&mut *store $(, $arg)*).await
            })
            .await
    }};
}

fn limits(cpu_ms: u64, deadline_ms: u64, memory_mib: usize) -> PluginLimits {
    PluginLimits {
        memory_bytes: memory_mib * 1024 * 1024,
        cpu: Duration::from_millis(cpu_ms),
        deadline: Duration::from_millis(deadline_ms),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_well_behaved_call_returns() {
    let host = host().await;
    let out = call!(host, "a", &PluginLimits::default(), call_echo, "o7").unwrap();
    assert_eq!(out, "o7");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_infinite_loop_is_stopped_by_the_cpu_budget() {
    let host = host().await;
    let started = Instant::now();
    let err = call!(host, "a", &limits(200, 10_000, 64), call_spin).unwrap_err();
    assert_eq!(err, CallError::CpuLimit);
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "{:?}",
        started.elapsed()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_wall_clock_deadline_also_stops_a_loop() {
    let host = host().await;
    let started = Instant::now();
    let err = call!(host, "a", &limits(60_000, 300, 64), call_spin).unwrap_err();
    assert_eq!(err, CallError::Timeout(Duration::from_millis(300)));
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "{:?}",
        started.elapsed()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_memory_bomb_hits_the_cap() {
    let host = host().await;
    let capped = limits(2_000, 10_000, 32);
    assert_eq!(
        call!(host, "a", &capped, call_allocate, 8).unwrap(),
        8 * 1024 * 1024
    );
    let err = call!(host, "a", &capped, call_allocate, 256).unwrap_err();
    assert_eq!(err, CallError::OutOfMemory(32));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_panic_is_a_trap_and_the_next_call_is_fine() {
    let host = host().await;
    let err = call!(host, "a", &PluginLimits::default(), call_crash).unwrap_err();
    assert!(matches!(err, CallError::Trap(_)), "{err:?}");
    // Fresh store per call: nothing of the crash remains.
    assert_eq!(
        call!(host, "a", &PluginLimits::default(), call_echo, "still here").unwrap(),
        "still here"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_spinning_plugin_does_not_block_others() {
    let host = host().await;
    let spinner = {
        let host = host.clone();
        tokio::spawn(async move { call!(host, "spinner", &limits(1_500, 10_000, 64), call_spin) })
    };
    // Give it time to get going, then check another call still answers
    // promptly, even with only two worker threads.
    tokio::time::sleep(Duration::from_millis(100)).await;
    for _ in 0..3 {
        let started = Instant::now();
        let out = call!(host, "other", &PluginLimits::default(), call_echo, "ping").unwrap();
        assert_eq!(out, "ping");
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "{:?}",
            started.elapsed()
        );
    }
    assert_eq!(spinner.await.unwrap().unwrap_err(), CallError::CpuLimit);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deep_recursion_is_a_trap_not_a_host_crash() {
    let host = host().await;
    assert_eq!(
        call!(host, "a", &PluginLimits::default(), call_recurse, 100).unwrap(),
        100
    );
    // Rust keeps its stack in linear memory, so this ends as an
    // out-of-bounds trap (or wasm's own stack limit, for other guests):
    // either way the call fails and the host is untouched.
    let err = call!(
        host,
        "a",
        &PluginLimits::default(),
        call_recurse,
        10_000_000
    )
    .unwrap_err();
    assert!(matches!(&err, CallError::Trap(_)), "{err:?}");
    assert_eq!(
        call!(host, "a", &PluginLimits::default(), call_echo, "fine").unwrap(),
        "fine"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_plugin_gets_at_most_two_calls_at_once() {
    let host = host().await;
    let spin = limits(400, 10_000, 64);
    let spinners: Vec<_> = (0..2)
        .map(|_| {
            let (host, spin) = (host.clone(), spin.clone());
            tokio::spawn(async move { call!(host, "busy", &spin, call_spin) })
        })
        .collect();
    tokio::time::sleep(Duration::from_millis(50)).await;
    // A third call to the same plugin waits for a slot...
    let started = Instant::now();
    call!(host, "busy", &PluginLimits::default(), call_echo, "queued").unwrap();
    assert!(
        started.elapsed() >= Duration::from_millis(250),
        "{:?}",
        started.elapsed()
    );
    // ...while other plugins don't.
    let started = Instant::now();
    call!(
        host,
        "quiet",
        &PluginLimits::default(),
        call_echo,
        "straight in"
    )
    .unwrap();
    assert!(
        started.elapsed() < Duration::from_millis(250),
        "{:?}",
        started.elapsed()
    );
    for spinner in spinners {
        assert_eq!(spinner.await.unwrap().unwrap_err(), CallError::CpuLimit);
    }
}

#[tokio::test]
async fn oversized_components_are_refused_before_compiling() {
    let runtime = Runtime::new().unwrap();
    let huge = vec![0u8; tether_plugins::MAX_COMPONENT_BYTES + 1];
    assert!(matches!(
        runtime.compile(huge).await,
        Err(tether_plugins::RuntimeError::TooLarge(_))
    ));
}

#[tokio::test]
async fn a_plugin_that_imports_sockets_or_files_cannot_load() {
    let runtime = Runtime::new().unwrap();
    let component = runtime
        .compile(build_guest("tether-plugins-test-guest-net"))
        .await
        .unwrap();
    // The guest really does want the network (std::net imports sockets,
    // and the filesystem with them)...
    let imports: Vec<String> = component
        .component_type()
        .imports(runtime.engine())
        .map(|(name, _)| name.to_owned())
        .collect();
    assert!(
        imports.iter().any(|i| i.starts_with("wasi:sockets/")),
        "{imports:?}"
    );
    // ...and can't be linked, so it never runs.
    let linker = runtime.linker::<()>().unwrap();
    let err = linker
        .instantiate_pre(&component)
        .err()
        .expect("must not link");
    let text = err.to_string();
    assert!(
        text.contains("wasi:sockets") || text.contains("wasi:filesystem"),
        "{text}"
    );
}

#[tokio::test]
async fn a_plugin_defining_its_own_resources_is_refused() {
    // Handles to guest-defined resources are host memory the limits can't
    // reach: a test guest made about 5 million a second.
    let runtime = Runtime::new().unwrap();
    let err = runtime
        .compile(build_guest("tether-plugins-test-guest-hoard"))
        .await
        .unwrap_err();
    assert!(
        matches!(&err, tether_plugins::RuntimeError::Rejected(m) if m.contains("resource type")),
        "{err}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn jobs_dont_wait_for_a_busy_plugin_and_pages_dont_wait_for_jobs() {
    let host = host().await;
    let job = |plugin: &'static str| {
        let host = host.clone();
        async move {
            let limits = limits(400, 5_000, 64);
            let store = host.runtime.store((), &limits);
            host.runtime
                .run_as(
                    tether_plugins::CallKind::Job,
                    plugin,
                    store,
                    &limits,
                    async |store| {
                        let guest =
                            Limits::instantiate_async(&mut *store, &host.component, &host.linker)
                                .await?;
                        guest.call_spin(&mut *store).await
                    },
                )
                .await
        }
    };
    // A job spinning for its CPU budget...
    let running = tokio::spawn(job("a"));
    tokio::time::sleep(Duration::from_millis(100)).await;
    // ...a second job of the same plugin is turned away at once...
    let started = Instant::now();
    assert_eq!(job("a").await.unwrap_err(), CallError::Busy);
    assert!(started.elapsed() < Duration::from_millis(50));
    // ...and its pages still run.
    let out = call!(host, "a", &PluginLimits::default(), call_echo, "page").unwrap();
    assert_eq!(out, "page");
    assert_eq!(running.await.unwrap().unwrap_err(), CallError::CpuLimit);
}
