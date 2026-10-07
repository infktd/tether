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
    Action, Badge, Card, CardGrid, Choice, CodeBlock, Column, Composition, Defenses, Entity,
    EntityKind, Field, FieldKind, Form, Lane, LaneItem, Levels, Link, NumberInput, Profile,
    ProfileCard, Progress, RecordPanel, Section, SelectInput, SettingsForm, SettingsGroup, Share,
    Stat, Tab, Table, TextInput, Timeline, Tone, Toolbar, ToolbarFilter, Value, Window,
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
    pub use crate::bindings::tether::plugin::identity::{
        Builtin, Character, Group, Member, MemberCharacter, Owner, State, Viewer,
    };

    /// The viewer, or `None` in a job.
    pub fn viewer() -> Option<Viewer> {
        crate::bindings::tether::plugin::identity::current()
    }

    /// The character the viewer acts as: the one they chose in their
    /// account menu (Change character), else their main. Use it where you
    /// act for them (registering, their fits, their requests); scope what
    /// they may see by `viewer().main` and permissions, never by this.
    /// `None` in a job.
    pub fn acting() -> Option<Character> {
        crate::bindings::tether::plugin::identity::acting()
    }

    /// The viewer's own groups (none in a job): to show something only to
    /// members of the groups it's limited to.
    pub fn groups() -> Vec<Group> {
        crate::bindings::tether::plugin::identity::groups()
    }

    /// Groups to offer the viewer to pick from (none in a job): every
    /// group that is neither Hidden nor Internal, and the viewer's own; all
    /// but Internal ones for `group_management` holders, every group for
    /// `admin.groups` holders. Both need `groups = true` in
    /// `[capabilities]`; without it they're empty.
    pub fn all_groups() -> Vec<Group> {
        crate::bindings::tether::plugin::identity::all_groups()
    }

    /// Whether the viewer is a superuser (AA's `is_superuser`): for what
    /// AA shows superusers only. False in a job.
    pub fn superuser() -> bool {
        crate::bindings::tether::plugin::identity::superuser()
    }

    /// First-party Member Audit only: who owns each of
    /// [`esi::characters`](crate::esi::characters) (their main and state).
    /// Every other app gets `None`: apps never learn which characters
    /// share an account.
    pub fn owners() -> Option<Vec<Owner>> {
        crate::bindings::tether::plugin::identity::owners()
    }

    /// First-party Member Audit only: every account holding one of its
    /// permissions (its main and state) with all its characters,
    /// registered or not. Every other app gets `None`.
    pub fn members() -> Option<Vec<Member>> {
        crate::bindings::tether::plugin::identity::members()
    }

    /// First-party HR Applications only: the characters now on the
    /// account behind one of this app's
    /// [`notify::submitter_reference`](crate::notify::submitter_reference)s,
    /// while the reference reaches them. Every other app gets `None`.
    pub fn submitter_characters(reference: &str) -> Option<Vec<Character>> {
        crate::bindings::tether::plugin::identity::submitter_characters(reference)
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
    /// What an ESI call answers when it doesn't go through. Among them
    /// `MissingScope(scope)`: the character is one of yours, but its login
    /// lacks this scope, which your app asked for after its pilot
    /// registered it. Skip what needs it and carry on with the rest:
    /// Tether asks the pilot to register again.
    pub use crate::bindings::tether::plugin::esi::FetchError as Error;
    pub use crate::bindings::tether::plugin::esi::{Named, Response, Subject};
    pub use crate::bindings::tether::plugin::identity::Character;

    use crate::bindings::tether::plugin::esi::Error as HostError;

    /// `post` and `names` answer the older error, which has no
    /// `MissingScope`.
    fn from_host(err: HostError) -> Error {
        match err {
            HostError::NotAllowed(why) => Error::NotAllowed(why),
            HostError::NotRegistered => Error::NotRegistered,
            HostError::NotADataSource => Error::NotADataSource,
            HostError::Token => Error::Token,
            HostError::Status(code) => Error::Status(code),
            HostError::Invalid(why) => Error::Invalid(why),
            HostError::TooLarge => Error::TooLarge,
            HostError::Unavailable => Error::Unavailable,
        }
    }

    /// What went wrong, in words for a page, a notice or a log line:
    /// never the error's Rust form (`Status(403)`).
    pub fn describe(err: &Error) -> String {
        match err {
            Error::NotAllowed(why) => format!("not allowed: {why}"),
            Error::NotRegistered => "the character isn't registered for this app".to_owned(),
            Error::MissingScope(_) => "the character's login lacks a scope this app asked for \
                                       since it registered: it needs registering again"
                .to_owned(),
            Error::NotADataSource => {
                "the character isn't one of this app's data sources".to_owned()
            }
            Error::Token => {
                "the character's EVE login stopped working: it needs logging in again".to_owned()
            }
            Error::Status(403) => {
                "ESI refused (403): the character lacks an in-game role or a scope".to_owned()
            }
            Error::Status(404) => "ESI found nothing there (404)".to_owned(),
            Error::Status(code) if *code >= 500 => {
                format!("ESI had trouble ({code}): it's tried again later")
            }
            Error::Status(code) => format!("ESI answered {code}"),
            Error::Invalid(why) => why.clone(),
            Error::TooLarge => "ESI's answer was larger than a call may return".to_owned(),
            Error::Unavailable => "ESI or Tether couldn't be reached".to_owned(),
        }
    }

    /// Calls a catalogue endpoint as `subject`. `params` are the endpoint's
    /// extra ids (e.g. `observer_id`); `page` is for paged endpoints. A
    /// character whose login lacks the endpoint's scope answers
    /// `MissingScope`.
    pub fn get(
        endpoint: &str,
        subject: Subject,
        params: &[(String, String)],
        page: Option<u32>,
    ) -> Result<Response, Error> {
        crate::bindings::tether::plugin::esi::fetch(endpoint, subject, params, page)
    }

    /// Changes something in EVE: one of Tether's write endpoints
    /// (`character-fitting-save`, with ESI's fitting JSON as `body`). Only
    /// in `submit`, for one of the viewer's own characters that is one of
    /// this plugin's characters. Sent once, never retried. A character
    /// whose login lacks the scope answers `NotRegistered`.
    pub fn post(endpoint: &str, subject: Subject, body: &str) -> Result<Response, Error> {
        crate::bindings::tether::plugin::esi::post(endpoint, subject, body).map_err(from_host)
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

    /// Characters you can call user-scope endpoints as: this plugin's
    /// characters: registered for it by pilots holding one of its
    /// permissions (any state), with working logins. One registered before
    /// you asked for another scope is still yours: a call needing that
    /// scope answers `MissingScope`, and the rest of it reads as before.
    pub fn characters() -> Vec<Character> {
        crate::bindings::tether::plugin::esi::characters()
    }

    /// This plugin's data-source characters in use.
    pub fn data_sources() -> Vec<Character> {
        crate::bindings::tether::plugin::esi::data_sources()
    }

    /// Names for ids (public ESI; at most 1,000).
    pub fn names(ids: &[i64]) -> Result<Vec<Named>, Error> {
        crate::bindings::tether::plugin::esi::names(ids).map_err(from_host)
    }
}

/// Discord messages to channels an admin assigned this plugin.
pub mod discord {
    pub use crate::bindings::tether::plugin::discord::{
        Channel, Embed, EmbedAuthor, EmbedField, Error, Image, Mention, Message, Ping,
    };

    pub fn channels() -> Vec<Channel> {
        crate::bindings::tether::plugin::discord::channels()
    }

    /// Posts to an assigned channel (at most 1,500 characters), pinging
    /// nobody or the Discord role mapped to a state (by name, such as
    /// `Mention::State("Member".into())`). Not from pages.
    pub fn send(channel: &str, text: &str, mention: Mention) -> Result<(), Error> {
        crate::bindings::tether::plugin::discord::send(channel, text, &mention)
    }

    /// Posts a card (Discord's embed) to an assigned channel; the mention,
    /// if any, is the text above it. Limits as Discord's: a title of 256
    /// characters, a description of 2,000, 10 fields. Images are CCP's
    /// (`Image::Corporation(id)`, `Image::TypeRender(type_id)`, ...).
    pub fn send_embed(channel: &str, embed: &Embed, mention: Mention) -> Result<(), Error> {
        crate::bindings::tether::plugin::discord::send_embed(channel, embed, &mention)
    }

    /// As [`send_embed`], the card's title a link to `page`, one of this
    /// app's own pages by its path (`"fleet/12"`): Tether makes the address
    /// on this instance.
    pub fn send_linked_embed(
        channel: &str,
        embed: &Embed,
        page: &str,
        mention: Mention,
    ) -> Result<(), Error> {
        crate::bindings::tether::plugin::discord::send_linked_embed(channel, embed, page, &mention)
    }

    /// Posts a [`Message`] (text, a card, or both) to an assigned channel,
    /// pinging up to 10 roles at once: states' (`Ping::State("Member".into())`)
    /// and, with `mention_groups` approved, groups' (`Ping::Group(name)`).
    /// A ping without a mapped role is left out and logged; the message
    /// still goes. Not from pages.
    pub fn send_message(channel: &str, message: &Message) -> Result<(), Error> {
        crate::bindings::tether::plugin::discord::send_message(channel, message)
    }

    impl Message {
        /// A message of `text` (may be empty with a card), pinging nobody.
        pub fn new(text: impl Into<String>) -> Self {
            Self {
                text: text.into(),
                embed: None,
                page: None,
                pings: Vec::new(),
            }
        }

        /// A card under the text.
        pub fn embed(mut self, embed: Embed) -> Self {
            self.embed = Some(embed);
            self
        }

        /// The card's title a link to one of this app's own pages.
        pub fn page(mut self, page: impl Into<String>) -> Self {
            self.page = Some(page.into());
            self
        }

        /// Pings `ping`'s role too.
        pub fn ping(mut self, ping: Ping) -> Self {
            self.pings.push(ping);
            self
        }
    }

    impl Embed {
        /// A card with just a title; fill in the rest with the builders.
        pub fn new(title: impl Into<String>) -> Self {
            Self {
                title: title.into(),
                description: None,
                color: None,
                author: None,
                thumbnail: None,
                fields: Vec::new(),
                footer: None,
                timestamp: None,
            }
        }

        pub fn description(mut self, text: impl Into<String>) -> Self {
            self.description = Some(text.into());
            self
        }

        /// The bar down its side, `0xRRGGBB`.
        pub fn color(mut self, rgb: u32) -> Self {
            self.color = Some(rgb);
            self
        }

        /// A line above the title, with an image beside it.
        pub fn author(mut self, name: impl Into<String>, icon: Option<Image>) -> Self {
            self.author = Some(EmbedAuthor {
                name: name.into(),
                icon,
            });
            self
        }

        pub fn thumbnail(mut self, image: Image) -> Self {
            self.thumbnail = Some(image);
            self
        }

        /// A field three to a row.
        pub fn field(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
            self.fields.push(EmbedField {
                name: name.into(),
                value: value.into(),
                inline: true,
            });
            self
        }

        /// A field on its own line.
        pub fn wide_field(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
            self.fields.push(EmbedField {
                name: name.into(),
                value: value.into(),
                inline: false,
            });
            self
        }

        pub fn footer(mut self, text: impl Into<String>) -> Self {
            self.footer = Some(text.into());
            self
        }

        /// RFC 3339; shown beside the footer in each reader's own time.
        pub fn timestamp(mut self, rfc3339: impl Into<String>) -> Self {
            self.timestamp = Some(rfc3339.into());
            self
        }
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

/// Doctrines apps share (allianceauth-fittings' in aa-fleetpings and
/// aa-fat): publish yours with `doctrines = "publish"`, offer others' with
/// `doctrines = "read"`. Each is offered only to whoever may see it.
pub mod doctrines {
    pub use crate::bindings::tether::plugin::doctrines::{Doctrine, Error, Shared};

    /// Replaces your published doctrines (at most 500), in this order.
    /// Holders of `see_all`, one of your permissions, see every one. Not
    /// from pages.
    pub fn publish(doctrines: &[Doctrine], see_all: Option<&str>) -> Result<(), Error> {
        crate::bindings::tether::plugin::doctrines::publish(doctrines, see_all)
    }

    /// Published doctrines the viewer may see (none in a job), each with
    /// its page's address.
    pub fn published() -> Result<Vec<Shared>, Error> {
        crate::bindings::tether::plugin::doctrines::published()
    }
}

/// Files you offer for download (aa-memberaudit's data exports), with
/// `downloads = true`: hand over rows in a job or submit, and Tether writes
/// the CSV and serves it at your `downloads/<name>` (link to it) to holders
/// of the permission you name.
pub mod downloads {
    pub use crate::bindings::tether::plugin::downloads::{Error, File};

    /// Rows one [`append`] may add.
    pub const MAX_ROWS_PER_APPEND: usize = 5_000;

    /// Starts building `name` (lowercase letters, digits, `-`): its title,
    /// your permission that may download it, and its column names. Returns
    /// the build, for [`append`] and [`finish`]; carry it between jobs.
    /// [`Error::Superseded`] from either means a newer build began: stop.
    pub fn begin(
        name: &str,
        title: &str,
        permission: &str,
        header: &[String],
    ) -> Result<u32, Error> {
        crate::bindings::tether::plugin::downloads::begin(name, title, permission, header)
    }

    /// Adds rows (at most [`MAX_ROWS_PER_APPEND`], each as many cells as
    /// the header).
    pub fn append(name: &str, build: u32, rows: &[Vec<String>]) -> Result<(), Error> {
        crate::bindings::tether::plugin::downloads::append(name, build, rows)
    }

    /// Makes the build the file people download.
    pub fn finish(name: &str, build: u32) -> Result<(), Error> {
        crate::bindings::tether::plugin::downloads::finish(name, build)
    }

    /// Your finished downloads.
    pub fn files() -> Vec<File> {
        crate::bindings::tether::plugin::downloads::files()
    }
}

/// Notices in Tether's notifications (the bell), as AA apps `notify`,
/// with `notify = true`: plain text from a submit or a job, your app's
/// name put before the title, only to accounts holding one of your
/// permissions or that submitted one of your forms, a limited number an
/// hour.
pub mod notify {
    pub use crate::bindings::tether::plugin::notify::{Error, Level};

    /// The longest title.
    pub const MAX_TITLE: usize = 100;
    /// The longest message.
    pub const MAX_MESSAGE: usize = 1_000;

    /// To one account (`identity::current().account_id`, kept from when
    /// they used your app); false if it no longer holds any of your
    /// permissions.
    pub fn account(
        account_id: i64,
        title: &str,
        message: &str,
        level: Level,
    ) -> Result<bool, Error> {
        crate::bindings::tether::plugin::notify::account(account_id, title, message, level)
    }

    /// To every holder of `permission`, one of yours, but `except` (the
    /// one who acted); how many it reached.
    pub fn holders(
        permission: &str,
        title: &str,
        message: &str,
        level: Level,
        except: Option<i64>,
    ) -> Result<u32, Error> {
        crate::bindings::tether::plugin::notify::holders(permission, title, message, level, except)
    }

    /// Only in `submit`, for the pilot posting the form: a reference to
    /// their account to keep with what they submitted (an application, a
    /// request), so [`submitter`] reaches them later even if they hold
    /// none of your permissions. Always the same for the same account; it
    /// reaches them for a year after the last post it was asked for in.
    pub fn submitter_reference() -> Result<String, Error> {
        crate::bindings::tether::plugin::notify::submitter_reference()
    }

    /// To the account behind one of your [`submitter_reference`]s; false
    /// if it's gone, past its year, or past the hourly limit for them.
    pub fn submitter(
        reference: &str,
        title: &str,
        message: &str,
        level: Level,
    ) -> Result<bool, Error> {
        crate::bindings::tether::plugin::notify::submitter(reference, title, message, level)
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
            links: Vec::new(),
            refresh_seconds: None,
            toolbar: None,
            panel: None,
        }
    }

    /// The page's own search and filters above its lists (see
    /// [`Toolbar::new`]). Without one, Tether still draws a search box on a
    /// page with a table, and finds rows among those shown.
    pub fn toolbar(mut self, toolbar: Toolbar) -> Self {
        self.toolbar = Some(toolbar);
        self
    }

    /// The selected row's details beside the list (see [`RecordPanel::new`]):
    /// draw it while the request's query selects a row.
    pub fn panel(mut self, panel: RecordPanel) -> Self {
        self.panel = Some(panel);
        self
    }

    /// A link to another of your pages beside the title (at most 8), for
    /// sub-pages: "Skill Sets · Character Finder · Reports". The page being
    /// shown is marked.
    pub fn link(mut self, label: impl Into<String>, path: impl Into<String>) -> Self {
        self.links.push(link(label, path));
        self
    }

    /// A primary button beside the title that opens another of your pages,
    /// e.g. `.button("Create timer", "timers/new")`. Counts toward the 8
    /// links.
    pub fn button(mut self, label: impl Into<String>, path: impl Into<String>) -> Self {
        self.links.push(link(label, path).primary());
        self
    }

    /// Reload the page's content every `seconds` (5 to 300) while this
    /// render says so, e.g. while a first sync fills it in. Leave it out
    /// once there's nothing more to wait for. Ignored on pages with a form.
    pub fn refresh(mut self, seconds: u32) -> Self {
        self.refresh_seconds = Some(seconds);
        self
    }

    /// The top of a page about one character or corporation.
    pub fn profile(self, profile: Profile) -> Self {
        self.section(Section::Profile(profile))
    }

    /// A grid of compact profile cards, e.g. My Characters. See
    /// [`CardGrid::new`].
    pub fn cards(self, grid: CardGrid) -> Self {
        self.section(Section::Cards(grid))
    }

    /// Text to copy (a fitting, a list): monospaced, kept exactly, with a
    /// Copy button. See [`CodeBlock::new`].
    pub fn code(self, code: CodeBlock) -> Self {
        self.section(Section::Code(code))
    }

    /// Events on lanes across a span of time. See [`Timeline::new`].
    pub fn timeline(self, timeline: Timeline) -> Self {
        self.section(Section::Timeline(timeline))
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

    /// A settings page's form: its fields in groups, saved at once from
    /// Tether's save bar (see [`SettingsForm::new`]).
    pub fn settings(self, settings: SettingsForm) -> Self {
        self.section(Section::Settings(settings))
    }

    pub fn tab(mut self, label: impl Into<String>, sections: Vec<Section>) -> Self {
        self.tabs.push(Tab {
            label: label.into(),
            sections,
        });
        self
    }
}

impl SettingsForm {
    /// A settings form posting as `id` (as a form's): add its groups with
    /// [`SettingsForm::group`]. Tether draws them with a save bar that
    /// appears once something changed, marks each changed field, puts
    /// them back on Discard and asks before leaving with changes unsaved;
    /// `submit` gets every field's value at once.
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            groups: Vec::new(),
        }
    }

    /// A group (at most 16; 120 fields in all).
    pub fn group(mut self, group: SettingsGroup) -> Self {
        self.groups.push(group);
        self
    }
}

impl SettingsGroup {
    /// A group of settings under its heading ("Discord", "Fuel alerts").
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            description: None,
            fields: Vec::new(),
        }
    }

    /// One line about it.
    pub fn description(mut self, text: impl Into<String>) -> Self {
        self.description = Some(text.into());
        self
    }

    /// A field (at most 30), as a form's.
    pub fn field(mut self, field: Field) -> Self {
        self.fields.push(field);
        self
    }
}

impl Toolbar {
    /// No search of your own (Tether finds rows among those shown) and no
    /// filters yet.
    pub fn new() -> Self {
        Self {
            search: None,
            filters: Vec::new(),
        }
    }

    /// Your page searches its own data for the box's words: `q` in the
    /// request's query ([`Request::search`]). `placeholder` says what it
    /// finds: "Search moons, systems, refineries".
    pub fn search(mut self, placeholder: impl Into<String>) -> Self {
        self.search = Some(placeholder.into());
        self
    }

    /// A filter (at most 8): its query parameter (lowercase letters,
    /// digits and `_`, not `q`), its name, and its values with their words.
    /// Read the chosen one with [`Request::param`].
    pub fn filter(
        mut self,
        param: impl Into<String>,
        label: impl Into<String>,
        choices: Vec<(String, String)>,
    ) -> Self {
        self.filters.push(ToolbarFilter {
            param: param.into(),
            label: label.into(),
            choices: choices
                .into_iter()
                .map(|(value, label)| Choice { value, label })
                .collect(),
            multiple: false,
        });
        self
    }

    /// A filter taking several of its values at once, showing what has any
    /// of them (tags): read them with [`Request::params`]. Its values have
    /// no commas.
    pub fn filter_any(
        mut self,
        param: impl Into<String>,
        label: impl Into<String>,
        choices: Vec<(String, String)>,
    ) -> Self {
        self = self.filter(param, label, choices);
        if let Some(last) = self.filters.last_mut() {
            last.multiple = true;
        }
        self
    }
}

impl Default for Toolbar {
    fn default() -> Self {
        Self::new()
    }
}

impl RecordPanel {
    /// The details of the row `param` selects (its query parameter, as a
    /// filter's): `kind` is the overline ("Moon · R32"), `title` its name.
    /// Link the row's name to the same page with it:
    /// `link(name, format!("moons?moon={id}"))`.
    pub fn new(
        param: impl Into<String>,
        kind: impl Into<String>,
        title: impl Into<String>,
    ) -> Self {
        Self {
            param: param.into(),
            kind: kind.into(),
            title: title.into(),
            context: None,
            figure: None,
            facts: Vec::new(),
            open: None,
            action: None,
        }
    }

    /// A line under the title.
    pub fn context(mut self, text: impl Into<String>) -> Self {
        self.context = Some(text.into());
        self
    }

    /// Its picture, drawn large: a ring of what it's made of (1 to 8
    /// parts, see [`part`]) with a few words in its middle (a moon's
    /// value).
    pub fn figure(mut self, parts: Vec<Share>, center: impl Into<String>) -> Self {
        self.figure = Some(Composition {
            parts,
            center: Some(center.into()),
        });
        self
    }

    /// A fact (at most 20).
    pub fn fact(mut self, label: impl Into<String>, value: impl Into<Value>) -> Self {
        self.facts.push((label.into(), value.into()));
        self
    }

    /// Its own page, as the panel's primary button ("Open moon").
    pub fn open(mut self, label: impl Into<String>, path: impl Into<String>) -> Self {
        self.open = Some(link(label, path));
        self
    }

    /// One more button that posts (see [`action`]).
    pub fn action(mut self, action: Action) -> Self {
        self.action = Some(action);
        self
    }
}

impl Request {
    /// A query parameter's value: a toolbar filter's, a record panel's
    /// (`""` when it isn't there).
    pub fn param(&self, name: &str) -> &str {
        self.query
            .iter()
            .find(|(n, _)| n == name)
            .map_or("", |(_, v)| v.as_str())
    }

    /// The toolbar's search, trimmed (`""` when there's none).
    pub fn search(&self) -> &str {
        self.param("q").trim()
    }

    /// A filter taking several values ([`Toolbar::filter_any`]): those
    /// chosen, none when it isn't there.
    pub fn params(&self, name: &str) -> Vec<&str> {
        self.param(name)
            .split(',')
            .filter(|v| !v.is_empty())
            .collect()
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

impl From<Entity> for Value {
    fn from(entity: Entity) -> Self {
        Value::Entity(entity)
    }
}

impl From<Progress> for Value {
    fn from(progress: Progress) -> Self {
        Value::Progress(progress)
    }
}

impl Profile {
    /// A profile of `subject`: usually a [`character`], or a
    /// [`corporation`] (its logo is shown).
    pub fn new(subject: Entity) -> Self {
        Self {
            subject,
            subtitle: None,
            corporation: None,
            alliance: None,
            facts: Vec::new(),
            badges: Vec::new(),
        }
    }

    /// A line under the name.
    pub fn subtitle(mut self, text: impl Into<String>) -> Self {
        self.subtitle = Some(text.into());
        self
    }

    pub fn corporation(mut self, corporation: Entity) -> Self {
        self.corporation = Some(corporation);
        self
    }

    pub fn alliance(mut self, alliance: Entity) -> Self {
        self.alliance = Some(alliance);
        self
    }

    /// One fact in the grid (at most 40): a label over its value.
    pub fn fact(mut self, label: impl Into<String>, value: impl Into<Value>) -> Self {
        self.facts.push((label.into(), value.into()));
        self
    }

    /// A badge after the name (at most 8).
    pub fn badge(mut self, badge: Badge) -> Self {
        self.badges.push(badge);
        self
    }
}

impl CardGrid {
    /// An empty grid; add cards with [`CardGrid::card`] or
    /// [`CardGrid::linked`] (at most 100).
    pub fn new() -> Self {
        Self {
            items: Vec::new(),
            register: false,
        }
    }

    /// Start with Tether's "Register Character" card, which opens
    /// character registration. Only drawn for plugins with user scopes.
    pub fn register(mut self) -> Self {
        self.register = true;
        self
    }

    /// A card that opens nothing.
    pub fn card(mut self, profile: Profile) -> Self {
        self.items.push(ProfileCard {
            profile,
            link: None,
        });
        self
    }

    /// A card opening one of your pages (a link path, e.g.
    /// `character/90000001`).
    pub fn linked(mut self, profile: Profile, path: impl Into<String>) -> Self {
        self.items.push(ProfileCard {
            profile,
            link: Some(path.into()),
        });
        self
    }
}

impl Default for CardGrid {
    fn default() -> Self {
        Self::new()
    }
}

impl CodeBlock {
    /// Text to copy, at most 16 KiB, e.g. a fitting in EFT format.
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            title: None,
            text: text.into(),
            copy_label: None,
        }
    }

    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// The button's words ("Copy" otherwise), e.g. "Copy fitting".
    pub fn copy_label(mut self, label: impl Into<String>) -> Self {
        self.copy_label = Some(label.into());
        self
    }
}

impl Progress {
    /// Fills live between two RFC 3339 instants, e.g. a skill's start and
    /// finish.
    pub fn between(mut self, from: impl Into<String>, to: impl Into<String>) -> Self {
        self.from = Some(from.into());
        self.to = Some(to.into());
        self
    }

    /// A few words above the bar.
    pub fn label(mut self, text: impl Into<String>) -> Self {
        self.label = Some(text.into());
        self
    }
}

fn entity(kind: EntityKind, id: i64, name: impl Into<String>) -> Entity {
    Entity {
        kind,
        id,
        name: name.into(),
        link: None,
    }
}

impl Entity {
    /// Its name links to one of your pages about it: a row's name opening
    /// its record (`character(id, name).link(format!("character/{id}"))`),
    /// or with a query, its record panel (see [`RecordPanel::new`]).
    pub fn link(mut self, path: impl Into<String>) -> Self {
        self.link = Some(path.into());
        self
    }
}

/// A character: portrait and name.
pub fn character(id: i64, name: impl Into<String>) -> Entity {
    entity(EntityKind::Character, id, name)
}

/// A corporation: logo and name.
pub fn corporation(id: i64, name: impl Into<String>) -> Entity {
    entity(EntityKind::Corporation, id, name)
}

/// An alliance: logo and name.
pub fn alliance(id: i64, name: impl Into<String>) -> Entity {
    entity(EntityKind::Alliance, id, name)
}

/// A faction: logo and name.
pub fn faction(id: i64, name: impl Into<String>) -> Entity {
    entity(EntityKind::Faction, id, name)
}

/// An item or ship type: icon and name.
pub fn item_type(id: i64, name: impl Into<String>) -> Entity {
    entity(EntityKind::Type, id, name)
}

/// The time left until an RFC 3339 instant, ticking live ("2d 4h 13m");
/// "done" once it has passed.
pub fn countdown(rfc3339: impl Into<String>) -> Value {
    Value::Countdown(rfc3339.into())
}

/// A bar filled to `fraction` (0 to 1). See [`Progress::between`] for one
/// that fills live.
pub fn progress(fraction: f64) -> Progress {
    Progress {
        fraction,
        from: None,
        to: None,
        label: None,
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

/// One of this plugin's pages as its full address, to share outside
/// Tether (a register link in fleet chat): read-only, with a Copy button.
/// `path` is a link path; Tether writes the site's address before it.
pub fn share(path: impl Into<String>) -> Value {
    Value::Share(path.into())
}

/// Tether's own Add owner button, with these words ("Log in with the fleet
/// boss"; "Add owner" when empty), for apps with data-source scopes: one
/// EVE login adds one of the viewer's characters as your data source and
/// brings them back to this page with its id as `owner` in the query.
/// That query is untrusted (anyone can type one): use it only if it's one
/// of the viewer's characters and in [`esi::data_sources`]. Only drawn for
/// those who may add owners (holders of one of your `add_...`
/// permissions, and app admins).
pub fn add_owner(label: impl Into<String>) -> Value {
    Value::AddOwner(label.into())
}

/// A link to another page of this plugin, relative to its pages.
pub fn link(label: impl Into<String>, path: impl Into<String>) -> Link {
    Link {
        label: label.into(),
        path: path.into(),
        primary: false,
    }
}

impl Link {
    /// Drawn as a primary button. At most one per screen region.
    pub fn primary(mut self) -> Self {
        self.primary = true;
        self
    }
}

/// A button that posts to your `submit` as `form`, like a one-button form
/// (see [`Action`]): add the row's id with [`Action::field`].
pub fn action(label: impl Into<String>, form: impl Into<String>) -> Action {
    Action {
        label: label.into(),
        form: form.into(),
        fields: Vec::new(),
        tone: Tone::Neutral,
        confirm: None,
    }
}

/// A row's buttons side by side (at most 4).
pub fn actions(actions: Vec<Action>) -> Value {
    Value::Actions(actions)
}

impl Action {
    /// A hidden value it posts (at most 10), e.g. `.field("id", "42")`.
    pub fn field(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.fields.push((name.into(), value.into()));
        self
    }

    /// `Tone::Danger` for destructive actions, `Tone::Accent` for a
    /// region's one primary action.
    pub fn tone(mut self, tone: Tone) -> Self {
        self.tone = tone;
        self
    }

    /// Ask first, stating what will happen: "Its 4 members lose access."
    pub fn confirm(mut self, sentence: impl Into<String>) -> Self {
        self.confirm = Some(sentence.into());
        self
    }
}

impl From<Action> for Value {
    fn from(action: Action) -> Self {
        Value::Action(action)
    }
}

/// A skill's level as EVE's five squares: `trained` filled (0 to 5), and
/// the level in training, if any, outlined.
pub fn levels(trained: u8, training: Option<u8>) -> Value {
    Value::Levels(Levels { trained, training })
}

/// One part of a composition: its label, amount (above 0) and grade, 0 to
/// 4, darker to brighter by value (a moon's ores: R4 = 0 to R64 = 4).
pub fn part(label: impl Into<String>, amount: f64, grade: u8) -> Share {
    Share {
        label: label.into(),
        amount,
        grade,
    }
}

/// What something is made of, as a small ring (1 to 8 parts).
pub fn composition(parts: Vec<Share>) -> Value {
    Value::Composition(Composition {
        parts,
        center: None,
    })
}

/// The same ring drawn large, with a few words in its middle and a legend
/// beside it (a moon's page: its value).
pub fn composition_large(parts: Vec<Share>, center: impl Into<String>) -> Value {
    Value::Composition(Composition {
        parts,
        center: Some(center.into()),
    })
}

/// Shield, armor and hull (each 0 to 1) as EVE's three rings; `alarm`
/// makes the core pulse (under attack, reinforced).
pub fn defenses(shield: f64, armor: f64, hull: f64, alarm: bool) -> Value {
    Value::Defenses(Defenses {
        shield,
        armor,
        hull,
        alarm,
    })
}

impl Timeline {
    /// A timeline over `from` to `to` (RFC 3339, at most 60 days).
    pub fn new(from: impl Into<String>, to: impl Into<String>) -> Self {
        Timeline {
            title: None,
            from: from.into(),
            to: to.into(),
            lanes: Vec::new(),
            windows: Vec::new(),
        }
    }

    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// A lane (at most 20), with its events (at most 50).
    pub fn lane(mut self, lane: Lane) -> Self {
        self.lanes.push(lane);
        self
    }

    /// A shaded stretch across every lane, e.g. prime time.
    pub fn window(mut self, from: impl Into<String>, to: impl Into<String>) -> Self {
        self.windows.push(Window {
            from: from.into(),
            to: to.into(),
        });
        self
    }
}

impl Lane {
    pub fn new(label: impl Into<String>) -> Self {
        Lane {
            label: label.into(),
            caption: None,
            items: Vec::new(),
        }
    }

    /// A few words under the label.
    pub fn caption(mut self, caption: impl Into<String>) -> Self {
        self.caption = Some(caption.into());
        self
    }

    pub fn item(mut self, item: LaneItem) -> Self {
        self.items.push(item);
        self
    }
}

impl LaneItem {
    /// An event at `at` (RFC 3339).
    pub fn new(label: impl Into<String>, at: impl Into<String>) -> Self {
        LaneItem {
            label: label.into(),
            at: at.into(),
            until: None,
            tone: Tone::Neutral,
            planned: false,
            link: None,
        }
    }

    /// With an end: drawn as a bar.
    pub fn until(mut self, until: impl Into<String>) -> Self {
        self.until = Some(until.into());
        self
    }

    /// `Warning` for what needs attention soon, `Danger` for hostile,
    /// `Success` for friendly.
    pub fn tone(mut self, tone: Tone) -> Self {
        self.tone = tone;
        self
    }

    /// A proposal, not something that happens yet: drawn dashed.
    pub fn planned(mut self) -> Self {
        self.planned = true;
        self
    }

    /// One of the plugin's pages about it.
    pub fn link(mut self, path: impl Into<String>) -> Self {
        self.link = Some(path.into());
        self
    }
}
