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

    /// Handles a posted form (see [`Form`]). The host has already checked
    /// the values against the form's fields. Plugins without forms can
    /// leave this out.
    fn submit(submission: Submission) -> Result<SubmitResult, PageError> {
        let _ = submission;
        Err(PageError::NotFound)
    }

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

                fn submit(
                    submission: $crate::Submission,
                ) -> ::core::result::Result<$crate::SubmitResult, $crate::PageError> {
                    <$plugin as $crate::Plugin>::submit(submission)
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
    Badge, Card, Choice, Column, Field, FieldKind, Form, Link, NumberInput, Section, SelectInput,
    Stat, Tab, Table, TextInput, Tone, Value,
};
pub use bindings::{Page, PageError, Request, Submission, SubmitResult};

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

/// Who is looking at a page or posting a form (none in jobs).
pub mod identity {
    pub use crate::bindings::tether::plugin::identity::{Builtin, Character, State, Viewer};

    /// The viewer, or `None` in a job.
    pub fn viewer() -> Option<Viewer> {
        crate::bindings::tether::plugin::identity::current()
    }

    impl Viewer {
        /// Whether they hold one of this plugin's permissions (its name in
        /// `[permissions]`).
        pub fn can(&self, permission: &str) -> bool {
            self.permissions.iter().any(|p| p == permission)
        }

        /// Whether their access state is Member.
        pub fn is_member(&self) -> bool {
            self.state.builtin == Some(Builtin::Member)
        }

        /// Whether they're Guest: identity only.
        pub fn is_guest(&self) -> bool {
            self.state.builtin == Some(Builtin::Guest)
        }
    }
}

/// ESI through the host: name an endpoint (see AGENTS.md) and whose token
/// to use. The host checks approval and compliance, and never shows you a
/// token.
///
/// ```ignore
/// use tether_plugin_sdk::esi::{self, Subject};
///
/// for source in esi::data_sources() {
///     let page = esi::get("corporation-mining-extractions", Subject::DataSource(source.id), &[], None)?;
///     let extractions: serde_json::Value = serde_json::from_str(&page.body)?;
/// }
/// ```
pub mod esi {
    pub use crate::bindings::tether::plugin::esi::{Error, Named, Response, Subject};
    pub use crate::bindings::tether::plugin::identity::Character;

    /// Calls a catalogue endpoint as `subject`. `params` are the endpoint's
    /// extra ids (e.g. `observer_id`); `page` is for paged endpoints.
    pub fn get(
        endpoint: &str,
        subject: Subject,
        params: &[(String, String)],
        page: Option<u32>,
    ) -> Result<Response, Error> {
        crate::bindings::tether::plugin::esi::get(endpoint, subject, params, page)
    }

    /// Every page of a paged endpoint, concatenated (for JSON arrays).
    pub fn get_all(
        endpoint: &str,
        subject: Subject,
        params: &[(String, String)],
    ) -> Result<Vec<String>, Error> {
        let first = get(endpoint, subject, params, Some(1))?;
        let mut bodies = vec![first.body];
        for page in 2..=first.pages {
            bodies.push(get(endpoint, subject, params, Some(page))?.body);
        }
        Ok(bodies)
    }

    /// Characters you can call user-scope endpoints as: Members'
    /// characters registered with all of this plugin's user scopes.
    pub fn characters() -> Vec<Character> {
        crate::bindings::tether::plugin::esi::characters()
    }

    /// This plugin's approved data-source characters.
    pub fn data_sources() -> Vec<Character> {
        crate::bindings::tether::plugin::esi::data_sources()
    }

    /// Names for ids (public ESI; at most 1,000).
    pub fn names(ids: &[i64]) -> Result<Vec<Named>, Error> {
        crate::bindings::tether::plugin::esi::names(ids)
    }
}

/// Discord messages to channels an admin assigned this plugin.
pub mod discord {
    pub use crate::bindings::tether::plugin::discord::{Channel, Error, Mention};

    pub fn channels() -> Vec<Channel> {
        crate::bindings::tether::plugin::discord::channels()
    }

    /// Posts to an assigned channel (at most 1,500 characters), pinging
    /// nobody or the Discord role mapped to a state (by name, such as
    /// `Mention::State("Member".into())`). Not from pages.
    pub fn send(channel: &str, text: &str, mention: Mention) -> Result<(), Error> {
        crate::bindings::tether::plugin::discord::send(channel, text, &mention)
    }
}

/// Secure Groups filters you declare in `plugin.toml` (`[[filters]]`):
/// ask which settings smart groups use, work out a value per character from
/// your own data, and report it, from a job, at least daily. The host
/// combines characters into accounts: this never tells you which share
/// one.
///
/// ```ignore
/// use tether_plugin_sdk::filters;
///
/// for setting in filters::wanted() {
///     // setting.config is the admin's field values, as a JSON object.
///     let values = my_values(&setting.name, &setting.config); // Vec<(character, value)>
///     filters::report(&setting.name, &setting.config, &values)?;
/// }
/// ```
pub mod filters {
    pub use crate::bindings::tether::plugin::filters::{Error, Setting, Value};

    /// The settings of your filters that smart groups use now.
    pub fn wanted() -> Vec<Setting> {
        crate::bindings::tether::plugin::filters::wanted()
    }

    /// Replaces your values for one setting: `(character id, value)`, 1 or 0
    /// for yes-or-no filters, a count for ones that add up. Not from pages.
    pub fn report(name: &str, config: &str, values: &[(i64, i64)]) -> Result<(), Error> {
        let values: Vec<Value> = values
            .iter()
            .map(|(character_id, value)| Value {
                character_id: *character_id,
                value: *value,
            })
            .collect();
        crate::bindings::tether::plugin::filters::report(name, config, &values)
    }
}

/// Timers apps share: publish yours (`timers = "publish"`), or show
/// everyone's (`timers = "read"`).
pub mod timers {
    pub use crate::bindings::tether::plugin::timers::{Error, Shared, Timer};

    /// Replaces your published timers (at most 500). Not from pages.
    pub fn publish(timers: &[Timer]) -> Result<(), Error> {
        crate::bindings::tether::plugin::timers::publish(timers)
    }

    /// Every app's published timers that ended at most a day ago.
    pub fn published() -> Result<Vec<Shared>, Error> {
        crate::bindings::tether::plugin::timers::published()
    }
}

/// Outbound HTTPS to the hosts in `capabilities.http` that an admin
/// approved. The host sends the request, sets the User-Agent, adds a
/// secret you name (you never see its value), follows redirects only
/// within your approved hosts and records every request.
///
/// ```ignore
/// use tether_plugin_sdk::http;
///
/// let answer = http::get("https://zkillboard.com/api/killID/128570923/")?;
/// if answer.is_success() {
///     let json = answer.text().unwrap_or("");
/// }
/// // With an API key the admin entered (`[capabilities.secrets.janice_api_key]`):
/// let priced = http::Request::post("https://janice.e-351.com/api/rest/v2/appraisal", b"Tritanium 100".to_vec())
///     .header("content-type", "text/plain")
///     .secret("janice_api_key")
///     .send()?;
/// ```
pub mod http {
    pub use crate::bindings::tether::plugin::http::{Error, Method, Request, Response};

    /// Sends a request and returns the answer, whatever its status.
    pub fn send(request: &Request) -> Result<Response, Error> {
        crate::bindings::tether::plugin::http::send(request)
    }

    /// A plain GET.
    pub fn get(url: &str) -> Result<Response, Error> {
        Request::get(url).send()
    }

    /// A GET asking for JSON (`accept: application/json`).
    pub fn get_json(url: &str) -> Result<Response, Error> {
        Request::get(url)
            .header("accept", "application/json")
            .send()
    }

    /// POSTs JSON text (`content-type: application/json`).
    pub fn post_json(url: &str, json: &str) -> Result<Response, Error> {
        Request::post(url, json.as_bytes().to_vec())
            .header("content-type", "application/json")
            .header("accept", "application/json")
            .send()
    }

    impl Request {
        pub fn get(url: impl Into<String>) -> Self {
            Self {
                method: Method::Get,
                url: url.into(),
                headers: Vec::new(),
                body: None,
                secret: None,
            }
        }

        pub fn post(url: impl Into<String>, body: Vec<u8>) -> Self {
            Self {
                method: Method::Post,
                url: url.into(),
                headers: Vec::new(),
                body: Some(body),
                secret: None,
            }
        }

        /// One of `accept`, `accept-language`, `content-type`,
        /// `if-none-match` and `if-modified-since`.
        pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
            self.headers.push((name.into(), value.into()));
            self
        }

        /// Has the host add this secret (its name in
        /// `capabilities.secrets`); the URL must be on the secret's host.
        pub fn secret(mut self, name: impl Into<String>) -> Self {
            self.secret = Some(name.into());
            self
        }

        pub fn send(&self) -> Result<Response, Error> {
            send(self)
        }
    }

    impl Response {
        /// A 2xx status.
        pub fn is_success(&self) -> bool {
            (200..300).contains(&self.status)
        }

        /// The body as UTF-8 text, if it is.
        pub fn text(&self) -> Option<&str> {
            std::str::from_utf8(&self.body).ok()
        }

        /// A response header by its lowercase name.
        pub fn header(&self, name: &str) -> Option<&str> {
            self.headers
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, v)| v.as_str())
        }
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

    pub fn form(self, form: Form) -> Self {
        self.section(Section::Form(form))
    }

    pub fn tab(mut self, label: impl Into<String>, sections: Vec<Section>) -> Self {
        self.tabs.push(Tab {
            label: label.into(),
            sections,
        });
        self
    }
}

impl Submission {
    /// A posted value by field name ("" for empty optional fields).
    pub fn value(&self, name: &str) -> &str {
        self.values
            .iter()
            .find(|(n, _)| n == name)
            .map_or("", |(_, v)| v.as_str())
    }

    /// A checkbox.
    pub fn checked(&self, name: &str) -> bool {
        self.value(name) == "true"
    }
}

impl Form {
    pub fn new(id: impl Into<String>, submit_label: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            title: None,
            description: None,
            fields: Vec::new(),
            submit_label: submit_label.into(),
        }
    }

    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    pub fn description(mut self, text: impl Into<String>) -> Self {
        self.description = Some(text.into());
        self
    }

    pub fn field(mut self, field: Field) -> Self {
        self.fields.push(field);
        self
    }
}

impl Field {
    fn new(name: impl Into<String>, label: impl Into<String>, kind: FieldKind) -> Self {
        Self {
            name: name.into(),
            label: label.into(),
            help: None,
            required: false,
            kind,
        }
    }

    /// A one-line text input of at most `max_length` characters.
    pub fn text(name: impl Into<String>, label: impl Into<String>, max_length: u32) -> Self {
        Self::new(
            name,
            label,
            FieldKind::Text(TextInput {
                value: None,
                max_length,
                placeholder: None,
            }),
        )
    }

    pub fn textarea(name: impl Into<String>, label: impl Into<String>, max_length: u32) -> Self {
        Self::new(
            name,
            label,
            FieldKind::Textarea(TextInput {
                value: None,
                max_length,
                placeholder: None,
            }),
        )
    }

    pub fn number(name: impl Into<String>, label: impl Into<String>) -> Self {
        Self::new(
            name,
            label,
            FieldKind::Number(NumberInput {
                value: None,
                min: None,
                max: None,
                integer: false,
            }),
        )
    }

    /// `options` as `(value, label)` pairs.
    pub fn select(
        name: impl Into<String>,
        label: impl Into<String>,
        options: Vec<(String, String)>,
    ) -> Self {
        Self::new(
            name,
            label,
            FieldKind::Select(SelectInput {
                options: options
                    .into_iter()
                    .map(|(value, label)| Choice { value, label })
                    .collect(),
                value: None,
            }),
        )
    }

    pub fn checkbox(name: impl Into<String>, label: impl Into<String>, checked: bool) -> Self {
        Self::new(name, label, FieldKind::Checkbox(checked))
    }

    pub fn required(mut self) -> Self {
        self.required = true;
        self
    }

    pub fn help(mut self, text: impl Into<String>) -> Self {
        self.help = Some(text.into());
        self
    }

    /// The starting value: text, a number as text, or a select's value.
    pub fn value(mut self, value: impl Into<String>) -> Self {
        let value = value.into();
        match &mut self.kind {
            FieldKind::Text(input) | FieldKind::Textarea(input) => input.value = Some(value),
            FieldKind::Number(input) => input.value = value.parse().ok(),
            FieldKind::Select(input) => input.value = Some(value),
            FieldKind::Checkbox(checked) => *checked = value == "true",
        }
        self
    }

    /// Limits for a number field.
    pub fn range(mut self, min: Option<f64>, max: Option<f64>, integer: bool) -> Self {
        if let FieldKind::Number(input) = &mut self.kind {
            input.min = min;
            input.max = max;
            input.integer = integer;
        }
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
