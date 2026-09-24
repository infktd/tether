//! The plugin runtime (F17, N8): Wasmtime running each plugin call in a
//! fresh, locked-down sandbox.
//!
//! Every call gets its own [`Store`], consumed by the call, so nothing
//! survives between calls or leaks between users. Each store has:
//!
//! - a memory cap for the whole call (all of the plugin's linear memories
//!   together): growing past it fails, and the call ends as
//!   [`CallError::OutOfMemory`];
//! - a CPU budget: a ticker thread advances Wasmtime's epoch every
//!   [`TICK`]; the guest yields to the async runtime on each tick (so a
//!   spinning plugin never pins a worker thread) and is interrupted when
//!   its ticks run out ([`CallError::CpuLimit`]);
//! - a wall-clock deadline for the whole call, host calls included
//!   ([`CallError::Timeout`]);
//! - caps on tables, instances and host resources (handles such as
//!   streams and pollables), which live outside linear memory;
//! - only the WASI a Rust plugin needs (I/O streams, clocks, randomness,
//!   CLI plumbing), backed by an empty context: no files, environment,
//!   arguments, stdio or network. Filesystem and sockets aren't linked.
//!
//! Calls also queue for a permit: a few at a time per plugin, and no more
//! than half the machine's cores across all plugins, so many slow calls
//! can't swamp the web server. Compiling is bounded by size and runs one
//! at a time on a blocking thread.
//!
//! Components that define their own resource types are refused at load:
//! their handles live in host memory that no limit here can reach (Wasmtime
//! has no cap on them; a test guest made about 5 million a second).
//!
//! What a plugin can call beyond that is decided by the linker the host
//! API (the WIT world) builds on top of [`Runtime::linker`].

pub mod host;
pub mod manifest;
pub mod package;
pub mod page;
pub mod storage;
#[cfg(feature = "testing")]
pub mod testing;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::Semaphore;
use wasmtime::component::types::ComponentItem;
use wasmtime::component::{Component, HasData, Linker, ResourceTable};
use wasmtime::{Config, Engine, ResourceLimiter, Store, Trap, UpdateDeadline};
use wasmtime_wasi::{WasiCtx, WasiCtxView, WasiView};

/// How often the epoch advances. The CPU budget is counted in these.
pub const TICK: Duration = Duration::from_millis(10);
/// Largest component accepted for compiling.
pub const MAX_COMPONENT_BYTES: usize = 32 * 1024 * 1024;
/// Calls one plugin may have running at once.
pub const CALLS_PER_PLUGIN: usize = 2;
/// Host resources (streams, pollables, host API handles) per call.
const MAX_HOST_RESOURCES: usize = 1_000;
/// Random bytes per request (wasmtime-wasi's default is 64 MiB).
const MAX_RANDOM_BYTES: u64 = 64 * 1024;
/// Data copied from a plugin into the host per call (its page, its log
/// lines): Wasmtime's own default is 128 MiB. Past it, the call traps.
pub const MAX_HOSTCALL_BYTES: usize = 16 * 1024 * 1024;
/// Longest trap message kept.
const MAX_TRAP_TEXT: usize = 2 * 1024;

/// Limits for one plugin call.
#[derive(Debug, Clone)]
pub struct PluginLimits {
    /// Linear memory for the whole call, all memories together, in bytes.
    pub memory_bytes: usize,
    /// Time spent running guest code.
    pub cpu: Duration,
    /// The whole call, including time waiting on host functions.
    pub deadline: Duration,
}

impl Default for PluginLimits {
    fn default() -> Self {
        Self {
            memory_bytes: 64 * 1024 * 1024,
            cpu: Duration::from_secs(2),
            deadline: Duration::from_secs(10),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    #[error("setting up the plugin engine: {0}")]
    Engine(String),
    #[error("not a valid plugin component: {0}")]
    Compile(String),
    #[error("the component is {0} bytes; the most accepted is {MAX_COMPONENT_BYTES}")]
    TooLarge(usize),
    #[error("the plugin can't be loaded: {0}")]
    Rejected(String),
    #[error("linking the host API: {0}")]
    Link(String),
}

/// Why a plugin call didn't return normally.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CallError {
    #[error("the plugin used more than its {0} MiB of memory")]
    OutOfMemory(usize),
    #[error("the plugin ran longer than its CPU budget")]
    CpuLimit,
    #[error("the plugin call took longer than {0:?}")]
    Timeout(Duration),
    /// A panic, an `unreachable`, a bad host call: the plugin's fault. The
    /// text is for admins (it can include names the plugin chose); users
    /// should only see that the plugin failed.
    #[error("the plugin crashed: {0}")]
    Trap(String),
}

/// Memory and table caps for one store. Counts the total across every
/// memory, and records when the cap was hit, so an out-of-memory abort
/// isn't mistaken for any other crash.
#[derive(Debug)]
struct Limiter {
    memory_bytes: usize,
    used: usize,
    exceeded: bool,
}

impl Limiter {
    fn new(memory_bytes: usize) -> Self {
        Self {
            memory_bytes,
            used: 0,
            exceeded: false,
        }
    }
}

impl ResourceLimiter for Limiter {
    fn memory_growing(
        &mut self,
        current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        // Called for every memory, including when one is first created
        // (current = 0), so `used` is the call's total.
        let total = self.used.saturating_sub(current).saturating_add(desired);
        if total > self.memory_bytes {
            self.exceeded = true;
            return Ok(false);
        }
        self.used = total;
        Ok(true)
    }

    fn table_growing(
        &mut self,
        _current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        Ok(desired <= 10_000)
    }

    fn instances(&self) -> usize {
        8
    }

    fn tables(&self) -> usize {
        8
    }

    fn memories(&self) -> usize {
        4
    }
}

/// A store's data: the host API's per-call state `T`, plus the sandbox's
/// own parts.
pub struct Sandbox<T> {
    pub data: T,
    limiter: Limiter,
    wasi: WasiCtx,
    table: ResourceTable,
}

impl<T: Send> WasiView for Sandbox<T> {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

/// The engine every plugin runs on, the epoch ticker, and the queues calls
/// and compiles wait in.
pub struct Runtime {
    engine: Engine,
    calls: Arc<Semaphore>,
    per_plugin: Mutex<HashMap<String, Arc<Semaphore>>>,
    compiles: Arc<Semaphore>,
}

impl std::fmt::Debug for Runtime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Runtime").finish_non_exhaustive()
    }
}

impl Runtime {
    pub fn new() -> Result<Self, RuntimeError> {
        let mut config = Config::new();
        config.epoch_interruption(true);
        let engine = Engine::new(&config).map_err(|e| RuntimeError::Engine(e.to_string()))?;
        // A real thread, not a task: a spinning guest runs inside a poll and
        // would starve a ticker on the same async worker. It runs as long as
        // anything (a compiled component, a store) still holds the engine.
        let weak = engine.weak();
        std::thread::Builder::new()
            .name("plugin-epoch".to_owned())
            .spawn(move || {
                loop {
                    std::thread::sleep(TICK);
                    match weak.upgrade() {
                        Some(engine) => engine.increment_epoch(),
                        None => break,
                    }
                }
            })
            .map_err(|e| RuntimeError::Engine(format!("starting the epoch ticker: {e}")))?;
        let cores = std::thread::available_parallelism().map_or(2, |n| n.get());
        Ok(Self {
            engine,
            // Half the cores: guests yield every tick, but the web server
            // shares these workers.
            calls: Arc::new(Semaphore::new((cores / 2).max(1))),
            per_plugin: Mutex::new(HashMap::new()),
            compiles: Arc::new(Semaphore::new(1)),
        })
    }

    /// Sets how many plugin calls may run at once across all plugins.
    pub fn with_call_limit(mut self, calls: usize) -> Self {
        self.calls = Arc::new(Semaphore::new(calls.max(1)));
        self
    }

    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    /// Compiles a component: size-checked, one at a time, on a blocking
    /// thread (Cranelift can take seconds on a large component), then
    /// vetted ([`Runtime::vet`]). Only compile packages whose signature has
    /// been verified. Keep the result and reuse it for every call.
    pub async fn compile(&self, bytes: Vec<u8>) -> Result<Component, RuntimeError> {
        if bytes.len() > MAX_COMPONENT_BYTES {
            return Err(RuntimeError::TooLarge(bytes.len()));
        }
        // The permit moves into the blocking job: a compile can't be
        // cancelled, so the slot is only free once it has really finished.
        let permit = self
            .compiles
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| RuntimeError::Engine("the runtime is shutting down".to_owned()))?;
        let engine = self.engine.clone();
        let component = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            Component::from_binary(&engine, &bytes)
        })
        .await
        .map_err(|e| RuntimeError::Compile(format!("the compiler stopped: {e}")))?
        .map_err(|e| RuntimeError::Compile(e.to_string()))?;
        self.vet(&component)?;
        Ok(component)
    }

    /// Refuses a component that defines its own resource types (anywhere in
    /// its exports): handles to them are host memory outside the sandbox's
    /// limits, and the host API has no use for them.
    pub fn vet(&self, component: &Component) -> Result<(), RuntimeError> {
        fn walk(engine: &Engine, name: &str, item: &ComponentItem) -> Result<(), String> {
            match item {
                ComponentItem::Resource(_) => Err(name.to_owned()),
                ComponentItem::ComponentInstance(instance) => instance
                    .exports(engine)
                    .try_for_each(|(n, e)| walk(engine, &format!("{name}/{n}"), &e.ty)),
                ComponentItem::Component(inner) => inner
                    .exports(engine)
                    .try_for_each(|(n, e)| walk(engine, &format!("{name}/{n}"), &e.ty)),
                _ => Ok(()),
            }
        }
        component
            .component_type()
            .exports(&self.engine)
            .try_for_each(|(name, export)| walk(&self.engine, name, &export.ty))
            .map_err(|what| {
                RuntimeError::Rejected(format!(
                    "it defines its own resource type ({what}), which plugins may not"
                ))
            })
    }

    /// A linker with just the WASI a Rust plugin needs: I/O streams,
    /// clocks, randomness, and the CLI plumbing std imports (stdio, exit,
    /// environment), all backed by an empty context. Filesystem and sockets
    /// aren't linked at all, so a plugin importing them can't even load.
    /// The host API adds its own interfaces on top.
    pub fn linker<T: Send + 'static>(&self) -> Result<Linker<Sandbox<T>>, RuntimeError> {
        let mut linker = Linker::new(&self.engine);
        link_wasi(&mut linker).map_err(|e| RuntimeError::Link(e.to_string()))?;
        Ok(linker)
    }

    /// A fresh store for one call. Hand it to [`Runtime::run`], which uses
    /// it up.
    pub fn store<T: Send + 'static>(&self, data: T, limits: &PluginLimits) -> Store<Sandbox<T>> {
        let mut table = ResourceTable::new();
        table.set_max_capacity(MAX_HOST_RESOURCES);
        let wasi = WasiCtx::builder()
            // Not linked anyway; denied here too in case that ever changes.
            .allow_tcp(false)
            .allow_udp(false)
            .allow_ip_name_lookup(false)
            .max_random_size(MAX_RANDOM_BYTES)
            .build();
        let sandbox = Sandbox {
            data,
            limiter: Limiter::new(limits.memory_bytes),
            wasi,
            table,
        };
        let mut store = Store::new(&self.engine, sandbox);
        store.limiter(|sandbox| &mut sandbox.limiter);
        store.set_hostcall_fuel(MAX_HOSTCALL_BYTES);
        // Yield to the async runtime on every tick; interrupt once the
        // CPU budget is spent.
        let mut ticks_left = (limits.cpu.as_millis() / TICK.as_millis()).max(1) as u64;
        store.set_epoch_deadline(1);
        store.epoch_deadline_callback(move |_| {
            if ticks_left == 0 {
                return Ok(UpdateDeadline::Interrupt);
            }
            ticks_left -= 1;
            Ok(UpdateDeadline::Yield(1))
        });
        store
    }

    fn plugin_queue(&self, plugin: &str) -> Arc<Semaphore> {
        let mut queues = self.per_plugin.lock().unwrap_or_else(|p| p.into_inner());
        queues
            .entry(plugin.to_owned())
            .or_insert_with(|| Arc::new(Semaphore::new(CALLS_PER_PLUGIN)))
            .clone()
    }

    /// Runs one call for `plugin` within its limits, turning the ways a
    /// plugin can fail into [`CallError`]s. Waits for a permit first (per
    /// plugin, then overall); the deadline covers the wait and the call.
    /// `call` gets the store and returns the guest call's future; the store
    /// is dropped afterwards, whatever happened. `plugin` must be an
    /// installed plugin's id, never text from a request: each id gets a
    /// queue that lives as long as the runtime.
    pub async fn run<T, R, F>(
        &self,
        plugin: &str,
        mut store: Store<Sandbox<T>>,
        limits: &PluginLimits,
        call: F,
    ) -> Result<R, CallError>
    where
        T: Send + 'static,
        F: AsyncFnOnce(&mut Store<Sandbox<T>>) -> wasmtime::Result<R>,
    {
        // One deadline for waiting and running together.
        let deadline = tokio::time::Instant::now() + limits.deadline;
        let queue = self.plugin_queue(plugin);
        let waited = tokio::time::timeout_at(deadline, async {
            let own = queue.acquire_owned().await;
            let shared = self.calls.clone().acquire_owned().await;
            (own, shared)
        })
        .await;
        let _permits = match waited {
            Ok((Ok(own), Ok(shared))) => (own, shared),
            Ok(_) => return Err(CallError::Trap("the runtime is shutting down".to_owned())),
            Err(_) => return Err(CallError::Timeout(limits.deadline)),
        };
        let outcome = tokio::time::timeout_at(deadline, call(&mut store)).await;
        if store.data().limiter.exceeded {
            return Err(CallError::OutOfMemory(limits.memory_bytes / (1024 * 1024)));
        }
        match outcome {
            Err(_) => Err(CallError::Timeout(limits.deadline)),
            Ok(Ok(value)) => Ok(value),
            Ok(Err(err)) if err.downcast_ref::<Trap>() == Some(&Trap::Interrupt) => {
                Err(CallError::CpuLimit)
            }
            Ok(Err(err)) => {
                let full = format!("{err:?}");
                tracing::debug!(plugin, error = full, "plugin trapped");
                // The cause first (wasmtime prints it after the backtrace),
                // so truncating never loses it.
                let cause = match err.downcast_ref::<Trap>() {
                    Some(trap) => trap.to_string(),
                    None => err.to_string(),
                };
                Err(CallError::Trap(clean(&format!("{cause}\n{full}"))))
            }
        }
    }
}

/// Trap text is partly the plugin's own (function names, messages): keep
/// it short and printable, fit for an admin log.
fn clean(text: &str) -> String {
    let mut out: String = text
        .chars()
        .map(|c| if c.is_control() && c != '\n' { ' ' } else { c })
        .take(MAX_TRAP_TEXT)
        .collect();
    if text.chars().count() > MAX_TRAP_TEXT {
        out.push('…');
    }
    out
}

/// wasi:io's host data: the store's resource table.
struct Io;

impl HasData for Io {
    type Data<'a> = &'a mut ResourceTable;
}

fn link_wasi<T: Send + 'static>(l: &mut Linker<Sandbox<T>>) -> wasmtime::Result<()> {
    use wasmtime_wasi::cli::{WasiCli, WasiCliView as _};
    use wasmtime_wasi::clocks::{WasiClocks, WasiClocksView as _};
    use wasmtime_wasi::p2::bindings::{cli, clocks, io, random};
    use wasmtime_wasi::random::WasiRandom;

    io::error::add_to_linker::<_, Io>(l, |t| t.ctx().table)?;
    io::poll::add_to_linker::<_, Io>(l, |t| t.ctx().table)?;
    io::streams::add_to_linker::<_, Io>(l, |t| t.ctx().table)?;
    clocks::wall_clock::add_to_linker::<_, WasiClocks>(l, |t| t.clocks())?;
    clocks::monotonic_clock::add_to_linker::<_, WasiClocks>(l, |t| t.clocks())?;
    random::random::add_to_linker::<_, WasiRandom>(l, |t| t.ctx().ctx.random())?;
    random::insecure::add_to_linker::<_, WasiRandom>(l, |t| t.ctx().ctx.random())?;
    random::insecure_seed::add_to_linker::<_, WasiRandom>(l, |t| t.ctx().ctx.random())?;
    cli::exit::add_to_linker::<_, WasiCli>(l, |t| t.cli())?;
    cli::environment::add_to_linker::<_, WasiCli>(l, |t| t.cli())?;
    cli::stdin::add_to_linker::<_, WasiCli>(l, |t| t.cli())?;
    cli::stdout::add_to_linker::<_, WasiCli>(l, |t| t.cli())?;
    cli::stderr::add_to_linker::<_, WasiCli>(l, |t| t.cli())?;
    cli::terminal_input::add_to_linker::<_, WasiCli>(l, |t| t.cli())?;
    cli::terminal_output::add_to_linker::<_, WasiCli>(l, |t| t.cli())?;
    cli::terminal_stdin::add_to_linker::<_, WasiCli>(l, |t| t.cli())?;
    cli::terminal_stdout::add_to_linker::<_, WasiCli>(l, |t| t.cli())?;
    cli::terminal_stderr::add_to_linker::<_, WasiCli>(l, |t| t.cli())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)] // test code

    use super::*;

    const MIB: usize = 1024 * 1024;

    #[test]
    fn the_memory_cap_is_for_all_memories_together() {
        let mut limiter = Limiter::new(64 * MIB);
        // Two memories created at 30 MiB each: fine, 60 MiB in all.
        assert!(limiter.memory_growing(0, 30 * MIB, None).unwrap());
        assert!(limiter.memory_growing(0, 30 * MIB, None).unwrap());
        // A third, or growing either past the total, is refused.
        assert!(!limiter.memory_growing(0, 30 * MIB, None).unwrap());
        assert!(limiter.exceeded);
    }

    #[test]
    fn growing_counts_only_the_difference() {
        let mut limiter = Limiter::new(64 * MIB);
        assert!(limiter.memory_growing(0, 16 * MIB, None).unwrap());
        assert!(limiter.memory_growing(16 * MIB, 48 * MIB, None).unwrap());
        assert!(!limiter.memory_growing(48 * MIB, 65 * MIB, None).unwrap());
        assert_eq!(limiter.used, 48 * MIB);
    }

    #[test]
    fn trap_text_is_short_and_printable() {
        let noisy = format!("bad\u{1b}[31m name\r\n{}", "x".repeat(10_000));
        let cleaned = clean(&noisy);
        assert!(!cleaned.contains('\u{1b}') && !cleaned.contains('\r'));
        assert!(cleaned.chars().count() <= MAX_TRAP_TEXT + 1);
        assert!(cleaned.ends_with('…'));
    }
}
