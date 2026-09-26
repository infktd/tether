//! What plugins reach through the host that needs Tether's own state:
//! ESI (with the right token), who is looking, and Discord. The web crate
//! implements [`Services`]; this crate only checks call counts and when a
//! call is allowed (pages can't send to Discord).

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

pub use crate::host::tether::plugin::discord::{Channel, Error as DiscordError, Mention};
pub use crate::host::tether::plugin::esi::{
    Error as EsiError, Named, Response as EsiResponse, Subject,
};
pub use crate::host::tether::plugin::identity::{Builtin, Character, State, Viewer};

/// ESI calls in one job run or form submission.
pub const MAX_ESI_CALLS: usize = 100;
/// ESI calls in one page render: pages run on every view.
pub const MAX_ESI_CALLS_PAGE: usize = 20;

/// ESI requests one call of a catalogue endpoint makes, which is what it
/// costs of the budgets above. `fleet-members` reads the character's fleet
/// and then its members.
pub fn esi_cost(endpoint: &str) -> usize {
    match endpoint {
        // Two ESI requests each: fleet then members; system then
        // constellation.
        "fleet-members" | "universe-system" => 2,
        _ => 1,
    }
}
/// Discord messages in one plugin call.
pub const MAX_DISCORD_SENDS: usize = 5;

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
    ) -> Fut<Result<EsiResponse, EsiError>>;
    fn esi_characters(&self, plugin: String) -> Fut<Vec<Character>>;
    fn esi_data_sources(&self, plugin: String) -> Fut<Vec<Character>>;
    fn esi_names(&self, plugin: String, ids: Vec<i64>) -> Fut<Result<Vec<Named>, EsiError>>;
    fn discord_channels(&self, plugin: String) -> Fut<Vec<Channel>>;
    fn discord_send(
        &self,
        plugin: String,
        channel: String,
        text: String,
        mention: Mention,
    ) -> Fut<Result<(), DiscordError>>;
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
