//! The host side of the plugin API (`wit/plugin.wit`, `tether:plugin@1`).
//!
//! [`Host::load`] compiles and links a plugin once; [`Host::render`] runs
//! one page render in a fresh sandbox and checks the page before anyone
//! draws it.

use std::sync::Arc;

use wasmtime::component::{HasData, Linker};

use crate::jobs;
use crate::page::{self, PageProblem};
use crate::services;
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
pub use tether::plugin::page::{
    Badge, Card, Choice, Column, Field, FieldKind, Form, Link, NumberInput, Section, SelectInput,
    Stat, Tab, Table, TextInput, Tone, Value,
};
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
    /// `None` where no queue is wired up (tests of pages alone).
    jobs: Option<jobs::Queue>,
    job_calls: usize,
    /// Page renders may not queue or cancel jobs, or send to Discord.
    jobs_refused: bool,
    services: Option<services::Shared>,
    viewer: Option<services::Viewer>,
    esi_calls: usize,
    discord_sends: usize,
}

impl CallState {
    fn new(plugin: &str, storage: Option<Storage>, jobs: Option<jobs::Queue>) -> Self {
        Self {
            plugin: plugin.to_owned(),
            logs: Vec::new(),
            dropped_logs: 0,
            storage,
            jobs,
            job_calls: 0,
            jobs_refused: false,
            services: None,
            viewer: None,
            esi_calls: 0,
            discord_sends: 0,
        }
    }

    fn esi(&mut self) -> Result<services::Shared, services::EsiError> {
        self.esi_calls += 1;
        let max = if self.jobs_refused {
            services::MAX_ESI_CALLS_PAGE
        } else {
            services::MAX_ESI_CALLS
        };
        if self.esi_calls > max {
            return Err(services::EsiError::Invalid(format!(
                "more than {max} ESI calls in one call"
            )));
        }
        self.services.clone().ok_or(services::EsiError::Unavailable)
    }
}

impl tether::plugin::identity::Host for CallState {
    async fn current(&mut self) -> Option<services::Viewer> {
        self.viewer.clone()
    }
}

impl tether::plugin::esi::Host for CallState {
    async fn get(
        &mut self,
        endpoint: String,
        subject: services::Subject,
        params: Vec<(String, String)>,
        page: Option<u32>,
    ) -> Result<services::EsiResponse, services::EsiError> {
        let services = self.esi()?;
        services
            .esi_get(self.plugin.clone(), endpoint, subject, params, page)
            .await
    }

    async fn consented(&mut self) -> Vec<services::Consent> {
        match self.esi() {
            Ok(services) => services.esi_consented(self.plugin.clone()).await,
            Err(_) => Vec::new(),
        }
    }

    async fn data_sources(&mut self) -> Vec<services::Character> {
        match self.esi() {
            Ok(services) => services.esi_data_sources(self.plugin.clone()).await,
            Err(_) => Vec::new(),
        }
    }

    async fn names(&mut self, ids: Vec<i64>) -> Result<Vec<services::Named>, services::EsiError> {
        if ids.len() > 1000 {
            return Err(services::EsiError::Invalid("at most 1,000 ids".to_owned()));
        }
        let services = self.esi()?;
        services.esi_names(self.plugin.clone(), ids).await
    }
}

impl tether::plugin::discord::Host for CallState {
    async fn channels(&mut self) -> Vec<services::Channel> {
        match &self.services {
            Some(services) => services.discord_channels(self.plugin.clone()).await,
            None => Vec::new(),
        }
    }

    async fn send(
        &mut self,
        channel: String,
        text: String,
        mention: services::Mention,
    ) -> Result<(), services::DiscordError> {
        if self.jobs_refused {
            return Err(services::DiscordError::NotAllowed(
                "pages can't send messages: do that in submit or a job".to_owned(),
            ));
        }
        self.discord_sends += 1;
        if self.discord_sends > services::MAX_DISCORD_SENDS {
            return Err(services::DiscordError::RateLimited);
        }
        let services = self
            .services
            .clone()
            .ok_or(services::DiscordError::Unavailable)?;
        services
            .discord_send(self.plugin.clone(), channel, text, mention)
            .await
    }
}

impl CallState {
    /// The queue, if this call may use it once more.
    fn queue(&mut self) -> Result<jobs::Queue, jobs::Error> {
        if self.jobs_refused {
            return Err(jobs::Error::Invalid(
                "pages can't queue or cancel jobs: do that in submit or a job".to_owned(),
            ));
        }
        self.job_calls += 1;
        if self.job_calls > jobs::MAX_CALLS {
            return Err(jobs::Error::Invalid(format!(
                "more than {} enqueue or cancel calls in one call",
                jobs::MAX_CALLS
            )));
        }
        self.jobs.clone().ok_or(jobs::Error::Unavailable)
    }
}

impl tether::plugin::jobs::Host for CallState {
    async fn enqueue(&mut self, job: jobs::NewJob) -> Result<(), jobs::Error> {
        let queue = self.queue()?;
        let job = jobs::check(job, chrono::Utc::now())?;
        queue.enqueue(self.plugin.clone(), job).await
    }

    async fn cancel(&mut self, key: String) -> Result<bool, jobs::Error> {
        let queue = self.queue()?;
        jobs::check_key(&key)?;
        queue.cancel(self.plugin.clone(), key).await
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

/// What a submission led to, plus what the plugin logged.
#[derive(Debug)]
pub struct Submitted {
    pub result: SubmitResult,
    pub logs: Vec<LogRecord>,
}

/// How a job went, plus what the plugin logged while running it.
#[derive(Debug)]
pub struct JobRun {
    pub result: Result<(), jobs::JobError>,
    pub logs: Vec<LogRecord>,
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
    jobs: Option<jobs::Queue>,
    services: Option<services::Shared>,
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
        Ok(Self {
            runtime,
            linker,
            jobs: None,
            services: None,
        })
    }

    /// Where plugins' `enqueue` and `cancel` go.
    pub fn with_jobs(mut self, queue: jobs::Queue) -> Self {
        self.jobs = Some(queue);
        self
    }

    /// Tether's side of ESI, identity and Discord.
    pub fn with_services(mut self, services: services::Shared) -> Self {
        self.services = Some(services);
        self
    }

    pub fn runtime(&self) -> &Runtime {
        &self.runtime
    }

    fn call_state(&self, plugin: &LoadedPlugin) -> CallState {
        let mut state = CallState::new(&plugin.id, plugin.storage.clone(), self.jobs.clone());
        state.services = self.services.clone();
        state
    }

    /// For page renders, which run on GETs anyone can be linked into:
    /// storage is read-only and jobs can't be queued or cancelled.
    fn render_state(&self, plugin: &LoadedPlugin) -> CallState {
        let mut state = CallState::new(
            &plugin.id,
            plugin.storage.as_ref().map(Storage::read_only),
            None,
        );
        state.jobs_refused = true;
        state.services = self.services.clone();
        state
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
        self.render_as(plugin, request, None, limits).await
    }

    /// Renders a page for `viewer` (what `identity.current` answers).
    pub async fn render_as(
        &self,
        plugin: &LoadedPlugin,
        request: Request,
        viewer: Option<services::Viewer>,
        limits: &PluginLimits,
    ) -> Result<Rendered, RenderError> {
        let mut state = self.render_state(plugin);
        state.viewer = viewer;
        let store = self.runtime.store(state, limits);
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

    /// Hands a checked form submission to the plugin. A page it answers
    /// with is checked like any other.
    pub async fn submit(
        &self,
        plugin: &LoadedPlugin,
        submission: Submission,
        limits: &PluginLimits,
    ) -> Result<Submitted, RenderError> {
        self.submit_as(plugin, submission, None, limits).await
    }

    /// Handles a form posted by `viewer`.
    pub async fn submit_as(
        &self,
        plugin: &LoadedPlugin,
        submission: Submission,
        viewer: Option<services::Viewer>,
        limits: &PluginLimits,
    ) -> Result<Submitted, RenderError> {
        let mut state = self.call_state(plugin);
        state.viewer = viewer;
        let store = self.runtime.store(state, limits);
        let (answer, logs) = self
            .runtime
            .run(&plugin.id, store, limits, async |store| {
                let instance = plugin.pre.instantiate_async(&mut *store).await?;
                let answer = instance.call_submit(&mut *store, &submission).await?;
                let state = &mut store.data_mut().data;
                Ok((answer, std::mem::take(&mut state.logs)))
            })
            .await?;
        let result = answer.map_err(|err| {
            RenderError::Plugin(match err {
                PageError::Failed(text) => PageError::Failed(printable(&text, MAX_LOG_TEXT)),
                other => other,
            })
        })?;
        match &result {
            SubmitResult::Page(page) => page::check(page)?,
            SubmitResult::Redirect(path) => page::check_link_path(path)?,
        }
        Ok(Submitted { result, logs })
    }

    /// Runs one of the plugin's jobs.
    pub async fn run_job(
        &self,
        plugin: &LoadedPlugin,
        job: jobs::Job,
        limits: &PluginLimits,
    ) -> Result<JobRun, CallError> {
        let store = self.runtime.store(self.call_state(plugin), limits);
        let (result, logs) = self
            .runtime
            .run_as(
                crate::CallKind::Job,
                &plugin.id,
                store,
                limits,
                async |store| {
                    let instance = plugin.pre.instantiate_async(&mut *store).await?;
                    let result = instance.call_run_job(&mut *store, &job).await?;
                    let state = &mut store.data_mut().data;
                    if state.dropped_logs > 0 {
                        tracing::warn!(
                            plugin = state.plugin,
                            dropped = state.dropped_logs,
                            "plugin wrote too many log lines; the rest were dropped"
                        );
                    }
                    Ok((result, std::mem::take(&mut state.logs)))
                },
            )
            .await?;
        let result = result.map_err(|err| match err {
            // The plugin's own words, for admins: bounded and clean.
            jobs::JobError::Retry(text) => jobs::JobError::Retry(printable(&text, MAX_LOG_TEXT)),
            jobs::JobError::Permanent(text) => {
                jobs::JobError::Permanent(printable(&text, MAX_LOG_TEXT))
            }
        });
        Ok(JobRun { result, logs })
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
