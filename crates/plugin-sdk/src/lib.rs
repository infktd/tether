//! Build Tether plugins.
//!
//! A plugin is a Rust library compiled to a WebAssembly component
//! (`cargo build --target wasm32-wasip2 --release`). It implements
//! [`Plugin`] and hands its type to [`export!`]:
//!
//! ```ignore
//! use tether_plugin_sdk::{Page, PageError, Plugin, Request, log};
//!
//! struct Hello;
//!
//! impl Plugin for Hello {
//!     fn render(request: Request) -> Result<Page, PageError> {
//!         log::info("rendering");
//!         match request.path.as_str() {
//!             "" => Ok(Page::new("Hello").text("o7")),
//!             _ => Err(PageError::NotFound),
//!         }
//!     }
//! }
//!
//! tether_plugin_sdk::export!(Hello);
//! ```
//!
//! Pages are data: the host renders them with its own templates, so every
//! plugin looks native. See `AGENTS.md` next to this crate for the whole
//! contract, the limits a plugin runs under, and patterns.

#![deny(unsafe_code)]

/// The generated bindings (their ABI glue needs `unsafe`). Plugins use the
/// re-exports below.
#[doc(hidden)]
#[allow(unsafe_code, clippy::too_many_arguments)]
pub mod bindings {
    wit_bindgen::generate!({
        world: "plugin",
        path: "../../wit",
        pub_export_macro: true,
        export_macro_name: "export_bindings",
        default_bindings_module: "tether_plugin_sdk::bindings",
    });
}

/// What a plugin implements: its pages, and optionally its jobs.
pub trait Plugin {
    /// Renders one of the plugin's pages.
    fn render(request: Request) -> Result<Page, PageError>;

    /// Runs a scheduled or queued job (see [`jobs`]). Plugins without jobs
    /// can leave this out.
    fn run_job(job: jobs::Job) -> Result<(), jobs::JobError> {
        Err(jobs::JobError::Permanent(format!(
            "this plugin has no job called {:?}",
            job.name
        )))
    }
}

/// Exports a type implementing [`Plugin`] as the plugin:
/// `tether_plugin_sdk::export!(MyPlugin);`
#[macro_export]
macro_rules! export {
    ($plugin:ty) => {
        const _: () = {
            struct TetherPluginExport;

            impl $crate::bindings::Guest for TetherPluginExport {
                fn render(
                    request: $crate::Request,
                ) -> ::core::result::Result<$crate::Page, $crate::PageError> {
                    <$plugin as $crate::Plugin>::render(request)
                }

                fn run_job(
                    job: $crate::jobs::Job,
                ) -> ::core::result::Result<(), $crate::jobs::JobError> {
                    <$plugin as $crate::Plugin>::run_job(job)
                }
            }

    $crate::bindings::export_bindings!(TetherPluginExport with_types_in $crate::bindings);
        };
    };
}
pub use bindings::tether::plugin::page::{
    Badge, Card, Column, Link, Section, Stat, Tab, Table, Tone, Value,
};
pub use bindings::{Page, PageError, Request};

/// Logs into the plugin's log in the admin panel. Keep messages short:
/// the host keeps the first 100 per call, 1 KiB each.
pub mod log {
    use crate::bindings::tether::plugin::log::{Level, write};

    pub fn debug(message: impl AsRef<str>) {
        write(Level::Debug, message.as_ref());
    }

    pub fn info(message: impl AsRef<str>) {
        write(Level::Info, message.as_ref());
    }

    pub fn warn(message: impl AsRef<str>) {
        write(Level::Warn, message.as_ref());
    }

    pub fn error(message: impl AsRef<str>) {
        write(Level::Error, message.as_ref());
    }
}

/// Background work: the schedules declared in `plugin.toml`
/// (`[[capabilities.schedules]]`) and one-off jobs queued here. Either way
/// the host calls [`Plugin::run_job`].
///
/// ```ignore
/// use tether_plugin_sdk::jobs;
///
/// // A ping at the chunk's arrival; queuing it again moves it.
/// jobs::enqueue(
///     jobs::NewJob::new("ping")
///         .key("moon:40161234")
///         .payload(r#"{"moon": 40161234}"#)
///         .at("2026-09-30T18:05:00Z"),
/// )?;
/// jobs::cancel("moon:40161234")?;
/// ```
pub mod jobs {
    pub use crate::bindings::tether::plugin::jobs::{Error, Job, JobError, NewJob};

    /// Queues a job; with a key, replaces the queued job with that key.
    pub fn enqueue(job: NewJob) -> Result<(), Error> {
        crate::bindings::tether::plugin::jobs::enqueue(&job)
    }

    /// Removes the queued job with this key; whether there was one.
    pub fn cancel(key: &str) -> Result<bool, Error> {
        crate::bindings::tether::plugin::jobs::cancel(key)
    }

    impl NewJob {
        pub fn new(name: impl Into<String>) -> Self {
            Self {
                name: name.into(),
                key: None,
                payload: "{}".to_owned(),
                run_at: None,
            }
        }

        pub fn key(mut self, key: impl Into<String>) -> Self {
            self.key = Some(key.into());
            self
        }

        /// JSON text.
        pub fn payload(mut self, json: impl Into<String>) -> Self {
            self.payload = json.into();
            self
        }

        /// When to run it: an RFC 3339 instant, e.g. `2026-09-30T18:05:00Z`.
        pub fn at(mut self, rfc3339: impl Into<String>) -> Self {
            self.run_at = Some(rfc3339.into());
            self
        }
    }
}

/// SQL in the plugin's own database schema, for plugins approved for
/// storage (`storage = true` in `plugin.toml`). Tables come from the
/// package's `migrations/`; each call is one transaction.
///
/// ```ignore
/// use tether_plugin_sdk::storage::{self, Value};
///
/// storage::execute(
///     "INSERT INTO notes (body, at) VALUES ($1, now())",
///     &["o7".into()],
/// )?;
/// let rows = storage::query("SELECT body FROM notes ORDER BY at DESC LIMIT 10", &[])?;
/// ```
pub mod storage {
    pub use crate::bindings::tether::plugin::storage::{
        DatabaseError, Error, Rows, Statement, Value,
    };

    /// Runs a statement and returns its rows (at most 5,000 rows and 4 MiB).
    pub fn query(sql: &str, params: &[Value]) -> Result<Rows, Error> {
        crate::bindings::tether::plugin::storage::query(sql, params)
    }

    /// Runs a statement and returns how many rows it changed.
    pub fn execute(sql: &str, params: &[Value]) -> Result<u64, Error> {
        crate::bindings::tether::plugin::storage::execute(sql, params)
    }

    /// Runs statements in one transaction (at most 50): all or none.
    pub fn transaction(statements: &[Statement]) -> Result<Vec<u64>, Error> {
        crate::bindings::tether::plugin::storage::transaction(statements)
    }

    impl Statement {
        pub fn new(sql: impl Into<String>, params: Vec<Value>) -> Self {
            Self {
                sql: sql.into(),
                params,
            }
        }
    }

    impl Rows {
        /// Where a column is, by name; `None` if it isn't there (or there
        /// are no rows, which carry no column names).
        pub fn column(&self, name: &str) -> Option<usize> {
            self.columns.iter().position(|c| c == name)
        }
    }

    impl Value {
        pub fn as_text(&self) -> Option<&str> {
            match self {
                Value::Text(t) | Value::Timestamp(t) | Value::Json(t) => Some(t),
                _ => None,
            }
        }

        pub fn as_integer(&self) -> Option<i64> {
            match self {
                Value::Integer(n) => Some(*n),
                _ => None,
            }
        }

        pub fn as_float(&self) -> Option<f64> {
            match self {
                Value::Float(n) => Some(*n),
                Value::Integer(n) => Some(*n as f64),
                _ => None,
            }
        }

        pub fn as_bool(&self) -> Option<bool> {
            match self {
                Value::Boolean(b) => Some(*b),
                _ => None,
            }
        }

        pub fn is_null(&self) -> bool {
            matches!(self, Value::Null)
        }

        /// An RFC 3339 instant, e.g. `2026-09-24T18:00:00Z`.
        pub fn timestamp(rfc3339: impl Into<String>) -> Self {
            Value::Timestamp(rfc3339.into())
        }

        /// JSON text, for json and jsonb columns.
        pub fn json(text: impl Into<String>) -> Self {
            Value::Json(text.into())
        }
    }

    impl From<&str> for Value {
        fn from(text: &str) -> Self {
            Value::Text(text.to_owned())
        }
    }

    impl From<String> for Value {
        fn from(text: String) -> Self {
            Value::Text(text)
        }
    }

    impl From<i64> for Value {
        fn from(n: i64) -> Self {
            Value::Integer(n)
        }
    }

    impl From<i32> for Value {
        fn from(n: i32) -> Self {
            Value::Integer(n.into())
        }
    }

    impl From<f64> for Value {
        fn from(n: f64) -> Self {
            Value::Float(n)
        }
    }

    impl From<bool> for Value {
        fn from(b: bool) -> Self {
            Value::Boolean(b)
        }
    }

    impl From<Vec<u8>> for Value {
        fn from(bytes: Vec<u8>) -> Self {
            Value::Bytes(bytes)
        }
    }

    impl<T: Into<Value>> From<Option<T>> for Value {
        fn from(value: Option<T>) -> Self {
            value.map_or(Value::Null, Into::into)
        }
    }
}

impl Page {
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            description: None,
            sections: Vec::new(),
            tabs: Vec::new(),
        }
    }

    /// The one-line description under the title.
    pub fn description(mut self, text: impl Into<String>) -> Self {
        self.description = Some(text.into());
        self
    }

    pub fn section(mut self, section: Section) -> Self {
        self.sections.push(section);
        self
    }

    /// A row of stats (at most 8).
    pub fn stats(self, stats: Vec<Stat>) -> Self {
        self.section(Section::Stats(stats))
    }

    pub fn table(self, table: Table) -> Self {
        self.section(Section::Table(table))
    }

    pub fn card(self, card: Card) -> Self {
        self.section(Section::Card(card))
    }

    /// A paragraph of plain text.
    pub fn text(self, text: impl Into<String>) -> Self {
        self.section(Section::Text(text.into()))
    }

    pub fn tab(mut self, label: impl Into<String>, sections: Vec<Section>) -> Self {
        self.tabs.push(Tab {
            label: label.into(),
            sections,
        });
        self
    }
}

impl Stat {
    pub fn new(label: impl Into<String>, value: impl Into<Value>) -> Self {
        Self {
            label: label.into(),
            value: value.into(),
            caption: None,
        }
    }

    pub fn caption(mut self, caption: impl Into<String>) -> Self {
        self.caption = Some(caption.into());
        self
    }
}

impl Column {
    pub fn text(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            numeric: false,
        }
    }

    /// Right-aligned, for numbers, ISK and times.
    pub fn numeric(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            numeric: true,
        }
    }
}

impl Table {
    pub fn new(columns: Vec<Column>) -> Self {
        Self {
            title: None,
            columns,
            rows: Vec::new(),
            empty: None,
        }
    }

    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// One row: as many values as there are columns.
    pub fn row(mut self, values: Vec<Value>) -> Self {
        self.rows.push(values);
        self
    }

    /// What to show when there are no rows.
    pub fn empty(mut self, text: impl Into<String>) -> Self {
        self.empty = Some(text.into());
        self
    }
}

impl Card {
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            description: None,
            fields: Vec::new(),
        }
    }

    pub fn description(mut self, text: impl Into<String>) -> Self {
        self.description = Some(text.into());
        self
    }

    pub fn field(mut self, label: impl Into<String>, value: impl Into<Value>) -> Self {
        self.fields.push((label.into(), value.into()));
        self
    }
}

impl From<&str> for Value {
    fn from(text: &str) -> Self {
        Value::Text(text.to_owned())
    }
}

impl From<String> for Value {
    fn from(text: String) -> Self {
        Value::Text(text)
    }
}

impl From<i64> for Value {
    fn from(n: i64) -> Self {
        Value::Number(n)
    }
}

impl From<Badge> for Value {
    fn from(badge: Badge) -> Self {
        Value::Badge(badge)
    }
}

impl From<Link> for Value {
    fn from(link: Link) -> Self {
        Value::Link(link)
    }
}

/// An amount of ISK.
pub fn isk(amount: f64) -> Value {
    Value::Isk(amount)
}

/// An instant in EVE time (UTC), RFC 3339, e.g. `2026-09-24T18:00:00Z`.
pub fn time(rfc3339: impl Into<String>) -> Value {
    Value::Time(rfc3339.into())
}

pub fn badge(label: impl Into<String>, tone: Tone) -> Badge {
    Badge {
        label: label.into(),
        tone,
    }
}

/// A link to another page of this plugin, relative to its pages.
pub fn link(label: impl Into<String>, path: impl Into<String>) -> Link {
    Link {
        label: label.into(),
        path: path.into(),
    }
}
