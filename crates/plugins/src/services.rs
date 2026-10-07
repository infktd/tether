//! What plugins reach through the host that needs Tether's own state:
//! ESI (with the right token), who is looking, Discord, and outbound HTTP
//! to approved hosts. The web crate implements [`Services`]; this crate
//! only checks call counts and when a call is allowed (pages can't send
//! to Discord, and can only GET).

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

pub use crate::host::tether::plugin::discord::{
    Channel, Embed, EmbedAuthor, EmbedField, Error as DiscordError, Image, Mention, Message, Ping,
    PingGroup,
};
pub use crate::host::tether::plugin::doctrines::{
    Doctrine, Error as DoctrineError, Shared as SharedDoctrine,
};
pub use crate::host::tether::plugin::downloads::{Error as DownloadError, File as DownloadFile};
pub use crate::host::tether::plugin::esi::{
    Error as EsiError, FetchError, Named, Response as EsiResponse, Subject,
};

/// `esi.get`'s answer for what `esi.fetch` answers: a token short of the
/// endpoint's scope is `not-registered`, as `get` has always said.
pub fn get_error(err: FetchError) -> EsiError {
    match err {
        FetchError::NotAllowed(why) => EsiError::NotAllowed(why),
        FetchError::NotRegistered | FetchError::MissingScope(_) => EsiError::NotRegistered,
        FetchError::NotADataSource => EsiError::NotADataSource,
        FetchError::Token => EsiError::Token,
        FetchError::Status(status) => EsiError::Status(status),
        FetchError::Invalid(why) => EsiError::Invalid(why),
        FetchError::TooLarge => EsiError::TooLarge,
        FetchError::Unavailable => EsiError::Unavailable,
    }
}

/// [`EsiError`] as `esi.fetch` answers it (the host's own refusals, such
/// as a call over the budget).
pub fn fetch_error(err: EsiError) -> FetchError {
    match err {
        EsiError::NotAllowed(why) => FetchError::NotAllowed(why),
        EsiError::NotRegistered => FetchError::NotRegistered,
        EsiError::NotADataSource => FetchError::NotADataSource,
        EsiError::Token => FetchError::Token,
        EsiError::Status(status) => FetchError::Status(status),
        EsiError::Invalid(why) => FetchError::Invalid(why),
        EsiError::TooLarge => FetchError::TooLarge,
        EsiError::Unavailable => FetchError::Unavailable,
    }
}
pub use crate::host::tether::plugin::filters::{
    Error as FilterError, Setting as FilterWanted, Value as FilterValue,
};
pub use crate::host::tether::plugin::http::{
    Error as HttpError, Method as HttpMethod, Request as HttpRequest, Response as HttpResponse,
};
pub use crate::host::tether::plugin::identity::{
    Builtin, Character, Group, Member, MemberCharacter, Owner, State, Viewer,
};
pub use crate::host::tether::plugin::notify::{Error as NotifyError, Level as NotifyLevel};
pub use crate::host::tether::plugin::timers::{Error as TimerError, Shared as SharedTimer, Timer};

/// ESI calls in one job run or form submission.
pub const MAX_ESI_CALLS: usize = 100;
/// ESI writes in one form post: one button, one change in EVE.
pub const MAX_ESI_WRITES: usize = 1;
/// The largest body a plugin may send ESI (a fitting is a few KiB).
pub const MAX_ESI_BODY: usize = 64 * 1024;
/// ESI calls in one page render: pages run on every view.
pub const MAX_ESI_CALLS_PAGE: usize = 20;

/// ESI requests one call of a catalogue endpoint makes, which is what it
/// costs of the budgets above. `fleet-members` reads the character's fleet
/// and then its members.
pub fn esi_cost(endpoint: &str) -> usize {
    match endpoint {
        // Two ESI requests each: fleet then members; system then
        // constellation. Structure assets and asset places cost two as
        // well: the host reads the assets' pages in the background, once
        // an hour for a data source, not charged page by page.
        "fleet-members"
        | "universe-system"
        | "corporation-structure-assets"
        | "corporation-hangar-assets"
        | "corporation-asset-places" => 2,
        _ => 1,
    }
}

/// An ESI call's answer, and the ESI requests it made beyond its
/// [`esi_cost`]: an answer read a second time, raw, when its typed read
/// failed (a value CCP added to one of ESI's lists). The host counts
/// those against the call's budget too.
#[derive(Debug, Clone)]
pub struct EsiReply {
    pub response: EsiResponse,
    pub extra_calls: usize,
}

/// Filter reports in one plugin call.
pub const MAX_FILTER_REPORTS: usize = 50;
/// Discord messages in one plugin call.
pub const MAX_DISCORD_SENDS: usize = 5;
/// Pings in one `discord.send-message`.
pub const MAX_PINGS: usize = 10;
/// `notify` calls in one plugin call.
pub const MAX_NOTIFY_CALLS: usize = 10;
/// HTTP requests in one job run or form submission.
pub const MAX_HTTP_CALLS: usize = 20;
/// HTTP requests in one page render: pages run on every view.
pub const MAX_HTTP_CALLS_PAGE: usize = 5;
/// The largest request body a plugin may send.
pub const MAX_HTTP_REQUEST_BODY: usize = 64 * 1024;
/// `identity.submitter-characters` lookups in one plugin call: a review
/// queue's worth.
pub const MAX_SUBMITTER_LOOKUPS: usize = 1000;

pub type Fut<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// Tether's side of the ESI and Discord interfaces. `plugin` is the
/// calling plugin's id, always from the host, never from the plugin.
pub trait Services: Send + Sync + std::fmt::Debug {
    /// A read, for `esi.get` and `esi.fetch`: `get` answers
    /// [`FetchError::MissingScope`] as `not-registered` ([`get_error`]).
    fn esi_get(
        &self,
        plugin: String,
        endpoint: String,
        subject: Subject,
        params: Vec<(String, String)>,
        page: Option<u32>,
    ) -> Fut<Result<EsiReply, FetchError>>;
    /// A write ([`crate::host`] has checked it's the pilot's own form post
    /// and `character` one of `account`'s characters).
    fn esi_post(
        &self,
        plugin: String,
        endpoint: String,
        character: i64,
        account: i64,
        body: String,
    ) -> Fut<Result<EsiReply, EsiError>>;
    fn esi_characters(&self, plugin: String) -> Fut<Vec<Character>>;
    fn esi_data_sources(&self, plugin: String) -> Fut<Vec<Character>>;
    fn esi_names(&self, plugin: String, ids: Vec<i64>) -> Fut<Result<Vec<Named>, EsiError>>;
    /// Who owns each of `plugin`'s [`esi_characters`](Self::esi_characters):
    /// `None` for every plugin but the first-party one allowed to know.
    fn identity_owners(&self, plugin: String) -> Fut<Option<Vec<Owner>>>;
    /// Every account holding one of `plugin`'s permissions with all its
    /// characters, registered or not: `None` for every plugin but the
    /// first-party one allowed to know.
    fn identity_members(&self, plugin: String) -> Fut<Option<Vec<Member>>>;
    /// The characters now on the account behind one of `plugin`'s
    /// submitter references: `None` for every plugin but the first-party
    /// one allowed to know, and for a reference that reaches nobody.
    fn identity_submitter_characters(
        &self,
        plugin: String,
        reference: String,
    ) -> Fut<Option<Vec<Character>>>;
    /// The groups of `account` (the viewer's, as the host built it), for a
    /// `plugin` approved for `groups`; none otherwise.
    fn identity_groups(&self, plugin: String, account: i64) -> Fut<Vec<Group>>;
    /// The groups `account` may be offered to pick from: neither Hidden
    /// nor Internal, and its own; all of them for group admins. None
    /// for a `plugin` not approved for `groups`.
    fn identity_all_groups(&self, plugin: String, account: i64) -> Fut<Vec<Group>>;
    /// Whether `account` is an active superuser.
    fn identity_superuser(&self, account: i64) -> Fut<bool>;
    fn discord_channels(&self, plugin: String) -> Fut<Vec<Channel>>;
    /// The groups with a Discord role mapped, for a `plugin` approved for
    /// `mention_groups`; none otherwise.
    fn discord_ping_groups(&self, plugin: String) -> Fut<Vec<PingGroup>>;
    /// `text`, or with `embed` a card and no text of its own, its title a
    /// link to the plugin's own `page` when one is given.
    fn discord_send(
        &self,
        plugin: String,
        channel: String,
        text: String,
        embed: Option<Embed>,
        page: Option<String>,
        mention: Mentions,
    ) -> Fut<Result<(), DiscordError>>;
    /// One outbound HTTPS request, to a host approved for `plugin`;
    /// `from_page` while rendering a page.
    fn http_send(
        &self,
        plugin: String,
        request: HttpRequest,
        from_page: bool,
    ) -> Fut<Result<HttpResponse, HttpError>>;
    /// The settings of `plugin`'s Secure Groups filters in use.
    fn filters_wanted(&self, plugin: String) -> Fut<Vec<FilterWanted>>;
    /// Replaces `plugin`'s values for one filter setting.
    fn filters_report(
        &self,
        plugin: String,
        name: String,
        config: String,
        values: Vec<FilterValue>,
    ) -> Fut<Result<(), FilterError>>;
    /// Replaces `plugin`'s published timers.
    fn timers_publish(&self, plugin: String, timers: Vec<Timer>) -> Fut<Result<(), TimerError>>;
    /// Replaces `plugin`'s shared doctrines.
    fn doctrines_publish(
        &self,
        plugin: String,
        doctrines: Vec<Doctrine>,
        see_all: Option<String>,
    ) -> Fut<Result<(), DoctrineError>>;
    fn downloads_begin(
        &self,
        plugin: String,
        name: String,
        title: String,
        permission: String,
        header: Vec<String>,
    ) -> Fut<Result<u32, DownloadError>>;
    fn downloads_append(
        &self,
        plugin: String,
        name: String,
        build: u32,
        rows: Vec<Vec<String>>,
    ) -> Fut<Result<(), DownloadError>>;
    fn downloads_finish(
        &self,
        plugin: String,
        name: String,
        build: u32,
    ) -> Fut<Result<(), DownloadError>>;
    fn downloads_files(&self, plugin: String) -> Fut<Vec<DownloadFile>>;
    /// A notice to `account`, if it holds one of `plugin`'s permissions:
    /// whether it was sent.
    fn notify_account(
        &self,
        plugin: String,
        account: i64,
        title: String,
        message: String,
        level: NotifyLevel,
    ) -> Fut<Result<bool, NotifyError>>;
    /// A notice to every holder of `plugin`'s own `permission` but
    /// `except`: how many it reached.
    fn notify_holders(
        &self,
        plugin: String,
        permission: String,
        title: String,
        message: String,
        level: NotifyLevel,
        except: Option<i64>,
    ) -> Fut<Result<u32, NotifyError>>;
    /// `account`'s submitter reference for `plugin`: only for the viewer
    /// of a form post, which the caller checks.
    fn notify_submitter_reference(
        &self,
        plugin: String,
        account: i64,
    ) -> Fut<Result<String, NotifyError>>;
    /// A notice to the account behind one of `plugin`'s submitter
    /// references: whether it was sent.
    fn notify_submitter(
        &self,
        plugin: String,
        reference: String,
        title: String,
        message: String,
        level: NotifyLevel,
    ) -> Fut<Result<bool, NotifyError>>;
    /// Shared doctrines `account` may see, for `plugin` to offer; none
    /// without an account (a job).
    fn doctrines_published(
        &self,
        plugin: String,
        account: Option<i64>,
    ) -> Fut<Result<Vec<SharedDoctrine>, DoctrineError>>;
    /// Every app's published timers, for a plugin that may read them;
    /// corporation-only ones only for `viewer_corporation`.
    fn timers_published(
        &self,
        plugin: String,
        viewer_corporation: Option<i64>,
    ) -> Fut<Result<Vec<SharedTimer>, TimerError>>;
}

pub type Shared = Arc<dyn Services>;

/// Whom a Discord message pings.
#[derive(Debug, Clone)]
pub enum Mentions {
    /// `send`'s one mention: refused when no role is mapped to its state.
    One(Mention),
    /// `send-message`'s pings: each without a role is left out and logged.
    Pings(Vec<Ping>),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_request_endpoints_cost_two() {
        assert_eq!(esi_cost("fleet-members"), 2);
        assert_eq!(esi_cost("character-skills"), 1);
    }
}
