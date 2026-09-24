//! The host side of the plugin API (`wit/plugin.wit`, `tether:plugin@1`).
//!
//! [`Host::load`] compiles and links a plugin once; [`Host::render`] runs
//! one page render in a fresh sandbox and checks the page before anyone
//! draws it.

use std::sync::Arc;

use wasmtime::component::{HasData, Linker};

use crate::page::{self, PageProblem};
use crate::storage::Storage;
use crate::{CallError, PluginLimits, Runtime, RuntimeError, Sandbox};
use tether::plugin::storage::{Error as StorageError, Rows, Statement, Value as StorageValue};

wasmtime::component::bindgen!({
    world: "plugin",
    path: "../../wit",
    imports: { default: async },
    exports: { default: async },
});

pub use tether::plugin::log::Level;
pub use tether::plugin::page::{Badge, Card, Column, Link, Section, Stat, Tab, Table, Tone, Value};
// `Page`, `PageError` and `Request` are generated at this module's root:
// the world `use`s them.

/// Logs a plugin may write in one call, and the characters each is cut to.
pub const MAX_LOGS: usize = 100;
pub const MAX_LOG_TEXT: usize = 1024;

/// One `log.write` from a plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogRecord {
    pub level: Level,
    pub message: String,
}

/// What host functions see during one call.
#[derive(Debug)]
pub struct CallState {
    plugin: String,
    logs: Vec<LogRecord>,
    dropped_logs: usize,
    /// `None` for plugins not approved for storage.
    storage: Option<Storage>,
}

impl CallState {
    fn new(plugin: &str, storage: Option<Storage>) -> Self {
        Self {
            plugin: plugin.to_owned(),
            logs: Vec::new(),
            dropped_logs: 0,
            storage,
        }
    }
}

impl tether::plugin::storage::Host for CallState {
    async fn query(
        &mut self,
        sql: String,
        params: Vec<StorageValue>,
    ) -> Result<Rows, StorageError> {
        match &self.storage {
            Some(storage) => storage.query(&sql, &params).await,
            None => Err(StorageError::NotApproved),
        }
    }

    async fn execute(
        &mut self,
        sql: String,
        params: Vec<StorageValue>,
    ) -> Result<u64, StorageError> {
        match &self.storage {
            Some(storage) => storage.execute(&sql, &params).await,
            None => Err(StorageError::NotApproved),
        }
    }

    async fn transaction(&mut self, statements: Vec<Statement>) -> Result<Vec<u64>, StorageError> {
        match &self.storage {
            Some(storage) => storage.transaction(&statements).await,
            None => Err(StorageError::NotApproved),
        }
    }
}

impl tether::plugin::log::Host for CallState {
    async fn write(&mut self, level: Level, message: String) {
        if self.logs.len() >= MAX_LOGS {
            self.dropped_logs += 1;
            return;
        }
        let message = printable(&message, MAX_LOG_TEXT);
        let plugin = self.plugin.as_str();
        // In its own quoted field, so a plugin's words can't pass for the
        // host's own log message.
        match level {
            Level::Debug => tracing::debug!(plugin, text = ?message, "plugin log"),
            Level::Info => tracing::info!(plugin, text = ?message, "plugin log"),
            Level::Warn => tracing::warn!(plugin, text = ?message, "plugin log"),
            Level::Error => tracing::error!(plugin, text = ?message, "plugin log"),
        }
        self.logs.push(LogRecord { level, message });
    }
}

// The world `use`s page types, which makes `page` an (empty) import.
impl tether::plugin::page::Host for CallState {}

/// Plugin-supplied text for logs: cut to `max` characters, with control
/// characters and invisible formatting (bidi overrides, zero-width
/// characters) replaced, so it can't disguise itself in a log.
pub fn printable(text: &str, max: usize) -> String {
    text.chars()
        .map(|c| {
            if c.is_control() || is_format(c) {
                ' '
            } else {
                c
            }
        })
        .take(max)
        .collect()
}

/// Invisible formatting characters: bidi overrides and isolates,
/// zero-width characters and the like.
pub(crate) fn is_format(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}'
            | '\u{061C}'
            | '\u{180E}'
            | '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{2069}'
            | '\u{FEFF}'
            // Blank-looking fillers, line and paragraph separators,
            // deprecated format controls, interlinear annotations, tag
            // characters (invisible ASCII look-alikes) and other format
            // controls in the higher planes.
            | '\u{115F}'
            | '\u{1160}'
            | '\u{3164}'
            | '\u{FFA0}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{206A}'..='\u{206F}'
            | '\u{FFF9}'..='\u{FFFB}'
            | '\u{110BD}'
            | '\u{110CD}'
            | '\u{13430}'..='\u{1343F}'
            | '\u{1BCA0}'..='\u{1BCA3}'
            | '\u{1D173}'..='\u{1D17A}'
            | '\u{E0001}'
            | '\u{E0020}'..='\u{E007F}'
    )
}

/// Host functions get the call state inside the sandbox.
struct HasState;

impl HasData for HasState {
    type Data<'a> = &'a mut CallState;
}

/// A compiled, linked plugin, ready to instantiate for each call.
#[derive(Clone)]
pub struct LoadedPlugin {
    id: String,
    pre: PluginPre<Sandbox<CallState>>,
    storage: Option<Storage>,
}

impl std::fmt::Debug for LoadedPlugin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoadedPlugin")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl LoadedPlugin {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn storage(&self) -> Option<&Storage> {
        self.storage.as_ref()
    }
}

/// A page, checked, plus what the plugin logged while making it.
#[derive(Debug)]
pub struct Rendered {
    pub page: Page,
    pub logs: Vec<LogRecord>,
}

#[derive(Debug, thiserror::Error)]
pub enum RenderError {
    /// The plugin itself says so (not found, forbidden, failed). Map each
    /// to a user-facing response; never show this error's text to users.
    #[error("the plugin answered: {0:?}")]
    Plugin(PageError),
    #[error(transparent)]
    Call(#[from] CallError),
    #[error("the plugin's page is invalid: {0}")]
    Invalid(#[from] PageProblem),
}

/// The plugin host: the runtime plus the host API's linker.
pub struct Host {
    runtime: Arc<Runtime>,
    linker: Linker<Sandbox<CallState>>,
}

impl std::fmt::Debug for Host {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Host").finish_non_exhaustive()
    }
}

impl Host {
    pub fn new(runtime: Arc<Runtime>) -> Result<Self, RuntimeError> {
        let mut linker = runtime.linker::<CallState>()?;
        Plugin::add_to_linker::<_, HasState>(&mut linker, |sandbox| &mut sandbox.data)
            .map_err(|e| RuntimeError::Link(e.to_string()))?;
        Ok(Self { runtime, linker })
    }

    pub fn runtime(&self) -> &Runtime {
        &self.runtime
    }

    /// Compiles, vets and links a plugin. A plugin importing anything the
    /// host doesn't provide (another API version, the filesystem) fails
    /// here, naming the import. `storage` is its database access, for
    /// plugins approved for it; without, storage calls answer
    /// `not-approved`.
    pub async fn load(
        &self,
        id: &str,
        component: Vec<u8>,
        storage: Option<Storage>,
    ) -> Result<LoadedPlugin, RuntimeError> {
        let component = self.runtime.compile(component).await?;
        let pre = self
            .linker
            .instantiate_pre(&component)
            .map_err(|e| RuntimeError::Rejected(e.to_string()))?;
        let pre = PluginPre::new(pre).map_err(|e| RuntimeError::Rejected(e.to_string()))?;
        Ok(LoadedPlugin {
            id: id.to_owned(),
            pre,
            storage,
        })
    }

    /// Renders one of the plugin's pages.
    pub async fn render(
        &self,
        plugin: &LoadedPlugin,
        request: Request,
        limits: &PluginLimits,
    ) -> Result<Rendered, RenderError> {
        let store = self
            .runtime
            .store(CallState::new(&plugin.id, plugin.storage.clone()), limits);
        let (answer, logs) = self
            .runtime
            .run(&plugin.id, store, limits, async |store| {
                let instance = plugin.pre.instantiate_async(&mut *store).await?;
                let answer = instance.call_render(&mut *store, &request).await?;
                let state = &mut store.data_mut().data;
                if state.dropped_logs > 0 {
                    tracing::warn!(
                        plugin = state.plugin,
                        dropped = state.dropped_logs,
                        "plugin wrote too many log lines; the rest were dropped"
                    );
                }
                Ok((answer, std::mem::take(&mut state.logs)))
            })
            .await?;
        let page = answer.map_err(|err| {
            RenderError::Plugin(match err {
                // The plugin's own words, for admins: bounded and clean.
                PageError::Failed(text) => PageError::Failed(printable(&text, MAX_LOG_TEXT)),
                other => other,
            })
        })?;
        page::check(&page)?;
        Ok(Rendered { page, logs })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invisible_characters_are_replaced() {
        for c in [
            '\u{202E}',
            '\u{200B}',
            '\u{2066}',
            '\u{FEFF}',
            '\u{3164}',
            '\u{206F}',
            '\u{FFF9}',
            '\u{E0041}',
            '\u{1D173}',
            '\u{2028}',
        ] {
            assert_eq!(printable(&format!("a{c}b"), 10), "a b", "{:?}", c);
        }
        assert_eq!(printable("o7 Ünïcode fine", 100), "o7 Ünïcode fine");
    }
}
