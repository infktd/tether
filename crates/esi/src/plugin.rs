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
const WALLET: &str = "esi-wallet.read_character_wallet.v1";
const CONTRACTS: &str = "esi-contracts.read_character_contracts.v1";
const MAIL: &str = "esi-mail.read_mail.v1";
const PLANETS: &str = "esi-planets.manage_planets.v1";
const CALENDAR: &str = "esi-calendar.read_calendar_events.v1";

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
        scope: WALLET,
        about: About::Character,
        paged: false,
        params: &[],
    },
    Endpoint {
        name: "character-wallet-journal",
        scope: WALLET,
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
    // A full character viewer (SeAT's and aa-memberaudit's): everything
    // below reads the one character the plugin names, with its own token,
    // and passes ESI's JSON through, unless it says otherwise. Optional
    // ids are noted; `params` lists the ones a call must give.
    Endpoint {
        // Optional `from_id`: transactions before that one (ESI's own
        // stepping back; it has no pages).
        name: "character-wallet-transactions",
        scope: WALLET,
        about: About::Character,
        paged: false,
        params: &[],
    },
    Endpoint {
        name: "character-contracts",
        scope: CONTRACTS,
        about: About::Character,
        paged: true,
        params: &[],
    },
    Endpoint {
        // One of the character's own contracts' items (ESI answers only
        // for a contract the character is party to).
        name: "character-contract-items",
        scope: CONTRACTS,
        about: About::Character,
        paged: false,
        params: &["contract_id"],
    },
    Endpoint {
        name: "character-contacts",
        scope: "esi-characters.read_contacts.v1",
        about: About::Character,
        paged: true,
        params: &[],
    },
    Endpoint {
        name: "character-standings",
        scope: "esi-characters.read_standings.v1",
        about: About::Character,
        paged: false,
        params: &[],
    },
    Endpoint {
        // Mail headers (subject, sender, recipients, labels, read), newest
        // 50; optional `last_mail_id` for the 50 before it. No bodies.
        name: "character-mail",
        scope: MAIL,
        about: About::Character,
        paged: false,
        params: &[],
    },
    Endpoint {
        // One mail's body, by the id its header gave: one mail of one
        // character per call, only the one asked for.
        name: "character-mail-body",
        scope: MAIL,
        about: About::Character,
        paged: false,
        params: &["mail_id"],
    },
    Endpoint {
        name: "character-mail-labels",
        scope: MAIL,
        about: About::Character,
        paged: false,
        params: &[],
    },
    Endpoint {
        name: "character-mailing-lists",
        scope: MAIL,
        about: About::Character,
        paged: false,
        params: &[],
    },
    Endpoint {
        name: "character-loyalty-points",
        scope: "esi-characters.read_loyalty.v1",
        about: About::Character,
        paged: false,
        params: &[],
    },
    Endpoint {
        // Planetary interaction colonies.
        name: "character-planets",
        scope: PLANETS,
        about: About::Character,
        paged: false,
        params: &[],
    },
    Endpoint {
        // One colony's layout (pins, links, routes), by `planet_id`.
        name: "character-planet",
        scope: PLANETS,
        about: About::Character,
        paged: false,
        params: &["planet_id"],
    },
    Endpoint {
        // Running jobs and those finished in the last 90 days.
        name: "character-industry-jobs",
        scope: "esi-industry.read_character_jobs.v1",
        about: About::Character,
        paged: false,
        params: &[],
    },
    Endpoint {
        name: "character-blueprints",
        scope: "esi-characters.read_blueprints.v1",
        about: About::Character,
        paged: true,
        params: &[],
    },
    Endpoint {
        // Open market orders.
        name: "character-orders",
        scope: "esi-markets.read_character_orders.v1",
        about: About::Character,
        paged: false,
        params: &[],
    },
    Endpoint {
        // Recent kills and losses, as ids and hashes: the killmails
        // themselves are public (`killmail-detail`).
        name: "character-killmails",
        scope: "esi-killmails.read_killmails.v1",
        about: About::Character,
        paged: true,
        params: &[],
    },
    Endpoint {
        // Public: one whole killmail (victim, its items, attackers), by id
        // and hash. `killmail` is the short form.
        name: "killmail-detail",
        scope: "",
        about: About::Public,
        paged: false,
        params: &["killmail_id", "killmail_hash"],
    },
    Endpoint {
        // Public: any character's corporations, by `character_id`.
        name: "character-corporation-history",
        scope: "",
        about: About::Public,
        paged: false,
        params: &["character_id"],
    },
    Endpoint {
        name: "character-attributes",
        scope: "esi-skills.read_skills.v1",
        about: About::Character,
        paged: false,
        params: &[],
    },
    Endpoint {
        // Jump fatigue.
        name: "character-fatigue",
        scope: "esi-characters.read_fatigue.v1",
        about: About::Character,
        paged: false,
        params: &[],
    },
    Endpoint {
        // The character's own corporation roles.
        name: "character-roles",
        scope: "esi-characters.read_corporation_roles.v1",
        about: About::Character,
        paged: false,
        params: &[],
    },
    Endpoint {
        name: "character-titles",
        scope: "esi-characters.read_titles.v1",
        about: About::Character,
        paged: false,
        params: &[],
    },
    Endpoint {
        // Every notification of the character's (the last 500 or 30
        // days), unlike `corporation-structure-notifications`, which
        // trims a data source's to its corporation's structures.
        name: "character-notifications",
        scope: "esi-characters.read_notifications.v1",
        about: About::Character,
        paged: false,
        params: &[],
    },
    Endpoint {
        // The next 50 calendar events; optional `from_event` for the 50
        // after it.
        name: "character-calendar",
        scope: CALENDAR,
        about: About::Character,
        paged: false,
        params: &[],
    },
    Endpoint {
        // One event of the character's calendar, by `event_id`.
        name: "character-calendar-event",
        scope: CALENDAR,
        about: About::Character,
        paged: false,
        params: &["event_id"],
    },
    Endpoint {
        name: "character-fittings",
        scope: "esi-fittings.read_fittings.v1",
        about: About::Character,
        paged: false,
        params: &[],
    },
    Endpoint {
        // The personal mining ledger (the last 30 days).
        name: "character-mining",
        scope: "esi-industry.read_character_mining.v1",
        about: About::Character,
        paged: true,
        params: &[],
    },
    Endpoint {
        // An Upwell structure's name, system and type, by `structure_id`,
        // as the character sees it (ESI answers only for structures the
        // character may dock at): where a clone or an asset is, instead
        // of a raw id. Not its owner or position.
        name: "universe-structure",
        scope: "esi-universe.read_structures.v1",
        about: About::Character,
        paged: false,
        params: &["structure_id"],
    },
    Endpoint {
        // Public: a character's public sheet, by `character_id`: birthday,
        // security status, bloodline, race, faction, gender and bio (what
        // anyone sees in game). Not its corporation's roles or anything a
        // token reads.
        name: "character-public",
        scope: "",
        about: About::Public,
        paged: false,
        params: &["character_id"],
    },
    Endpoint {
        // Public: an item category (16 is skills) and its groups, by
        // `category_id`.
        name: "universe-category",
        scope: "",
        about: About::Public,
        paged: false,
        params: &["category_id"],
    },
    Endpoint {
        // Public: an item group (a skill group, say), its name and its
        // types, by `group_id`.
        name: "universe-group",
        scope: "",
        about: About::Public,
        paged: false,
        params: &["group_id"],
    },
    Endpoint {
        // Public: an NPC station, by `station_id`.
        name: "universe-station",
        scope: "",
        about: About::Public,
        paged: false,
        params: &["station_id"],
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
    /// ESI requests made beyond what the endpoint's cost says: an answer
    /// read a second time, raw, because its typed read failed. The host
    /// counts them against the plugin's ESI budget.
    pub refetched: u32,
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

/// An optional positive id a plugin may give (`from_id` and the like).
fn positive_id(params: &[(String, String)], name: &str) -> Result<Option<i64>, EsiError> {
    match params.iter().find(|(k, _)| k == name) {
        None => Ok(None),
        Some((_, v)) => v
            .parse::<i64>()
            .ok()
            .filter(|id| *id > 0)
            .map(Some)
            .ok_or_else(|| EsiError::InvalidInput(format!("{name} must be a number"))),
    }
}

/// A boxed future, built in the function that returns it.
type Pending<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;

/// The request `send` makes, through [`Esi::call_full`], built and boxed
/// here. A function that awaits many different requests (a match of
/// endpoints) then holds only a request builder and a pointer for each in
/// its stack frame, not the whole request: a debug build gives every one
/// its own stack slot, and together they overflowed a thread's stack.
fn fetch<'a, T, E, F, S>(esi: &'a Esi, send: S) -> Pending<'a, Result<ResponseValue<T>, EsiError>>
where
    T: Send + 'a,
    E: std::fmt::Debug + Send + 'a,
    F: std::future::Future<Output = Result<ResponseValue<T>, eve_esi_client::Error<E>>> + Send + 'a,
    S: FnOnce() -> F + Send + 'a,
{
    Box::pin(async move { esi.call_full(Priority::Bulk, send()).await })
}

/// The answer as it is, with its page count (see [`fetch`]).
fn typed<'a, T, E, F, S>(esi: &'a Esi, send: S) -> Pending<'a, Result<Response, EsiError>>
where
    T: serde::Serialize + Send + 'a,
    E: std::fmt::Debug + Send + 'a,
    F: std::future::Future<Output = Result<ResponseValue<T>, eve_esi_client::Error<E>>> + Send + 'a,
    S: FnOnce() -> F + Send + 'a,
{
    Box::pin(async move {
        let response = fetch(esi, send).await?;
        let pages = pages(response.headers());
        Ok(Response {
            body: json(&response.into_inner())?,
            pages,
            refetched: 0,
        })
    })
}

/// As [`typed`], read loosely (see [`loose`]).
fn loosely<'a, T, E, F, S>(
    esi: &'a Esi,
    send: S,
    again: Again,
) -> Pending<'a, Result<Response, EsiError>>
where
    T: serde::Serialize + Send + 'a,
    E: std::fmt::Debug + Send + 'a,
    F: std::future::Future<Output = Result<ResponseValue<T>, eve_esi_client::Error<E>>> + Send + 'a,
    S: FnOnce() -> F + Send + 'a,
{
    Box::pin(async move {
        let response = fetch(esi, move || loose(send(), again)).await?;
        Ok(Response {
            pages: pages(response.headers()),
            refetched: refetched(response.headers()),
            body: response.into_inner(),
        })
    })
}

/// Where [`loose`] reads an answer again: the client the typed request
/// went through (its HTTP client, with the token if there is one, its
/// base URL and error limiter), the endpoint's path and its query (the
/// page, `last_mail_id` and the like).
struct Again {
    client: Client,
    path: String,
    query: Vec<(&'static str, String)>,
}

impl Again {
    fn new(client: &Client, path: String) -> Self {
        Self {
            client: client.clone(),
            path,
            query: Vec::new(),
        }
    }

    fn with(mut self, name: &'static str, value: impl ToString) -> Self {
        self.query.push((name, value.to_string()));
        self
    }
}

/// Set on the headers of an answer read a second time, raw (see
/// [`read_again`]): that's one more ESI request, which the plugin's
/// budget counts ([`Response::refetched`]). Never sent anywhere.
const REFETCHED: &str = "x-tether-refetched";

/// Reads `again` as it is, when a typed read failed (`fallback`). Raw
/// requests skip eve-esi-client's hooks, so what they add is added here:
/// the compatibility date, and backing off while the error budget is low
/// (the read is skipped then, and the typed read's failure stands). The
/// status and error-limit headers reach Tether's budget through
/// `Esi::call_full`; a failure is ESI's status, never a body passed on as
/// data.
// The library's own error: `Esi::call_full` reads the budget from it.
#[allow(clippy::result_large_err)]
async fn read_again<T, E>(
    again: Again,
    fallback: eve_esi_client::Error<E>,
) -> Result<ResponseValue<T>, eve_esi_client::Error<E>>
where
    T: serde::de::DeserializeOwned,
{
    if crate::budget::bulk_delay(again.client.error_budget()).is_some() {
        tracing::warn!("ESI error budget low; not reading an answer again");
        return Err(fallback);
    }
    let url = format!("{}{}", again.client.baseurl(), again.path);
    let Ok(response) = again
        .client
        .client()
        .get(&url)
        .header("X-Compatibility-Date", eve_esi_client::COMPATIBILITY_DATE)
        .query(&again.query)
        .send()
        .await
    else {
        return Err(fallback);
    };
    let (status, mut headers) = (response.status(), response.headers().clone());
    if !status.is_success() {
        return Err(eve_esi_client::Error::UnexpectedResponse(response));
    }
    let Ok(bytes) = response.bytes().await else {
        return Err(fallback);
    };
    let Ok(body) = serde_json::from_slice::<T>(&bytes) else {
        return Err(fallback);
    };
    headers.insert(REFETCHED, HeaderValue::from_static("1"));
    Ok(ResponseValue::new(body, status, headers))
}

/// A typed request's answer, as JSON. ESI's enums (location flags, roles,
/// notification and contract types) are closed in this client, so a value
/// CCP adds later fails the typed read of the whole response: then it is
/// read again as it is (`again`), as plain JSON. Read again, not from the
/// bytes the typed read failed on: the client reports an error answer
/// whose body doesn't read the same way, without its status, and the page
/// count is in the headers it dropped.
// The library's own error: `Esi::call_full` reads the budget from it.
#[allow(clippy::result_large_err)]
async fn loose<T, E, F>(
    request: F,
    again: Again,
) -> Result<ResponseValue<serde_json::Value>, eve_esi_client::Error<E>>
where
    T: serde::Serialize,
    F: std::future::Future<Output = Result<ResponseValue<T>, eve_esi_client::Error<E>>>,
{
    match request.await {
        Ok(response) => {
            let (status, headers) = (response.status(), response.headers().clone());
            serde_json::to_value(response.into_inner())
                .map(|body| ResponseValue::new(body, status, headers))
                .map_err(|e| eve_esi_client::Error::Custom(e.to_string()))
        }
        Err(eve_esi_client::Error::InvalidResponsePayload(bytes, err)) => {
            read_again(
                again,
                eve_esi_client::Error::InvalidResponsePayload(bytes, err),
            )
            .await
        }
        Err(other) => Err(other),
    }
}

/// Whether an answer was read a second time (see [`REFETCHED`]).
fn refetched(headers: &HeaderMap) -> u32 {
    u32::from(headers.contains_key(REFETCHED))
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
                    refetched: 0,
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
                    refetched: 0,
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
                    refetched: 0,
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
                    refetched: 0,
                })
            }
            "killmail-detail" => {
                let id = positive("killmail_id")?;
                let hash = param("killmail_hash")
                    .filter(|h| h.len() == 40 && h.bytes().all(|b| b.is_ascii_hexdigit()))
                    .ok_or_else(|| {
                        EsiError::InvalidInput("killmail_hash must be 40 hex digits".into())
                    })?
                    .to_ascii_lowercase();
                let killmail = self
                    .call_full(
                        Priority::Bulk,
                        self.uncached()
                            .get_killmails_killmail_id_killmail_hash()
                            .killmail_id(id)
                            .killmail_hash(hash)
                            .send(),
                    )
                    .await?
                    .into_inner();
                Ok(Response {
                    body: json(&killmail)?,
                    pages: 1,
                    refetched: 0,
                })
            }
            "character-corporation-history" => {
                let id = positive("character_id")?;
                let history = self
                    .call_full(
                        Priority::Bulk,
                        self.uncached()
                            .get_characters_character_id_corporationhistory()
                            .character_id(id)
                            .send(),
                    )
                    .await?
                    .into_inner();
                Ok(Response {
                    body: json(&history)?,
                    pages: 1,
                    refetched: 0,
                })
            }
            "character-public" => {
                let id = positive("character_id")?;
                let detail = self
                    .call_full(
                        Priority::Bulk,
                        self.uncached()
                            .get_characters_detail()
                            .character_id(id)
                            .send(),
                    )
                    .await?
                    .into_inner();
                Ok(Response {
                    body: serde_json::json!({
                        "character_id": id,
                        "name": detail.name,
                        "birthday": detail.birthday,
                        "security_status": detail.security_status,
                        "bloodline_id": detail.bloodline_id,
                        "race_id": detail.race_id,
                        "faction_id": detail.faction_id,
                        "gender": detail.gender,
                        "description": detail.description,
                    }),
                    pages: 1,
                    refetched: 0,
                })
            }
            "universe-category" => {
                let id = positive("category_id")?;
                let category = self
                    .call_full(
                        Priority::Bulk,
                        self.uncached()
                            .get_universe_categories_category_id()
                            .category_id(id)
                            .send(),
                    )
                    .await?
                    .into_inner();
                Ok(Response {
                    body: json(&category)?,
                    pages: 1,
                    refetched: 0,
                })
            }
            "universe-group" => {
                let id = positive("group_id")?;
                let group = self
                    .call_full(
                        Priority::Bulk,
                        self.uncached()
                            .get_universe_groups_group_id()
                            .group_id(id)
                            .send(),
                    )
                    .await?
                    .into_inner();
                Ok(Response {
                    body: json(&group)?,
                    pages: 1,
                    refetched: 0,
                })
            }
            "universe-station" => {
                let id = positive("station_id")?;
                let client = self.uncached();
                let again = Again::new(&client, format!("/universe/stations/{id}"));
                let request = client.get_universe_stations_station_id().station_id(id);
                loosely(self, move || request.send(), again).await
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
        // The request is built and run in `typed`, off this frame (see
        // `fetch`).
        macro_rules! get {
            ($request:expr) => {{
                let request = $request;
                typed(self, move || request.send()).await
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
                    refetched: 0,
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
                    refetched: 0,
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
                let again = Again::new(&client, format!("/corporations/{corporation}/assets"))
                    .with("page", page);
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
                            read_again::<Vec<Asset>, _>(
                                again,
                                eve_esi_client::Error::InvalidResponsePayload(bytes, err),
                            )
                            .await
                        }
                        Err(other) => Err(other),
                    }
                };
                let response = self.call_full(priority, request).await?;
                let pages = pages(response.headers());
                let again = refetched(response.headers());
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
                    refetched: again,
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
                    refetched: 0,
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
                    refetched: 0,
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
                    refetched: 0,
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
                    refetched: 0,
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
                            refetched: 0,
                        });
                    }
                    Err(err) => return Err(err),
                };
                if fleet.fleet_boss_id != character {
                    return Ok(Response {
                        body: serde_json::json!({ "in_fleet": true, "boss": false }),
                        pages: 1,
                        refetched: 0,
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
                    refetched: 0,
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
            // The character viewer's entries, in a function of their own:
            // one match this long makes too deep a stack frame in debug
            // builds.
            _ => Box::pin(self.character_viewer(endpoint, &client, character, params, page)).await,
        }
    }

    /// [`Esi::plugin_get`] for the character viewer's entries (a full
    /// character's data, as SeAT and aa-memberaudit show it): each reads
    /// `character`, with its own token in `client`.
    async fn character_viewer(
        &self,
        endpoint: &Endpoint,
        client: &Client,
        character: i64,
        params: &[(String, String)],
        page: Option<std::num::NonZeroU32>,
    ) -> Result<Response, EsiError> {
        // Ids are positive: anything else is a certain ESI error, which
        // would spend the error budget Tether shares.
        let id = |name: &str| -> Result<i64, EsiError> {
            positive_id(params, name)?
                .ok_or_else(|| EsiError::InvalidInput(format!("{name} must be a number")))
        };
        // Each request is built and run in `typed` or `loosely`, off this
        // frame (see `fetch`).
        macro_rules! get {
            ($request:expr) => {{
                let request = $request;
                typed(self, move || request.send()).await
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
        // As `get!`, read loosely (see `loose`): for responses with enums.
        // `$again` is the same request, for reading it again as it is.
        macro_rules! loose {
            ($request:expr, $again:expr) => {{
                let request = $request;
                loosely(self, move || request.send(), $again).await
            }};
        }
        // As `paged!`, read loosely; `$path` is the endpoint's.
        macro_rules! loose_paged {
            ($request:expr, $path:expr) => {{
                let p = page.map_or(1, std::num::NonZeroU32::get);
                let again = Again::new(client, $path).with("page", p);
                loose!($request.page(p), again)
            }};
        }
        let again = |path: String| Again::new(client, path);
        match endpoint.name {
            "character-wallet-transactions" => {
                let request = client
                    .get_characters_character_id_wallet_transactions()
                    .character_id(character);
                match positive_id(params, "from_id")? {
                    Some(from) => get!(request.from_id(from)),
                    None => get!(request),
                }
            }
            "character-contracts" => loose_paged!(
                client
                    .get_characters_character_id_contracts()
                    .character_id(character),
                format!("/characters/{character}/contracts")
            ),
            "character-contract-items" => get!(
                client
                    .get_characters_character_id_contracts_contract_id_items()
                    .character_id(character)
                    .contract_id(id("contract_id")?)
            ),
            "character-contacts" => loose_paged!(
                client
                    .get_characters_character_id_contacts()
                    .character_id(character),
                format!("/characters/{character}/contacts")
            ),
            "character-standings" => loose!(
                client
                    .get_characters_character_id_standings()
                    .character_id(character),
                again(format!("/characters/{character}/standings"))
            ),
            "character-mail" => {
                let request = client
                    .get_characters_character_id_mail()
                    .character_id(character);
                let path = format!("/characters/{character}/mail");
                match positive_id(params, "last_mail_id")? {
                    Some(last) => loose!(
                        request.last_mail_id(last),
                        again(path).with("last_mail_id", last)
                    ),
                    None => loose!(request, again(path)),
                }
            }
            "character-mail-body" => {
                let mail = id("mail_id")?;
                loose!(
                    client
                        .get_characters_character_id_mail_mail_id()
                        .character_id(character)
                        .mail_id(mail),
                    again(format!("/characters/{character}/mail/{mail}"))
                )
            }
            "character-mail-labels" => loose!(
                client
                    .get_characters_character_id_mail_labels()
                    .character_id(character),
                again(format!("/characters/{character}/mail/labels"))
            ),
            "character-mailing-lists" => get!(
                client
                    .get_characters_character_id_mail_lists()
                    .character_id(character)
            ),
            "character-loyalty-points" => get!(
                client
                    .get_characters_character_id_loyalty_points()
                    .character_id(character)
            ),
            "character-planets" => loose!(
                client
                    .get_characters_character_id_planets()
                    .character_id(character),
                again(format!("/characters/{character}/planets"))
            ),
            "character-planet" => get!(
                client
                    .get_characters_character_id_planets_planet_id()
                    .character_id(character)
                    .planet_id(id("planet_id")?)
            ),
            "character-industry-jobs" => loose!(
                client
                    .get_characters_character_id_industry_jobs()
                    .character_id(character)
                    .include_completed(true),
                again(format!("/characters/{character}/industry/jobs"))
                    .with("include_completed", true)
            ),
            "character-blueprints" => loose_paged!(
                client
                    .get_characters_character_id_blueprints()
                    .character_id(character),
                format!("/characters/{character}/blueprints")
            ),
            "character-orders" => loose!(
                client
                    .get_characters_character_id_orders()
                    .character_id(character),
                again(format!("/characters/{character}/orders"))
            ),
            "character-killmails" => paged!(
                client
                    .get_characters_character_id_killmails_recent()
                    .character_id(character)
            ),
            "character-attributes" => get!(
                client
                    .get_characters_character_id_attributes()
                    .character_id(character)
            ),
            "character-fatigue" => get!(
                client
                    .get_characters_character_id_fatigue()
                    .character_id(character)
            ),
            "character-roles" => loose!(
                client
                    .get_characters_character_id_roles()
                    .character_id(character),
                again(format!("/characters/{character}/roles"))
            ),
            "character-titles" => get!(
                client
                    .get_characters_character_id_titles()
                    .character_id(character)
            ),
            "character-notifications" => loose!(
                client
                    .get_characters_character_id_notifications()
                    .character_id(character),
                again(format!("/characters/{character}/notifications"))
            ),
            "character-calendar" => {
                let request = client
                    .get_characters_character_id_calendar()
                    .character_id(character);
                let path = format!("/characters/{character}/calendar");
                match positive_id(params, "from_event")? {
                    Some(from) => loose!(
                        request.from_event(from),
                        again(path).with("from_event", from)
                    ),
                    None => loose!(request, again(path)),
                }
            }
            "character-calendar-event" => {
                let event = id("event_id")?;
                loose!(
                    client
                        .get_characters_character_id_calendar_event_id()
                        .character_id(character)
                        .event_id(event),
                    again(format!("/characters/{character}/calendar/{event}"))
                )
            }
            "character-fittings" => loose!(
                client
                    .get_characters_character_id_fittings()
                    .character_id(character),
                again(format!("/characters/{character}/fittings"))
            ),
            "character-mining" => paged!(
                client
                    .get_characters_character_id_mining()
                    .character_id(character)
            ),
            "universe-structure" => {
                let structure_id = id("structure_id")?;
                let request = client
                    .get_universe_structures_structure_id()
                    .structure_id(structure_id);
                let structure = fetch(self, move || request.send()).await?.into_inner();
                // Where it is and what it is: not whose, or where in space.
                Ok(Response {
                    body: serde_json::json!({
                        "structure_id": structure_id,
                        "name": structure.name,
                        "solar_system_id": structure.solar_system_id,
                        "type_id": structure.type_id,
                    }),
                    pages: 1,
                    refetched: 0,
                })
            }
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
    fn scopes_are_known_read_scopes_of_the_right_kind() {
        use tether_core::scopes::{self, ScopeKind as Kind};
        for e in ENDPOINTS {
            match e.about {
                About::Public => assert_eq!(e.scope, "", "{}", e.name),
                about => {
                    let info = scopes::info(e.scope).unwrap_or_else(|| panic!("{}", e.name));
                    assert!(!scopes::is_write(e.scope), "{}", e.name);
                    // A corporation endpoint may read a data source's own
                    // character data (its notifications), not the reverse.
                    if about == About::Character {
                        assert_eq!(info.kind, Kind::Character, "{}", e.name);
                    }
                }
            }
        }
    }

    #[test]
    fn optional_ids_are_positive_numbers_or_absent() {
        let p = |v: &str| vec![("from_id".to_owned(), v.to_owned())];
        assert_eq!(positive_id(&[], "from_id").unwrap(), None);
        assert_eq!(positive_id(&p("12"), "from_id").unwrap(), Some(12));
        assert!(positive_id(&p("0"), "from_id").is_err());
        assert!(positive_id(&p("x"), "from_id").is_err());
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
