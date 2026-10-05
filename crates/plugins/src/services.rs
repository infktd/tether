//! What plugins reach through the host that needs Tether's own state:
//! ESI (with the right token), who is looking, Discord, and outbound HTTP
//! to approved hosts. The web crate implements [`Services`]; this crate
//! only checks call counts and when a call is allowed (pages can't send
//! to Discord, and can only GET).

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

pub use crate::host::tether::plugin::discord::{
    Channel, Embed, EmbedAuthor, EmbedField, Error as DiscordError, Image, Mention,
};
pub use crate::host::tether::plugin::doctrines::{
    Doctrine, Error as DoctrineError, Shared as SharedDoctrine,
};
pub use crate::host::tether::plugin::downloads::{Error as DownloadError, File as DownloadFile};
pub use crate::host::tether::plugin::esi::{
    Error as EsiError, Named, Response as EsiResponse, Subject,
};
pub use crate::host::tether::plugin::filters::{
    Error as FilterError, Setting as FilterWanted, Value as FilterValue,
};
pub use crate::host::tether::plugin::http::{
    Error as HttpError, Method as HttpMethod, Request as HttpRequest, Response as HttpResponse,
};
pub use crate::host::tether::plugin::identity::{Builtin, Character, Group, Owner, State, Viewer};
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
        // constellation; an assets page then the structures it's checked
        // against (cached, one page mostly).
        "fleet-members" | "universe-system" | "corporation-structure-assets" => 2,
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
/// `notify` calls in one plugin call.
pub const MAX_NOTIFY_CALLS: usize = 10;
/// HTTP requests in one job run or form submission.
pub const MAX_HTTP_CALLS: usize = 20;
/// HTTP requests in one page render: pages run on every view.
pub const MAX_HTTP_CALLS_PAGE: usize = 5;
/// The largest request body a plugin may send.
pub const MAX_HTTP_REQUEST_BODY: usize = 64 * 1024;

pub type Fut<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// Tether's side of the ESI and Discord interfaces. `plugin` is the
/// calling plugin's id, always from the host, never from the plugin.
pub trait Services: Send + Sync + std::fmt::Debug {
    fn esi_get(
        &self,
        plugin: String,
        endpoint: String,
        subject: Subject,
        params: Vec<(String, String)>,
        page: Option<u32>,
    ) -> Fut<Result<EsiReply, EsiError>>;
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
    /// `text`, or with `embed` a card and no text of its own.
    fn discord_send(
        &self,
        plugin: String,
        channel: String,
        text: String,
        embed: Option<Embed>,
        mention: Mention,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_request_endpoints_cost_two() {
        assert_eq!(esi_cost("fleet-members"), 2);
        assert_eq!(esi_cost("character-skills"), 1);
    }
}
