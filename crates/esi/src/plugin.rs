//! The ESI endpoints plugins may call (F16, N8): a fixed catalogue, each
//! with the scope it needs and what it's about (a character, or the
//! corporation of a data-source character), plus a few public endpoints
//! read without any token ([`About::Public`]).
//!
//! Public endpoints take ids the plugin gives (a killmail's id and hash):
//! they read nobody's data, so there is no token to misuse. Every plugin
//! may call them; the subject a plugin passes isn't used.
//!
//! The host fills in the character and corporation ids itself. A plugin
//! names an endpoint and whose token to use; it never builds a URL.
//!
//! Calls share the main client's rate limits and error-limit backoff, and
//! feed the error budget, but never its cache: calls with a character's
//! token go to ESI every time (ESI's own cache still answers repeats
//! cheaply), so ESI checks that character's scopes and roles on every
//! request and nothing a token fetched is stored. Public endpoints skip
//! the cache too (see `uncached`). Responses reach the plugin as JSON.
//!
//! `fleet-members` checks that the data-source character is the fleet boss
//! from `/characters/{id}/fleet`, as of ESI's cached answer (a few
//! seconds), then reads `/fleets/{fleet_id}/members` with the same token.
//! It makes two ESI requests, and costs a plugin two of its per-call ESI
//! budget.

use std::time::Duration;

use eve_esi_client::{Client, ClientInfo, ResponseValue};
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
    /// Public data, read without a token. No scope; any subject.
    Public,
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
const STARBASES: &str = "esi-corporations.read_starbases.v1";
const ASSETS: &str = "esi-assets.read_corporation_assets.v1";

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
        // A moon's name and system (/universe/names doesn't do moons).
        // Public: any plugin, no token. It was read through a mining data
        // source before public endpoints existed; plugins that still pass
        // one work as before (the subject isn't used).
        name: "universe-moon",
        scope: "",
        about: About::Public,
        paged: false,
        params: &["moon_id"],
    },
    Endpoint {
        // A planet's name and system (public; /universe/names doesn't do
        // planets). Structures matches customs offices' notifications
        // with it.
        name: "universe-planet",
        scope: "",
        about: About::Public,
        paged: false,
        params: &["planet_id"],
    },
    Endpoint {
        // Which alliance holds sovereignty where (public): only system
        // and alliance ids, for claims by alliances.
        name: "sovereignty-systems",
        scope: "",
        about: About::Public,
        paged: false,
        params: &[],
    },
    Endpoint {
        // The data-source character's own notifications, trimmed to those
        // about its corporation's structures and moon drills
        // (`STRUCTURE_NOTIFICATIONS`): never mail, wars, contracts, kills
        // or anything else personal. Structures relays them to Discord.
        name: "corporation-structure-notifications",
        scope: "esi-characters.read_notifications.v1",
        about: About::Corporation,
        paged: false,
        params: &[],
    },
    Endpoint {
        // A solar system's name, security and region (public data, read
        // with the data source's token like `universe-moon`: /universe/names
        // doesn't say which region a system is in).
        name: "universe-system",
        scope: "esi-corporations.read_structures.v1",
        about: About::Corporation,
        paged: false,
        params: &["system_id"],
    },
    Endpoint {
        // The corporation's starbases (POS): type, system, moon, state and
        // its timers. CCP requires the Director role.
        name: "corporation-starbases",
        scope: STARBASES,
        about: About::Corporation,
        paged: true,
        params: &[],
    },
    Endpoint {
        // One starbase's fuel bay (fuel blocks and strontium), by the ids
        // `corporation-starbases` gives. Only the fuels, not who may
        // anchor, take fuel or be shot at.
        name: "corporation-starbase",
        scope: STARBASES,
        about: About::Corporation,
        paged: false,
        params: &["starbase_id", "system_id"],
    },
    Endpoint {
        // The corporation's customs offices (POCOs): system, reinforcement
        // window, access and tax rates. CCP requires the Director role.
        name: "corporation-customs-offices",
        scope: "esi-planets.read_customs_offices.v1",
        about: About::Corporation,
        paged: true,
        params: &[],
    },
    Endpoint {
        // The corporation's assets trimmed to what sits in structures'
        // slots and bays (`STRUCTURE_ASSET_FLAGS`: fittings, fighters,
        // fuel, quantum cores, moon material) and its Orbital Skyhooks:
        // never its hangars, cargo, deliveries or anything else. Pages
        // are ESI's (one may come back empty). CCP requires the Director
        // role.
        name: "corporation-structure-assets",
        scope: ASSETS,
        about: About::Corporation,
        paged: true,
        params: &[],
    },
    Endpoint {
        // Names of the corporation's own items (starbases, customs offices
        // as "Customs Office (planet)"), for up to 1,000 `item_ids` (a
        // comma list). ESI names only the corporation's items.
        name: "corporation-asset-names",
        scope: ASSETS,
        about: About::Corporation,
        paged: false,
        params: &["item_ids"],
    },
    Endpoint {
        // Where the corporation's own items are in space, for up to 1,000
        // `item_ids`: Structures finds an Orbital Skyhook's planet (the
        // nearest one) with it, as ESI names none.
        name: "corporation-asset-locations",
        scope: ASSETS,
        about: About::Corporation,
        paged: false,
        params: &["item_ids"],
    },
    Endpoint {
        // The fleet the data-source character runs (aa-afat's ESI fleet
        // tracking): the host finds the fleet from the character's own
        // token and answers only when it is the fleet boss, so a plugin
        // never names a fleet id.
        name: "fleet-members",
        scope: "esi-fleets.read_fleet.v1",
        about: About::Corporation,
        paged: false,
        params: &[],
    },
    // Public: one killmail, by the id and hash a killboard link carries
    // (Ship Replacement checks losses with it). Only the victim, ship,
    // place and time come back, and how many attackers.
    Endpoint {
        name: "killmail",
        scope: "",
        about: About::Public,
        paged: false,
        params: &["killmail_id", "killmail_hash"],
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

/// The notification types `corporation-structure-notifications` passes
/// on: Upwell structures' attacks, reinforcements, fuel, services, power
/// and anchoring, Metenox reagents, starbases, customs offices, Orbital
/// Skyhooks, and moon drills. Everything else a character receives
/// stays with the host.
pub const STRUCTURE_NOTIFICATIONS: &[&str] = &[
    "StructureUnderAttack",
    "StructureLostShields",
    "StructureLostArmor",
    "StructureDestroyed",
    "StructureFuelAlert",
    "StructureServicesOffline",
    "StructureWentLowPower",
    "StructureWentHighPower",
    "StructureOnline",
    "StructureAnchoring",
    "StructureUnanchoring",
    // Metenox moon drills running low on, or out of, magmatic gas.
    "StructureLowReagentsAlert",
    "StructureNoReagentsAlert",
    // Starbases: under attack, low on fuel or strontium.
    "TowerAlertMsg",
    "TowerResourceAlertMsg",
    // Customs offices: attacked, reinforced.
    "OrbitalAttacked",
    "OrbitalReinforced",
    // Orbital Skyhooks.
    "SkyhookDeployed",
    "SkyhookDestroyed",
    "SkyhookLostShields",
    "SkyhookOnline",
    "SkyhookUnderAttack",
    "MoonminingExtractionStarted",
    "MoonminingExtractionFinished",
    "MoonminingAutomaticFracture",
    "MoonminingLaserFired",
    "MoonminingExtractionCancelled",
];

/// A character notification, read loosely (see
/// `corporation-structure-notifications`).
#[derive(serde::Deserialize)]
struct Notification {
    notification_id: i64,
    #[serde(rename = "type")]
    kind: String,
    timestamp: String,
    #[serde(default)]
    text: Option<String>,
}

/// Where `corporation-structure-assets` looks. Slots and bays only
/// structures have: services, fuel bay, quantum core, moon material bay.
pub const STRUCTURE_ASSET_FLAGS: &[&str] = &["StructureFuel", "QuantumCoreRoom", "MoonMaterialBay"];
pub const STRUCTURE_ASSET_FLAG_PREFIXES: &[&str] = &["ServiceSlot"];
/// Slots and bays ships have too (fittings, fighters): passed only for
/// items in the corporation's Upwell structures.
pub const SHARED_ASSET_FLAGS: &[&str] = &["FighterBay"];
pub const SHARED_ASSET_FLAG_PREFIXES: &[&str] =
    &["HiSlot", "MedSlot", "LoSlot", "RigSlot", "FighterTube"];

/// `prefix` then one digit, as `HiSlot0`.
fn numbered(flag: &str, prefixes: &[&str]) -> bool {
    prefixes.iter().any(|p| {
        flag.strip_prefix(p)
            .is_some_and(|n| n.len() == 1 && n.bytes().all(|b| b.is_ascii_digit()))
    })
}

/// Orbital Skyhooks' type ids: ESI lists skyhooks nowhere but the
/// corporation's assets (anchored in space, at their planet).
pub const SKYHOOK_TYPES: &[i64] = &[81080];

/// A corporation asset, read loosely (see `corporation-structure-assets`).
#[derive(serde::Deserialize)]
struct Asset {
    item_id: i64,
    type_id: i64,
    location_id: i64,
    location_flag: String,
    location_type: String,
    quantity: i64,
}

impl Asset {
    /// In a structure's slot or bay, or a skyhook itself. Flags ships
    /// share count only for items in one of `upwell`, the corporation's
    /// Upwell structures (none if those couldn't be read).
    fn about_structures(&self, upwell: Option<&[i64]>) -> bool {
        let flag = self.location_flag.as_str();
        let in_upwell = upwell.is_some_and(|ids| ids.contains(&self.location_id));
        STRUCTURE_ASSET_FLAGS.contains(&flag)
            || numbered(flag, STRUCTURE_ASSET_FLAG_PREFIXES)
            || (in_upwell && self.in_shared_slot())
            || (SKYHOOK_TYPES.contains(&self.type_id) && self.location_type == "solar_system")
    }

    /// In a slot or bay ships have too.
    fn in_shared_slot(&self) -> bool {
        let flag = self.location_flag.as_str();
        SHARED_ASSET_FLAGS.contains(&flag) || numbered(flag, SHARED_ASSET_FLAG_PREFIXES)
    }
}

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

/// Item ids a plugin names, most at once.
pub const MAX_ITEM_IDS: usize = 1000;

/// `item_ids`: 1 to [`MAX_ITEM_IDS`] positive ids, comma-separated, each
/// once.
fn item_ids(params: &[(String, String)]) -> Result<Vec<i64>, EsiError> {
    let bad = || {
        EsiError::InvalidInput(format!(
            "item_ids must be 1 to {MAX_ITEM_IDS} positive ids, comma-separated"
        ))
    };
    let text = params
        .iter()
        .find(|(k, _)| k == "item_ids")
        .map(|(_, v)| v.as_str())
        .ok_or_else(bad)?;
    let mut ids = Vec::new();
    for part in text.split(',') {
        // Refused before reading all of a long list.
        if ids.len() >= MAX_ITEM_IDS {
            return Err(bad());
        }
        let id: i64 = part.trim().parse().map_err(|_| bad())?;
        if id <= 0 {
            return Err(bad());
        }
        ids.push(id);
    }
    ids.sort_unstable();
    ids.dedup();
    if ids.is_empty() || ids.len() > MAX_ITEM_IDS {
        return Err(bad());
    }
    Ok(ids)
}

impl Esi {
    /// The shared client, sending `token` with every request: same rate
    /// limits and error-limit backoff, but no cache. The token rides as a
    /// default header, which eve-esi-client can't see when it keys its
    /// cache, so a cached copy would be keyed by URL alone: another
    /// character's request could be answered with it, and it would land
    /// in Postgres unmarked. Every token-bearing call asks ESI.
    fn with_token(&self, token: &Secret<String>) -> Result<Client, EsiError> {
        let mut headers = HeaderMap::new();
        let mut auth = HeaderValue::from_str(&format!("Bearer {}", token.expose()))
            .map_err(|_| EsiError::InvalidInput("the token isn't a valid header".into()))?;
        auth.set_sensitive(true);
        headers.insert(AUTHORIZATION, auth);
        // User-Agent and X-Compatibility-Date come from the shared state's
        // default headers, added to every request.
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
            shared.inner().without_cache(),
        ))
    }

    /// The corporation's Upwell structures' ids, through the same client
    /// and cache (Structures reads the list just before), or none if the
    /// token can't read them (no scope or Station Manager role).
    async fn upwell_ids(&self, client: &Client, corporation: i64) -> Option<Vec<i64>> {
        let mut ids = Vec::new();
        let mut page = 1u32;
        loop {
            let response = self
                .call_full(
                    Priority::Bulk,
                    client
                        .get_corporations_corporation_id_structures()
                        .corporation_id(corporation)
                        .page(page)
                        .send(),
                )
                .await
                .ok()?;
            let last = pages(response.headers());
            ids.extend(response.into_inner().iter().map(|s| s.structure_id));
            if page >= last || page >= 50 {
                return Some(ids);
            }
            page += 1;
        }
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

    /// The shared client without its cache, for public endpoints whose ids
    /// plugins choose: cached, every killmail anyone asked about would stay
    /// in the cache until it expires. Same HTTP client, base URL, rate and
    /// error limiters (so backoff is shared) and default headers; calls
    /// still go through `call_full`'s budget.
    fn uncached(&self) -> Client {
        let shared = self.client();
        Client::new_with_client(
            shared.baseurl(),
            shared.client().clone(),
            shared.inner().without_cache(),
        )
    }

    /// Calls a public catalogue endpoint ([`About::Public`]), without a
    /// token. `params` are checked here.
    pub async fn plugin_get_public(
        &self,
        endpoint: &Endpoint,
        params: &[(String, String)],
    ) -> Result<Response, EsiError> {
        let param = |name: &str| {
            params
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.as_str())
        };
        let positive = |name: &str| -> Result<i64, EsiError> {
            param(name)
                .and_then(|v| v.parse::<i64>().ok())
                .filter(|id| *id > 0)
                .ok_or_else(|| EsiError::InvalidInput(format!("{name} must be a number")))
        };
        match endpoint.name {
            "killmail" => {
                let id: i64 = param("killmail_id")
                    .and_then(|v| v.parse().ok())
                    .filter(|id| *id > 0)
                    .ok_or_else(|| EsiError::InvalidInput("killmail_id must be a number".into()))?;
                let hash = param("killmail_hash")
                    .filter(|h| h.len() == 40 && h.bytes().all(|b| b.is_ascii_hexdigit()))
                    .ok_or_else(|| {
                        EsiError::InvalidInput("killmail_hash must be 40 hex digits".into())
                    })?
                    .to_ascii_lowercase();
                let client = self.uncached();
                let killmail = self
                    .call_full(
                        Priority::Bulk,
                        client
                            .get_killmails_killmail_id_killmail_hash()
                            .killmail_id(id)
                            .killmail_hash(hash)
                            .send(),
                    )
                    .await?
                    .into_inner();
                let victim = &killmail.victim;
                Ok(Response {
                    body: serde_json::json!({
                        "killmail_id": killmail.killmail_id,
                        "killmail_time": killmail.killmail_time,
                        "solar_system_id": killmail.solar_system_id,
                        "victim": {
                            "character_id": victim.character_id,
                            "corporation_id": victim.corporation_id,
                            "alliance_id": victim.alliance_id,
                            "ship_type_id": victim.ship_type_id,
                        },
                        "attackers": killmail.attackers.len(),
                    }),
                    pages: 1,
                })
            }
            "universe-moon" => {
                let id = positive("moon_id")?;
                let moon = self
                    .call_full(
                        Priority::Bulk,
                        self.uncached()
                            .get_universe_moons_moon_id()
                            .moon_id(id)
                            .send(),
                    )
                    .await?
                    .into_inner();
                Ok(Response {
                    body: json(&moon)?,
                    pages: 1,
                })
            }
            "universe-planet" => {
                let id = positive("planet_id")?;
                let planet = self
                    .call_full(
                        Priority::Bulk,
                        self.uncached()
                            .get_universe_planets_planet_id()
                            .planet_id(id)
                            .send(),
                    )
                    .await?
                    .into_inner();
                Ok(Response {
                    body: serde_json::json!({
                        "planet_id": planet.planet_id,
                        "name": planet.name,
                        "system_id": planet.system_id,
                        "type_id": planet.type_id,
                        "position": {
                            "x": planet.position.x,
                            "y": planet.position.y,
                            "z": planet.position.z,
                        },
                    }),
                    pages: 1,
                })
            }
            "sovereignty-systems" => {
                let systems = self
                    .call_full(
                        Priority::Bulk,
                        self.uncached().get_sovereignty_systems().send(),
                    )
                    .await?
                    .into_inner();
                // Only which alliance holds which system.
                let systems = json(&systems)?;
                let claimed: Vec<serde_json::Value> = systems["solar_systems"]
                    .as_array()
                    .map(|all| {
                        all.iter()
                            .filter_map(|s| {
                                let alliance = s["claim"]["alliance"]["alliance_id"].as_i64()?;
                                let system = s["solar_system_id"].as_i64()?;
                                Some(serde_json::json!({
                                    "system_id": system,
                                    "alliance_id": alliance,
                                }))
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                Ok(Response {
                    body: serde_json::Value::Array(claimed),
                    pages: 1,
                })
            }
            other => Err(EsiError::InvalidInput(format!(
                "no public endpoint {other}"
            ))),
        }
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
            "corporation-starbases" => paged!(
                client
                    .get_corporations_corporation_id_starbases()
                    .corporation_id(corporation)
            ),
            "corporation-starbase" => {
                let starbase = self
                    .call_full(
                        priority,
                        client
                            .get_corporations_corporation_id_starbases_starbase_id()
                            .corporation_id(corporation)
                            .starbase_id(id("starbase_id")?)
                            .system_id(id("system_id")?)
                            .send(),
                    )
                    .await?
                    .into_inner();
                let fuels: Vec<serde_json::Value> = starbase
                    .fuels
                    .iter()
                    .map(|f| serde_json::json!({ "type_id": f.type_id, "quantity": f.quantity }))
                    .collect();
                Ok(Response {
                    body: serde_json::json!({ "fuels": fuels }),
                    pages: 1,
                })
            }
            "corporation-customs-offices" => paged!(
                client
                    .get_corporations_corporation_id_customs_offices()
                    .corporation_id(corporation)
            ),
            "corporation-structure-assets" => {
                let page = page.map_or(1, std::num::NonZeroU32::get);
                let request = client
                    .get_corporations_corporation_id_assets()
                    .corporation_id(corporation)
                    .page(page)
                    .send();
                // `location_flag` is read as text: a flag CCP adds after
                // this client was generated fails the typed read of the
                // whole page. That page is then fetched again as it is
                // (headers included, for the page count) and read loosely.
                let raw = client.client().clone();
                let url = format!("{}/corporations/{corporation}/assets", client.baseurl());
                let request = async move {
                    match request.await {
                        Ok(response) => {
                            let (status, headers) = (response.status(), response.headers().clone());
                            let items = response
                                .into_inner()
                                .iter()
                                .map(|a| Asset {
                                    item_id: a.item_id,
                                    type_id: a.type_id,
                                    location_id: a.location_id,
                                    location_flag: a.location_flag.to_string(),
                                    location_type: a.location_type.to_string(),
                                    quantity: a.quantity,
                                })
                                .collect::<Vec<_>>();
                            Ok(ResponseValue::new(items, status, headers))
                        }
                        Err(eve_esi_client::Error::InvalidResponsePayload(bytes, err)) => {
                            let fallback =
                                eve_esi_client::Error::InvalidResponsePayload(bytes, err);
                            let Ok(response) = raw.get(&url).query(&[("page", page)]).send().await
                            else {
                                return Err(fallback);
                            };
                            let (status, headers) = (response.status(), response.headers().clone());
                            if !status.is_success() {
                                // Its status and error-limit headers reach
                                // the budget.
                                return Err(eve_esi_client::Error::UnexpectedResponse(response));
                            }
                            let Ok(bytes) = response.bytes().await else {
                                return Err(fallback);
                            };
                            match serde_json::from_slice::<Vec<Asset>>(&bytes) {
                                Ok(items) => Ok(ResponseValue::new(items, status, headers)),
                                Err(_) => Err(fallback),
                            }
                        }
                        Err(other) => Err(other),
                    }
                };
                let response = self.call_full(priority, request).await?;
                let pages = pages(response.headers());
                // Slots and bays ships share pass only for the
                // corporation's own Upwell structures: never its ships'
                // fittings (nor their item ids, which asset names and
                // locations would take).
                let assets = response.into_inner();
                let upwell = if assets.iter().any(Asset::in_shared_slot) {
                    self.upwell_ids(&client, corporation).await
                } else {
                    None
                };
                let items: Vec<serde_json::Value> = assets
                    .into_iter()
                    .filter(|a| a.about_structures(upwell.as_deref()))
                    .map(|a| {
                        serde_json::json!({
                            "item_id": a.item_id,
                            "type_id": a.type_id,
                            "location_id": a.location_id,
                            "location_flag": a.location_flag,
                            "location_type": a.location_type,
                            "quantity": a.quantity,
                        })
                    })
                    .collect();
                Ok(Response {
                    body: serde_json::Value::Array(items),
                    pages,
                })
            }
            "corporation-asset-locations" => {
                let ids = item_ids(params)?;
                let locations = self
                    .call_full(
                        priority,
                        client
                            .post_corporations_corporation_id_assets_locations()
                            .corporation_id(corporation)
                            .body(ids)
                            .send(),
                    )
                    .await?
                    .into_inner();
                let locations: Vec<serde_json::Value> = locations
                    .iter()
                    .map(|l| {
                        serde_json::json!({
                            "item_id": l.item_id,
                            "position": { "x": l.position.x, "y": l.position.y, "z": l.position.z },
                        })
                    })
                    .collect();
                Ok(Response {
                    body: serde_json::Value::Array(locations),
                    pages: 1,
                })
            }
            "corporation-asset-names" => {
                let ids = item_ids(params)?;
                let names = self
                    .call_full(
                        priority,
                        client
                            .post_corporations_corporation_id_assets_names()
                            .corporation_id(corporation)
                            .body(ids)
                            .send(),
                    )
                    .await?
                    .into_inner();
                let names: Vec<serde_json::Value> = names
                    .iter()
                    .map(|n| serde_json::json!({ "item_id": n.item_id, "name": n.name }))
                    .collect();
                Ok(Response {
                    body: serde_json::Value::Array(names),
                    pages: 1,
                })
            }
            "corporation-structure-notifications" => {
                let request = client
                    .get_characters_character_id_notifications()
                    .character_id(character)
                    .send();
                // `type` is read as text: a type CCP adds after this client
                // was generated fails the typed read (and with it every
                // notification), so the body is read again loosely.
                let request = async move {
                    match request.await {
                        Ok(response) => {
                            let (status, headers) = (response.status(), response.headers().clone());
                            let items = response
                                .into_inner()
                                .iter()
                                .map(|n| Notification {
                                    notification_id: n.notification_id,
                                    kind: n.type_.to_string(),
                                    timestamp: n.timestamp.to_rfc3339(),
                                    text: n.text.clone(),
                                })
                                .collect::<Vec<_>>();
                            Ok(ResponseValue::new(items, status, headers))
                        }
                        Err(eve_esi_client::Error::InvalidResponsePayload(bytes, err)) => {
                            match serde_json::from_slice::<Vec<Notification>>(&bytes) {
                                Ok(items) => Ok(ResponseValue::new(
                                    items,
                                    reqwest::StatusCode::OK,
                                    HeaderMap::new(),
                                )),
                                Err(_) => {
                                    Err(eve_esi_client::Error::InvalidResponsePayload(bytes, err))
                                }
                            }
                        }
                        Err(other) => Err(other),
                    }
                };
                let response = self.call_full(priority, request).await?;
                // Only structure notifications, and only what's needed of
                // them (not the sender or whether it was read).
                let notifications: Vec<serde_json::Value> = response
                    .into_inner()
                    .into_iter()
                    .filter(|n| STRUCTURE_NOTIFICATIONS.contains(&n.kind.as_str()))
                    .map(|n| {
                        serde_json::json!({
                            "notification_id": n.notification_id,
                            "type": n.kind,
                            "timestamp": n.timestamp,
                            "text": n.text,
                        })
                    })
                    .collect();
                Ok(Response {
                    body: serde_json::Value::Array(notifications),
                    pages: 1,
                })
            }
            "universe-system" => {
                let system = self
                    .call_full(
                        priority,
                        client
                            .get_universe_systems_system_id()
                            .system_id(id("system_id")?)
                            .send(),
                    )
                    .await?
                    .into_inner();
                let constellation = self
                    .call_full(
                        priority,
                        client
                            .get_universe_constellations_constellation_id()
                            .constellation_id(system.constellation_id)
                            .send(),
                    )
                    .await?
                    .into_inner();
                Ok(Response {
                    body: serde_json::json!({
                        "system_id": system.system_id,
                        "name": system.name,
                        "security_status": system.security_status,
                        "constellation_id": system.constellation_id,
                        "region_id": constellation.region_id,
                        "planets": system.planets.iter().map(|p| p.planet_id).collect::<Vec<_>>(),
                    }),
                    pages: 1,
                })
            }
            "fleet-members" => {
                let fleet = match self
                    .call_full(
                        priority,
                        client
                            .get_characters_character_id_fleet()
                            .character_id(character)
                            .send(),
                    )
                    .await
                {
                    Ok(fleet) => fleet.into_inner(),
                    // Not in a fleet.
                    Err(EsiError::Status(404)) => {
                        return Ok(Response {
                            body: serde_json::json!({ "in_fleet": false, "boss": false }),
                            pages: 1,
                        });
                    }
                    Err(err) => return Err(err),
                };
                if fleet.fleet_boss_id != character {
                    return Ok(Response {
                        body: serde_json::json!({ "in_fleet": true, "boss": false }),
                        pages: 1,
                    });
                }
                let members = self
                    .call_full(
                        priority,
                        client
                            .get_fleets_fleet_id_members()
                            .fleet_id(fleet.fleet_id)
                            .send(),
                    )
                    .await?
                    .into_inner();
                // Who, in what, where, since when: what a FAT records.
                let members: Vec<serde_json::Value> = members
                    .iter()
                    .map(|m| {
                        serde_json::json!({
                            "character_id": m.character_id,
                            "ship_type_id": m.ship_type_id,
                            "solar_system_id": m.solar_system_id,
                            "join_time": m.join_time,
                        })
                    })
                    .collect();
                Ok(Response {
                    body: serde_json::json!({
                        "in_fleet": true,
                        "boss": true,
                        "fleet_id": fleet.fleet_id,
                        "members": members,
                    }),
                    pages: 1,
                })
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use eve_esi_client::types::CharactersCharacterIdNotificationsGetItemType as Kind;

    #[test]
    fn structure_notifications_are_esi_types() {
        for name in STRUCTURE_NOTIFICATIONS {
            let kind: Kind = name.parse().unwrap();
            // The filter compares the type's text: it must round-trip.
            assert_eq!(kind.to_string(), *name);
        }
    }

    fn asset(flag: &str, type_id: i64, location_type: &str) -> Asset {
        Asset {
            item_id: 1,
            type_id,
            location_id: 2,
            location_flag: flag.to_owned(),
            location_type: location_type.to_owned(),
            quantity: 1,
        }
    }

    #[test]
    fn structure_assets_are_slots_bays_and_skyhooks_only() {
        // The asset's location is item 2.
        let upwell: &[i64] = &[2];
        for flag in [
            "ServiceSlot4",
            "StructureFuel",
            "QuantumCoreRoom",
            "MoonMaterialBay",
        ] {
            assert!(asset(flag, 34, "item").about_structures(None), "{flag}");
        }
        // Ships have these too: only in the corporation's structures.
        for flag in [
            "HiSlot0",
            "MedSlot7",
            "LoSlot3",
            "RigSlot2",
            "FighterTube1",
            "FighterBay",
        ] {
            assert!(
                asset(flag, 34, "item").about_structures(Some(upwell)),
                "{flag}"
            );
            assert!(
                !asset(flag, 34, "item").about_structures(Some(&[3])),
                "{flag}"
            );
            assert!(!asset(flag, 34, "item").about_structures(None), "{flag}");
        }
        for flag in [
            "Hangar",
            "CorpSAG1",
            "Cargo",
            "CorpDeliveries",
            "OfficeFolder",
            "HiSlot",
            "HiSlot10",
            "HiSlotX",
            "AutoFit",
        ] {
            assert!(
                !asset(flag, 34, "item").about_structures(Some(upwell)),
                "{flag}"
            );
        }
        assert!(asset("AutoFit", 81080, "solar_system").about_structures(None));
        // A skyhook in a hangar (packaged, say) isn't one in space.
        assert!(!asset("Hangar", 81080, "item").about_structures(None));
    }

    #[test]
    fn item_ids_are_a_short_list_of_positive_ids() {
        let p = |v: &str| vec![("item_ids".to_owned(), v.to_owned())];
        assert_eq!(item_ids(&p("3, 1,3")).unwrap(), vec![1, 3]);
        assert!(item_ids(&p("")).is_err());
        assert!(item_ids(&p("1,-2")).is_err());
        assert!(item_ids(&p("1,x")).is_err());
        assert!(item_ids(&[]).is_err());
        let many = (1..=1001)
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join(",");
        assert!(item_ids(&p(&many)).is_err());
    }

    #[test]
    fn endpoint_names_are_unique() {
        for (i, e) in ENDPOINTS.iter().enumerate() {
            assert!(
                ENDPOINTS[i + 1..].iter().all(|o| o.name != e.name),
                "{}",
                e.name
            );
        }
    }
}
