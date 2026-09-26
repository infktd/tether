//! The ESI endpoints plugins may call (F16, N8): a fixed catalogue, each
//! with the scope it needs and what it's about (a character, or the
//! corporation of a data-source character).
//!
//! The host fills in the character and corporation ids itself. A plugin
//! names an endpoint and whose token to use; it never builds a URL. That
//! matters beyond tidiness: eve-esi-client caches by URL alone, not by
//! token, so a plugin that could choose ids could read another
//! character's cached data without ESI ever seeing the request.
//!
//! Calls go through the shared client's rate limits, error-limit backoff
//! and cache (with a client that carries the character's token), and feed
//! the error budget. Responses reach the plugin as JSON.
//!
//! One limit of the shared cache: corporation endpoints are cached by URL
//! across data sources, so while one source keeps an entry fresh, another
//! source of the same corporation reads it without ESI checking its roles.
//! Admins approve data sources knowing that (the approval page says so).

use std::time::Duration;

use eve_esi_client::{Client, ClientInfo};
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue};
use tether_core::Secret;

use crate::client::{Esi, EsiError, Priority};

/// What an endpoint is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum About {
    /// A character, with its own token (a user's consented scopes).
    Character,
    /// A data-source character's corporation, with that character's token.
    Corporation,
}

/// One endpoint plugins may call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Endpoint {
    /// The name plugins use.
    pub name: &'static str,
    /// The scope the token must carry.
    pub scope: &'static str,
    pub about: About,
    /// Numbered pages (`X-Pages`).
    pub paged: bool,
    /// Extra ids the plugin supplies, as `(name, what)`.
    pub params: &'static [&'static str],
}

const MINING: &str = "esi-industry.read_corporation_mining.v1";

pub const ENDPOINTS: &[Endpoint] = &[
    Endpoint {
        name: "corporation-mining-extractions",
        scope: MINING,
        about: About::Corporation,
        paged: true,
        params: &[],
    },
    Endpoint {
        name: "corporation-mining-observers",
        scope: MINING,
        about: About::Corporation,
        paged: true,
        params: &[],
    },
    Endpoint {
        name: "corporation-mining-observer",
        scope: MINING,
        about: About::Corporation,
        paged: true,
        params: &["observer_id"],
    },
    Endpoint {
        name: "corporation-structures",
        scope: "esi-corporations.read_structures.v1",
        about: About::Corporation,
        paged: true,
        params: &[],
    },
    Endpoint {
        // Who holds which corporation roles (a Director's or Personnel
        // Manager's view): Moon Mining finds Station Managers with it.
        name: "corporation-roles",
        scope: "esi-corporations.read_corporation_membership.v1",
        about: About::Corporation,
        paged: false,
        params: &[],
    },
    Endpoint {
        // A moon's name and system (public data, read with the data
        // source's token like the rest: /universe/names doesn't do moons).
        name: "universe-moon",
        scope: MINING,
        about: About::Corporation,
        paged: false,
        params: &["moon_id"],
    },
    Endpoint {
        name: "character-skills",
        scope: "esi-skills.read_skills.v1",
        about: About::Character,
        paged: false,
        params: &[],
    },
    Endpoint {
        name: "character-skillqueue",
        scope: "esi-skills.read_skillqueue.v1",
        about: About::Character,
        paged: false,
        params: &[],
    },
    Endpoint {
        name: "character-ship",
        scope: "esi-location.read_ship_type.v1",
        about: About::Character,
        paged: false,
        params: &[],
    },
    Endpoint {
        name: "character-assets",
        scope: "esi-assets.read_assets.v1",
        about: About::Character,
        paged: true,
        params: &[],
    },
    Endpoint {
        name: "character-wallet",
        scope: "esi-wallet.read_character_wallet.v1",
        about: About::Character,
        paged: false,
        params: &[],
    },
    Endpoint {
        name: "character-wallet-journal",
        scope: "esi-wallet.read_character_wallet.v1",
        about: About::Character,
        paged: true,
        params: &[],
    },
    Endpoint {
        name: "character-clones",
        scope: "esi-clones.read_clones.v1",
        about: About::Character,
        paged: false,
        params: &[],
    },
    Endpoint {
        name: "character-implants",
        scope: "esi-clones.read_implants.v1",
        about: About::Character,
        paged: false,
        params: &[],
    },
    Endpoint {
        name: "character-location",
        scope: "esi-location.read_location.v1",
        about: About::Character,
        paged: false,
        params: &[],
    },
];

pub fn endpoint(name: &str) -> Option<&'static Endpoint> {
    ENDPOINTS.iter().find(|e| e.name == name)
}

/// Ids the host fills in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Target {
    pub character_id: i64,
    pub corporation_id: i64,
}

/// A response: the JSON body, and how many pages there are.
#[derive(Debug, Clone, PartialEq)]
pub struct Response {
    pub body: serde_json::Value,
    pub pages: u32,
}

/// How long one plugin ESI request may take.
const TIMEOUT: Duration = Duration::from_secs(30);

fn json<T: serde::Serialize>(value: &T) -> Result<serde_json::Value, EsiError> {
    serde_json::to_value(value).map_err(|e| EsiError::Unavailable(e.to_string()))
}

fn pages(headers: &reqwest::header::HeaderMap) -> u32 {
    headers
        .get("x-pages")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
        .unwrap_or(1)
}

impl Esi {
    /// The shared client, sending `token` with every request: same rate
    /// limits, error-limit backoff and cache.
    fn with_token(&self, token: &Secret<String>) -> Result<Client, EsiError> {
        let mut headers = HeaderMap::new();
        let mut auth = HeaderValue::from_str(&format!("Bearer {}", token.expose()))
            .map_err(|_| EsiError::InvalidInput("the token isn't a valid header".into()))?;
        auth.set_sensitive(true);
        headers.insert(AUTHORIZATION, auth);
        headers.insert(
            "x-compatibility-date",
            HeaderValue::from_static(eve_esi_client::COMPATIBILITY_DATE),
        );
        let http = tether_net::Outbound::library_client(
            self.allowlist().clone(),
            self.user_agent(),
            TIMEOUT,
            headers,
        )
        .map_err(|e| EsiError::Config(e.to_string()))?;
        let shared = self.client();
        // The token rides on this client: its destination must be listed,
        // scheme and port included, not just resolve through the list.
        self.allowlist()
            .check(shared.baseurl())
            .map_err(|e| EsiError::Config(e.to_string()))?;
        Ok(Client::new_with_client(
            shared.baseurl(),
            http,
            shared.inner().clone(),
        ))
    }

    /// A corporation's member list (character ids), read with a member's
    /// token carrying `esi-corporations.read_corporation_membership.v1`
    /// (Corp Stats). No in-game role is needed.
    pub async fn corporation_members(
        &self,
        token: &Secret<String>,
        corporation_id: i64,
    ) -> Result<Vec<i64>, EsiError> {
        let client = self.with_token(token)?;
        let request = client
            .get_corporations_corporation_id_members()
            .corporation_id(corporation_id)
            .send();
        Ok(self
            .call_full(Priority::Bulk, request)
            .await?
            .into_inner()
            .0)
    }

    /// Calls a catalogue endpoint for `target` with that character's
    /// token. `params` are the endpoint's extra ids, checked here; `page`
    /// is for paged endpoints.
    pub async fn plugin_get(
        &self,
        endpoint: &Endpoint,
        token: &Secret<String>,
        target: Target,
        params: &[(String, String)],
        page: Option<u32>,
    ) -> Result<Response, EsiError> {
        let id = |name: &str| -> Result<i64, EsiError> {
            params
                .iter()
                .find(|(k, _)| k == name)
                .and_then(|(_, v)| v.parse().ok())
                .ok_or_else(|| EsiError::InvalidInput(format!("{name} must be a number")))
        };
        let page = page.and_then(std::num::NonZeroU32::new);
        let client = self.with_token(token)?;
        let character = target.character_id;
        let corporation = target.corporation_id;
        let priority = Priority::Bulk;
        macro_rules! get {
            ($request:expr) => {{
                let response = self.call_full(priority, $request.send()).await?;
                let pages = pages(response.headers());
                Ok(Response {
                    body: json(&response.into_inner())?,
                    pages,
                })
            }};
        }
        macro_rules! paged {
            ($request:expr) => {{
                let request = $request;
                match page {
                    Some(p) => get!(request.page(p)),
                    None => get!(request),
                }
            }};
        }
        match endpoint.name {
            "corporation-mining-extractions" => paged!(
                client
                    .get_corporation_corporation_id_mining_extractions()
                    .corporation_id(corporation)
            ),
            "corporation-mining-observers" => paged!(
                client
                    .get_corporation_corporation_id_mining_observers()
                    .corporation_id(corporation)
            ),
            "corporation-mining-observer" => paged!(
                client
                    .get_corporation_corporation_id_mining_observers_observer_id()
                    .corporation_id(corporation)
                    .observer_id(id("observer_id")?)
            ),
            "corporation-structures" => paged!(
                client
                    .get_corporations_corporation_id_structures()
                    .corporation_id(corporation)
            ),
            "corporation-roles" => {
                // Only who holds which roles, not grantable roles or where:
                // enough for Moon Mining, less intel for any other plugin.
                let response = self
                    .call_full(
                        priority,
                        client
                            .get_corporations_corporation_id_roles()
                            .corporation_id(corporation)
                            .send(),
                    )
                    .await?;
                let pages = pages(response.headers());
                let members: Vec<serde_json::Value> = response
                    .into_inner()
                    .iter()
                    .map(|m| {
                        serde_json::json!({
                            "character_id": m.character_id,
                            "roles": m.roles,
                        })
                    })
                    .collect();
                Ok(Response {
                    body: serde_json::Value::Array(members),
                    pages,
                })
            }
            "universe-moon" => get!(client.get_universe_moons_moon_id().moon_id(id("moon_id")?)),
            "character-skills" => get!(
                client
                    .get_characters_character_id_skills()
                    .character_id(character)
            ),
            "character-skillqueue" => get!(
                client
                    .get_characters_character_id_skillqueue()
                    .character_id(character)
            ),
            "character-ship" => get!(
                client
                    .get_characters_character_id_ship()
                    .character_id(character)
            ),
            "character-assets" => paged!(
                client
                    .get_characters_character_id_assets()
                    .character_id(character)
            ),
            "character-wallet" => get!(
                client
                    .get_characters_character_id_wallet()
                    .character_id(character)
            ),
            "character-wallet-journal" => paged!(
                client
                    .get_characters_character_id_wallet_journal()
                    .character_id(character)
            ),
            "character-clones" => get!(
                client
                    .get_characters_character_id_clones()
                    .character_id(character)
            ),
            "character-implants" => get!(
                client
                    .get_characters_character_id_implants()
                    .character_id(character)
            ),
            "character-location" => get!(
                client
                    .get_characters_character_id_location()
                    .character_id(character)
            ),
            other => Err(EsiError::InvalidInput(format!("no endpoint {other}"))),
        }
    }
}
