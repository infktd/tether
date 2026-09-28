//! EVE's structure notifications: reading their text and turning them
//! into a Discord message and, for some, a timer.
//!
//! A notification's text is YAML, but flat: `key: value` lines, with a few
//! lists (`- item` lines under a `key:` line). Values may carry an anchor
//! (`&id001 1035466617946`); list items may be aliases (`*id001`). That's
//! all that's read here, which keeps a YAML parser out of the plugin.

use std::collections::BTreeMap;

use chrono::{DateTime, Duration, Utc};

/// A Territorial Claim Unit and an Infrastructure Hub.
const TCU: i64 = 32226;
const IHUB: i64 = 32458;

/// Keys naming a character, corporation, alliance or constellation.
const ENTITY_KEYS: [&str; 27] = [
    "charID",
    "aggressorID",
    "aggressorCorpID",
    "aggressorAllianceID",
    "allianceID",
    "corpID",
    "creditorID",
    "debtorID",
    "allyID",
    "enemyID",
    "defenderID",
    "entityID",
    "mercID",
    "offeredID",
    "ownerID1",
    "ownerID2",
    "declaredByID",
    "againstID",
    "opponentID",
    "quitterID",
    "invokingCharID",
    "creator_id",
    "closer_id",
    "corporation_id",
    "oldOwnerCorpID",
    "newOwnerCorpID",
    "constellationID",
];

/// Seconds between 1601-01-01 (Windows file time, which EVE uses) and
/// the Unix epoch.
const FILETIME_EPOCH: i64 = 11_644_473_600;
/// File time ticks (100 ns) per second.
const TICKS: i64 = 10_000_000;

/// A notification's fields.
#[derive(Debug, Default)]
pub struct Fields {
    scalars: BTreeMap<String, String>,
    lists: BTreeMap<String, Vec<String>>,
    /// Lists of lists (`- - a` then `  - b` lines), as
    /// StructuresReinforcementChanged's `allStructureInfo`.
    nested: BTreeMap<String, Vec<Vec<String>>>,
}

/// A value without its anchor or quotes.
fn clean(value: &str) -> String {
    let mut value = value.trim();
    if let Some(rest) = value.strip_prefix('&') {
        value = rest.split_once(' ').map_or("", |(_, v)| v.trim());
    }
    let unquoted = value
        .strip_prefix('\'')
        .and_then(|v| v.strip_suffix('\''))
        .or_else(|| value.strip_prefix('"').and_then(|v| v.strip_suffix('"')));
    unquoted.unwrap_or(value).to_owned()
}

impl Fields {
    pub fn parse(text: &str) -> Self {
        let mut fields = Self::default();
        let mut list: Option<String> = None;
        let mut nested_open = false;
        for line in text.lines() {
            if line.trim().is_empty() {
                continue;
            }
            // A list item of the key above (EVE writes them unindented or
            // indented by two); `- - a` starts an inner list, whose other
            // items are indented `  - b`.
            if let Some(item) = line.trim_start().strip_prefix("- ") {
                if let Some(key) = &list {
                    if let Some(first) = item.strip_prefix("- ") {
                        nested_open = true;
                        fields
                            .nested
                            .entry(key.clone())
                            .or_default()
                            .push(vec![clean(first)]);
                    } else if nested_open && line.starts_with("  ") {
                        if let Some(inner) = fields
                            .nested
                            .get_mut(key)
                            .and_then(|lists| lists.last_mut())
                        {
                            inner.push(clean(item));
                        }
                    } else {
                        nested_open = false;
                        fields
                            .lists
                            .entry(key.clone())
                            .or_default()
                            .push(clean(item));
                    }
                }
                continue;
            }
            if line.starts_with(' ') {
                continue;
            }
            let Some((key, value)) = line.split_once(':') else {
                list = None;
                continue;
            };
            let key = key.trim().to_owned();
            let value = clean(value);
            nested_open = false;
            if value.is_empty() {
                list = Some(key);
            } else {
                list = None;
                fields.scalars.insert(key, value);
            }
        }
        fields
    }

    pub fn text(&self, key: &str) -> Option<&str> {
        self.scalars.get(key).map(String::as_str)
    }

    pub fn int(&self, key: &str) -> Option<i64> {
        self.text(key).and_then(|v| v.parse().ok())
    }

    pub fn float(&self, key: &str) -> Option<f64> {
        self.text(key).and_then(|v| v.parse().ok())
    }

    /// A list of lists (`allStructureInfo`).
    pub fn nested(&self, key: &str) -> &[Vec<String>] {
        self.nested.get(key).map_or(&[], Vec::as_slice)
    }

    pub fn ints(&self, key: &str) -> Vec<i64> {
        self.lists
            .get(key)
            .map(|items| items.iter().filter_map(|i| i.parse().ok()).collect())
            .unwrap_or_default()
    }

    /// The structure it's about.
    pub fn structure_id(&self) -> Option<i64> {
        self.int("structureID")
    }

    /// Its solar system (EVE spells the key two ways).
    pub fn system_id(&self) -> Option<i64> {
        self.int("solarsystemID")
            .or_else(|| self.int("solarSystemID"))
    }

    /// A starbase's moon (starbase notifications name no starbase).
    pub fn moon_id(&self) -> Option<i64> {
        self.int("moonID")
    }

    /// A customs office's or skyhook's planet.
    pub fn planet_id(&self) -> Option<i64> {
        self.int("planetID")
    }

    /// The structure's type (EVE spells the key two ways).
    pub fn type_id(&self) -> Option<i64> {
        self.int("structureTypeID").or_else(|| self.int("typeID"))
    }

    /// A sovereignty notification's structure type: its own, or by its
    /// campaign (1 a TCU, 2 an IHub, as aa-structures).
    pub fn sov_type_id(&self) -> Option<i64> {
        self.int("structureTypeID")
            .or_else(|| match self.int("campaignEventType") {
                Some(1) => Some(TCU),
                Some(2) => Some(IHUB),
                _ => None,
            })
    }

    /// Every id `/universe/names` can name: the system, the structure's
    /// type, the characters, corporations and alliances involved, and the
    /// services that went offline. Moons and planets aren't among them
    /// (Structures names those itself); goal and war ids aren't names.
    pub fn ids(&self) -> Vec<i64> {
        let mut ids: Vec<i64> = [self.system_id(), self.type_id(), self.sov_type_id()]
            .into_iter()
            .flatten()
            .collect();
        ids.extend(ENTITY_KEYS.iter().filter_map(|key| self.int(key)));
        ids.extend(self.ints("listOfServiceModuleIDs"));
        ids.extend(
            self.nested("allStructureInfo")
                .iter()
                .filter_map(|info| info.get(2)?.parse::<i64>().ok()),
        );
        ids.retain(|id| *id > 0);
        ids
    }

    /// An absolute file time field.
    fn filetime(&self, key: &str) -> Option<DateTime<Utc>> {
        let ticks = self.int(key)?;
        DateTime::from_timestamp(ticks / TICKS - FILETIME_EPOCH, 0)
    }

    /// A file time span field, after `at`.
    fn after(&self, at: DateTime<Utc>, key: &str) -> Option<DateTime<Utc>> {
        let ticks = self.int(key)?;
        at.checked_add_signed(Duration::seconds(ticks / TICKS))
    }

    /// A moon's name from its link (`<a href="showinfo:14//4016...">Name</a>`).
    fn moon(&self) -> String {
        self.text("moonLink")
            .and_then(|l| l.split_once('>'))
            .and_then(|(_, rest)| rest.split_once('<'))
            .map(|(name, _)| escape(name.trim()))
            .filter(|n| !n.is_empty())
            .or_else(|| self.int("moonID").map(|id| format!("moon {id}")))
            .unwrap_or_else(|| "a moon".to_owned())
    }
}

/// Which channel a notification goes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    /// Under attack, reinforced, destroyed.
    Attack,
    /// Fuel alerts, services offline, low power.
    Fuel,
    /// Online, high power, anchoring, unanchoring.
    State,
    /// Moon drills.
    Moon,
    /// Sovereignty, and the bills that keep it.
    Sov,
    /// Wars.
    War,
    /// Members joining and leaving, applications, projects.
    Corp,
}

impl Category {
    pub const ALL: [Category; 7] = [
        Category::Attack,
        Category::Fuel,
        Category::State,
        Category::Moon,
        Category::Sov,
        Category::War,
        Category::Corp,
    ];

    /// Its name in storage (`owner_channels.category`).
    pub fn name(self) -> &'static str {
        match self {
            Category::Attack => "attack",
            Category::Fuel => "fuel",
            Category::State => "state",
            Category::Moon => "moon",
            Category::Sov => "sov",
            Category::War => "war",
            Category::Corp => "corp",
        }
    }

    /// What a manager sees.
    pub fn label(self) -> &'static str {
        match self {
            Category::Attack => "Attacks",
            Category::Fuel => "Fuel and services",
            Category::State => "State changes",
            Category::Moon => "Moon extractions",
            Category::Sov => "Sovereignty and bills",
            Category::War => "Wars",
            Category::Corp => "Members and projects",
        }
    }
}

/// aa-structures' severity of a notification (its embed colour): with
/// default pings on, danger pings @everyone and warning @here. Tether's
/// bot mentions the roles of the states the settings name instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Danger,
    Warning,
    Info,
}

/// aa-structures' own "starbase reinforced", from the starbase's state.
pub const STARBASE_REINFORCED: &str = "TowerReinforcedExtra";

/// Every notification type Structures sends, as aa-structures' webhook
/// filters list them: its type, what a manager sees, its severity
/// (aa-structures' embed colour; its "success" pings nobody, as info
/// here), and the channel kind it goes to.
pub const TYPES: [(&str, &str, Severity, Category); 75] = [
    (
        "StructureUnderAttack",
        "Upwell structure under attack",
        Severity::Danger,
        Category::Attack,
    ),
    (
        "StructureLostShields",
        "Upwell structure lost shields",
        Severity::Danger,
        Category::Attack,
    ),
    (
        "StructureLostArmor",
        "Upwell structure lost armor",
        Severity::Danger,
        Category::Attack,
    ),
    (
        "StructureDestroyed",
        "Upwell structure destroyed",
        Severity::Danger,
        Category::Attack,
    ),
    (
        "TowerAlertMsg",
        "Starbase under attack",
        Severity::Warning,
        Category::Attack,
    ),
    (
        STARBASE_REINFORCED,
        "Starbase reinforced",
        Severity::Danger,
        Category::Attack,
    ),
    (
        "OrbitalAttacked",
        "Customs office under attack",
        Severity::Warning,
        Category::Attack,
    ),
    (
        "OrbitalReinforced",
        "Customs office reinforced",
        Severity::Danger,
        Category::Attack,
    ),
    (
        "SkyhookUnderAttack",
        "Skyhook under attack",
        Severity::Danger,
        Category::Attack,
    ),
    (
        "SkyhookLostShields",
        "Skyhook lost shields",
        Severity::Danger,
        Category::Attack,
    ),
    (
        "SkyhookDestroyed",
        "Skyhook destroyed",
        Severity::Danger,
        Category::Attack,
    ),
    (
        "StructureFuelAlert",
        "Upwell structure fuel alert",
        Severity::Warning,
        Category::Fuel,
    ),
    (
        "StructureJumpFuelAlert",
        "Jump gate low on liquid ozone",
        Severity::Warning,
        Category::Fuel,
    ),
    (
        "StructureRefueledExtra",
        "Upwell structure refueled",
        Severity::Info,
        Category::Fuel,
    ),
    (
        "StructureServicesOffline",
        "Upwell structure services went offline",
        Severity::Danger,
        Category::Fuel,
    ),
    (
        "StructureWentLowPower",
        "Upwell structure went low power",
        Severity::Warning,
        Category::Fuel,
    ),
    (
        "StructureLowReagentsAlert",
        "Metenox low on reagents",
        Severity::Warning,
        Category::Fuel,
    ),
    (
        "StructureNoReagentsAlert",
        "Metenox out of reagents",
        Severity::Danger,
        Category::Fuel,
    ),
    (
        "TowerResourceAlertMsg",
        "Starbase fuel alert",
        Severity::Warning,
        Category::Fuel,
    ),
    (
        "TowerRefueledExtra",
        "Starbase refueled",
        Severity::Info,
        Category::Fuel,
    ),
    (
        "StructureWentHighPower",
        "Upwell structure went high power",
        Severity::Info,
        Category::State,
    ),
    (
        "StructureOnline",
        "Upwell structure online",
        Severity::Info,
        Category::State,
    ),
    (
        "StructureAnchoring",
        "Upwell structure anchoring",
        Severity::Info,
        Category::State,
    ),
    (
        "StructureUnanchoring",
        "Upwell structure unanchoring",
        Severity::Info,
        Category::State,
    ),
    (
        "OwnershipTransferred",
        "Upwell structure ownership transferred",
        Severity::Info,
        Category::State,
    ),
    (
        "StructuresReinforcementChanged",
        "Upwell structure reinforcement time changed",
        Severity::Info,
        Category::State,
    ),
    (
        "SkyhookDeployed",
        "Skyhook deployed",
        Severity::Info,
        Category::State,
    ),
    (
        "SkyhookOnline",
        "Skyhook online",
        Severity::Info,
        Category::State,
    ),
    (
        "MoonminingExtractionStarted",
        "Moon extraction started",
        Severity::Info,
        Category::Moon,
    ),
    (
        "MoonminingExtractionFinished",
        "Moon extraction finished",
        Severity::Info,
        Category::Moon,
    ),
    (
        "MoonminingAutomaticFracture",
        "Moon automatic fracture",
        Severity::Info,
        Category::Moon,
    ),
    (
        "MoonminingLaserFired",
        "Moon laser fired",
        Severity::Info,
        Category::Moon,
    ),
    (
        "MoonminingExtractionCancelled",
        "Moon extraction cancelled",
        Severity::Warning,
        Category::Moon,
    ),
    (
        "SovStructureReinforced",
        "Sovereignty structure reinforced",
        Severity::Danger,
        Category::Sov,
    ),
    (
        "SovStructureDestroyed",
        "Sovereignty structure destroyed",
        Severity::Danger,
        Category::Sov,
    ),
    (
        "EntosisCaptureStarted",
        "Sovereignty entosis capture started",
        Severity::Warning,
        Category::Sov,
    ),
    (
        "SovCommandNodeEventStarted",
        "Sovereignty command nodes decloaking",
        Severity::Warning,
        Category::Sov,
    ),
    (
        "SovAllClaimAquiredMsg",
        "Sovereignty claimed",
        Severity::Info,
        Category::Sov,
    ),
    (
        "SovAllClaimLostMsg",
        "Sovereignty lost",
        Severity::Info,
        Category::Sov,
    ),
    (
        "AllAnchoringMsg",
        "Structure anchoring in alliance space",
        Severity::Warning,
        Category::Sov,
    ),
    (
        "InfrastructureHubBillAboutToExpire",
        "IHub bill about to expire",
        Severity::Danger,
        Category::Sov,
    ),
    (
        "IHubDestroyedByBillFailure",
        "IHub destroyed by bill failure",
        Severity::Danger,
        Category::Sov,
    ),
    (
        "BillOutOfMoneyMsg",
        "Bill out of money",
        Severity::Warning,
        Category::Sov,
    ),
    (
        "CorpAllBillMsg",
        "Bill issued",
        Severity::Warning,
        Category::Sov,
    ),
    (
        "WarDeclared",
        "War declared",
        Severity::Danger,
        Category::War,
    ),
    (
        "DeclareWar",
        "War declared (by a corporation)",
        Severity::Danger,
        Category::War,
    ),
    (
        "WarInherited",
        "War inherited",
        Severity::Danger,
        Category::War,
    ),
    (
        "WarAdopted",
        "War adopted",
        Severity::Warning,
        Category::War,
    ),
    (
        "AcceptedAlly",
        "War ally accepted",
        Severity::Warning,
        Category::War,
    ),
    (
        "AllyJoinedWarAggressorMsg",
        "War ally joined the aggressor",
        Severity::Warning,
        Category::War,
    ),
    (
        "AllyJoinedWarAllyMsg",
        "War ally joined an ally",
        Severity::Warning,
        Category::War,
    ),
    (
        "AllyJoinedWarDefenderMsg",
        "War ally joined the defender",
        Severity::Warning,
        Category::War,
    ),
    (
        "AllWarCorpJoinedAllianceMsg",
        "Corporation at war joined an alliance",
        Severity::Info,
        Category::War,
    ),
    (
        "AllWarSurrenderMsg",
        "War surrendered",
        Severity::Warning,
        Category::War,
    ),
    (
        "CorpWarSurrenderMsg",
        "War party surrendered",
        Severity::Warning,
        Category::War,
    ),
    (
        "OfferedSurrender",
        "War surrender offered by you",
        Severity::Warning,
        Category::War,
    ),
    (
        "WarSurrenderOfferMsg",
        "War surrender offered",
        Severity::Info,
        Category::War,
    ),
    (
        "OfferedToAlly",
        "War offered to become ally",
        Severity::Info,
        Category::War,
    ),
    (
        "MercOfferedNegotiationMsg",
        "War mercenary offer",
        Severity::Info,
        Category::War,
    ),
    (
        "MercOfferRetractedMsg",
        "War mercenary offer retracted",
        Severity::Info,
        Category::War,
    ),
    (
        "WarHQRemovedFromSpace",
        "War HQ removed from space",
        Severity::Warning,
        Category::War,
    ),
    (
        "WarInvalid",
        "War invalid",
        Severity::Warning,
        Category::War,
    ),
    (
        "WarRetractedByConcord",
        "War retracted by CONCORD",
        Severity::Warning,
        Category::War,
    ),
    (
        "CorpBecameWarEligible",
        "Became eligible for war",
        Severity::Warning,
        Category::War,
    ),
    (
        "CorpNoLongerWarEligible",
        "No longer eligible for war",
        Severity::Info,
        Category::War,
    ),
    (
        "CorpAppNewMsg",
        "Character submitted application",
        Severity::Info,
        Category::Corp,
    ),
    (
        "CorpAppInvitedMsg",
        "Character invited to join corporation",
        Severity::Info,
        Category::Corp,
    ),
    (
        "CharAppWithdrawMsg",
        "Character withdrew application",
        Severity::Info,
        Category::Corp,
    ),
    (
        "CharAppRejectMsg",
        "Application rejected",
        Severity::Info,
        Category::Corp,
    ),
    (
        "CorpAppRejectCustomMsg",
        "Application rejected with a message",
        Severity::Info,
        Category::Corp,
    ),
    (
        "CharAppAcceptMsg",
        "Character joins corporation",
        Severity::Info,
        Category::Corp,
    ),
    (
        "CharLeftCorpMsg",
        "Character leaves corporation",
        Severity::Info,
        Category::Corp,
    ),
    (
        "CorporationGoalCreated",
        "Corporation project created",
        Severity::Info,
        Category::Corp,
    ),
    (
        "CorporationGoalCompleted",
        "Corporation project completed",
        Severity::Info,
        Category::Corp,
    ),
    (
        "CorporationGoalClosed",
        "Corporation project closed",
        Severity::Info,
        Category::Corp,
    ),
];

/// Types aa-structures sends only for an owner marked alliance main
/// (`is_alliance_main`): every corporation of an alliance gets them.
const ALLIANCE_LEVEL: [&str; 20] = [
    "BillOutOfMoneyMsg",
    "InfrastructureHubBillAboutToExpire",
    "IHubDestroyedByBillFailure",
    "SovAllClaimAquiredMsg",
    "SovAllClaimLostMsg",
    "SovCommandNodeEventStarted",
    "EntosisCaptureStarted",
    "SovStructureDestroyed",
    "SovStructureReinforced",
    "AllyJoinedWarAggressorMsg",
    "AllyJoinedWarAllyMsg",
    "AllyJoinedWarDefenderMsg",
    "CorpWarSurrenderMsg",
    "CorpBecameWarEligible",
    "CorpNoLongerWarEligible",
    "WarAdopted",
    "WarDeclared",
    "WarInherited",
    "WarRetractedByConcord",
    "WarSurrenderOfferMsg",
];

/// Whether only an alliance main's notifications of this type are sent.
pub fn alliance_level(kind: &str) -> bool {
    ALLIANCE_LEVEL.contains(&kind)
}

/// Types Tether makes itself rather than reads from ESI.
pub const GENERATED: [&str; 4] = [
    STARBASE_REINFORCED,
    "StructureJumpFuelAlert",
    "StructureRefueledExtra",
    "TowerRefueledExtra",
];

/// Whether a notification is about one of the owner's structures (sent
/// once that structure is known), rather than the corporation.
pub fn structure_related(kind: &str) -> bool {
    matches!(
        category(kind),
        Some(Category::Attack | Category::Fuel | Category::State | Category::Moon)
    ) && kind != "StructuresReinforcementChanged"
}

/// A type's severity (info for any not listed).
pub fn severity(kind: &str) -> Severity {
    TYPES
        .iter()
        .find(|(k, _, _, _)| *k == kind)
        .map_or(Severity::Info, |(_, _, s, _)| *s)
}

pub fn category(kind: &str) -> Option<Category> {
    TYPES
        .iter()
        .find(|(k, _, _, _)| *k == kind)
        .map(|(_, _, _, c)| *c)
}

/// A timer a notification announces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Timer {
    /// Structure Timers' names: Armor, Hull, Final, Anchoring,
    /// Unanchoring.
    pub kind: &'static str,
    pub at: DateTime<Utc>,
}

pub fn timer(kind: &str, fields: &Fields, at: DateTime<Utc>) -> Option<Timer> {
    // A customs office out of reinforcement, and a skyhook's (as
    // aa-structures reads them): absolute file times.
    match kind {
        "OrbitalReinforced" => {
            return Some(Timer {
                kind: "Final",
                at: fields.filetime("reinforceExitTime")?,
            });
        }
        "SkyhookLostShields" => {
            return Some(Timer {
                kind: "Final",
                at: fields.filetime("timestamp")?,
            });
        }
        _ => {}
    }
    let (kind, key) = match kind {
        "StructureLostShields" => ("Armor", "timeLeft"),
        "StructureLostArmor" => ("Hull", "timeLeft"),
        "StructureAnchoring" => ("Anchoring", "timeLeft"),
        "StructureUnanchoring" => ("Unanchoring", "timeLeft"),
        _ => return None,
    };
    Some(Timer {
        kind,
        at: fields.after(at, key)?,
    })
}

/// A sovereignty structure reinforced: when its command nodes decloak,
/// and which structure (aa-structures' timer names: TCU, I-HUB).
pub fn sov_timer(kind: &str, fields: &Fields) -> Option<(&'static str, DateTime<Utc>)> {
    if kind != "SovStructureReinforced" {
        return None;
    }
    let structure = match fields.int("campaignEventType") {
        Some(1) => "TCU",
        Some(2) => "I-HUB",
        _ => "Other",
    };
    Some((structure, fields.filetime("decloakTime")?))
}

/// An infrastructure hub's bill, or another.
fn bill(fields: &Fields) -> &'static str {
    match fields.int("billTypeID") {
        Some(7) => "Infrastructure Hub bill",
        _ => "bill",
    }
}

/// What the message needs to know besides the notification.
pub struct Context<'a> {
    /// The structure's name, if Structures has it.
    pub structure: Option<String>,
    /// Who sent it (sovereignty's name the alliance holding it), if known.
    pub sender: Option<String>,
    /// A name for an id, if known.
    pub name: &'a dyn Fn(i64) -> Option<String>,
}

/// A player-chosen name made safe for Discord: no links, code spans,
/// emphasis, spoilers or strikethrough built from its characters (each
/// escaped, backslashes too).
pub fn escape(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        if matches!(
            c,
            '[' | ']' | '(' | ')' | '`' | '\\' | '*' | '_' | '~' | '|'
        ) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Whether a notification names its holder by its sender (sovereignty).
pub fn names_sender(kind: &str) -> bool {
    matches!(
        kind,
        "SovStructureReinforced"
            | "SovStructureDestroyed"
            | "EntosisCaptureStarted"
            | "SovCommandNodeEventStarted"
    )
}

/// A message cut to `max` characters, marked where it was cut.
pub fn clip(message: &str, max: usize) -> String {
    if message.chars().count() <= max {
        return message.to_owned();
    }
    let mut cut: String = message.chars().take(max.saturating_sub(1)).collect();
    cut.push('…');
    cut
}

fn eve(t: DateTime<Utc>) -> String {
    t.format("%Y-%m-%d %H:%M").to_string()
}

/// ISK with thousands separators, as a whole number.
fn isk(value: Option<f64>) -> String {
    let Some(value) = value else {
        return "?".to_owned();
    };
    let digits = format!("{:.0}", value.abs());
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    if value < 0.0 {
        out.insert(0, '-');
    }
    out
}

/// Text a player wrote (an application, a project's name, a war HQ): no
/// markup, made safe as names are, and cut to `max` characters.
fn player_text(text: &str, max: usize) -> String {
    let mut plain = String::new();
    let mut in_tag = false;
    for c in text.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            c if !in_tag => plain.push(c),
            _ => {}
        }
    }
    let mut cut: String = plain.trim().chars().take(max).collect();
    if plain.trim().chars().count() > max {
        cut.push('…');
    }
    // No links from what a player wrote: Discord doesn't link an escaped
    // scheme.
    escape(&cut).replace("://", "\\://")
}

/// Player text as a Discord quote, line by line; no heading, list or
/// nested quote from a line's start.
fn quote(text: &str) -> String {
    let text = player_text(text, 500);
    if text.is_empty() {
        return String::new();
    }
    text.lines()
        .map(|line| {
            let line = line.trim_start();
            if line.starts_with(['#', '-', '>', '+']) {
                format!("\n> \\{line}")
            } else {
                format!("\n> {line}")
            }
        })
        .collect()
}

fn percent(value: Option<f64>) -> String {
    value.map_or_else(|| "?".to_owned(), |v| format!("{v:.0}%"))
}

/// Shield, armor and hull left, as far as the notification says: in
/// percent (`shieldPercentage`) or as a fraction (`shieldValue`,
/// `shieldLevel`).
fn damage(fields: &Fields) -> String {
    let part = |name: &str| {
        fields
            .float(&format!("{name}Percentage"))
            .or_else(|| fields.float(&format!("{name}Value")).map(|v| v * 100.0))
            .or_else(|| fields.float(&format!("{name}Level")).map(|v| v * 100.0))
            .map(|v| format!("{name} {v:.0}%"))
    };
    let parts: Vec<String> = ["shield", "armor", "hull"]
        .into_iter()
        .filter_map(part)
        .collect();
    let Some((first, rest)) = parts.split_first() else {
        return String::new();
    };
    let mut first = first.clone();
    // Sentence case.
    if let Some(c) = first.get(..1) {
        first = c.to_uppercase() + first.get(1..).unwrap_or_default();
    }
    let mut text = vec![first];
    text.extend(rest.iter().cloned());
    format!(" {}.", text.join(", "))
}

/// The Discord message for a notification.
pub fn message(kind: &str, fields: &Fields, at: DateTime<Utc>, cx: &Context<'_>) -> Option<String> {
    let name = |id: Option<i64>| id.and_then(|id| (cx.name)(id));
    let structure = cx
        .structure
        .clone()
        .or_else(|| fields.text("structureName").map(str::to_owned))
        .or_else(|| fields.structure_id().map(|id| format!("Structure {id}")))
        .unwrap_or_else(|| "A structure".to_owned());
    let structure = escape(&structure);
    let type_name = name(fields.type_id());
    let system = name(fields.system_id());
    // Starbases and orbitals: at their moon or planet.
    let at_body = fields
        .moon_id()
        .filter(|_| kind.starts_with("Tower"))
        .or_else(|| {
            fields
                .planet_id()
                .filter(|_| kind.starts_with("Orbital") || kind.starts_with("Skyhook"))
        })
        .map(|id| name(Some(id)).map_or_else(|| format!("celestial {id}"), |n| escape(&n)));
    let mut place = structure.clone();
    if let Some(t) = &type_name {
        place.push_str(&format!(" ({t})"));
    }
    if let Some(b) = &at_body {
        place.push_str(&format!(" at {b}"));
    }
    if let Some(s) = &system {
        place.push_str(&format!(" in {s}"));
    }
    // Who, for starbases and customs offices.
    let aggressor: Vec<String> = [
        name(fields.int("aggressorID")),
        name(fields.int("aggressorCorpID")),
        name(fields.int("aggressorAllianceID")),
    ]
    .into_iter()
    .flatten()
    .filter(|s| !s.is_empty())
    .map(|s| escape(&s))
    .collect();
    let aggressor = if aggressor.is_empty() {
        String::new()
    } else {
        format!(" by {}", aggressor.join(", "))
    };
    let timer = timer(kind, fields, at).map(|t| eve(t.at));
    let text = match kind {
        "StructureUnderAttack" => {
            let attacker: Vec<String> = [
                name(fields.int("charID")),
                fields.text("corpName").map(str::to_owned),
                fields.text("allianceName").map(str::to_owned),
            ]
            .into_iter()
            .flatten()
            .filter(|s| !s.is_empty())
            .map(|s| escape(&s))
            .collect();
            let by = if attacker.is_empty() {
                String::new()
            } else {
                format!(" by {}", attacker.join(", "))
            };
            format!(
                "Under attack: {place}{by}. Shield {}, armor {}, hull {}.",
                percent(fields.float("shieldPercentage")),
                percent(fields.float("armorPercentage")),
                percent(fields.float("hullPercentage")),
            )
        }
        "StructureLostShields" => format!(
            "Reinforced: {place} lost its shields. Armor timer ends {} EVE.",
            timer.unwrap_or_else(|| "at an unknown time".to_owned())
        ),
        "StructureLostArmor" => format!(
            "Reinforced: {place} lost its armor. Hull timer ends {} EVE.",
            timer.unwrap_or_else(|| "at an unknown time".to_owned())
        ),
        "StructureDestroyed" => format!("Destroyed: {place}."),
        "StructureLowReagentsAlert" => {
            format!("Low reagents: {place} has magmatic gas for about a day more.")
        }
        "StructureNoReagentsAlert" => {
            format!("Out of reagents: {place} has run out of magmatic gas.")
        }
        "TowerAlertMsg" => format!(
            "Starbase under attack: {place}{aggressor}.{}",
            damage(fields)
        ),
        "TowerResourceAlertMsg" => {
            format!("Starbase fuel alert: {place} is running low on fuel or strontium.")
        }
        "OrbitalAttacked" => format!(
            "Customs office under attack: {place}{aggressor}.{}",
            damage(fields)
        ),
        "OrbitalReinforced" => format!(
            "Customs office reinforced: {place}{aggressor}. It comes out of reinforcement {} EVE.",
            timer.unwrap_or_else(|| "at an unknown time".to_owned())
        ),
        "SkyhookUnderAttack" => {
            let attacker: Vec<String> = [
                name(fields.int("charID")),
                fields.text("corpName").map(str::to_owned),
                fields.text("allianceName").map(str::to_owned),
            ]
            .into_iter()
            .flatten()
            .filter(|s| !s.is_empty())
            .map(|s| escape(&s))
            .collect();
            let by = if attacker.is_empty() {
                aggressor
            } else {
                format!(" by {}", attacker.join(", "))
            };
            format!("Skyhook under attack: {place}{by}.{}", damage(fields))
        }
        "SkyhookLostShields" => format!(
            "Skyhook reinforced: {place} lost its shields. It comes out of reinforcement {} EVE.",
            timer.unwrap_or_else(|| "at an unknown time".to_owned())
        ),
        "SkyhookDestroyed" => format!("Skyhook destroyed: {place}."),
        "SkyhookDeployed" => format!("Skyhook deployed: {place} started onlining."),
        "SkyhookOnline" => format!("Skyhook online: {place} is online."),
        "StructureFuelAlert" => format!("Fuel alert: {place} is running low on fuel."),
        "StructureServicesOffline" => {
            let services: Vec<String> = fields
                .ints("listOfServiceModuleIDs")
                .into_iter()
                .map(|id| (cx.name)(id).unwrap_or_else(|| format!("type {id}")))
                .collect();
            if services.is_empty() {
                format!("Services offline: {place}.")
            } else {
                format!("Services offline: {place}: {}.", services.join(", "))
            }
        }
        "StructureWentLowPower" => format!("Low power: {place} went low power."),
        "StructureWentHighPower" => format!("High power: {place} went high power."),
        "StructureOnline" => format!("Online: {place} is online."),
        "StructureAnchoring" => match timer {
            Some(t) => format!("Anchoring: {place} started anchoring; it anchors at {t} EVE."),
            None => format!("Anchoring: {place} started anchoring."),
        },
        "StructureUnanchoring" => match timer {
            Some(t) => {
                format!("Unanchoring: {place} started unanchoring; it unanchors at {t} EVE.")
            }
            None => format!("Unanchoring: {place} started unanchoring."),
        },
        "MoonminingExtractionStarted" => {
            let ready = fields.filetime("readyTime").map(eve);
            let auto = fields.filetime("autoTime").map(eve);
            match (ready, auto) {
                (Some(r), Some(a)) => format!(
                    "Extraction started: {place}, {}. The chunk arrives {r} EVE and fractures automatically {a} EVE.",
                    fields.moon()
                ),
                _ => format!("Extraction started: {place}, {}.", fields.moon()),
            }
        }
        "MoonminingExtractionFinished" => match fields.filetime("autoTime").map(eve) {
            Some(a) => format!(
                "Chunk arrived: {place}, {}. It fractures automatically {a} EVE.",
                fields.moon()
            ),
            None => format!("Chunk arrived: {place}, {}.", fields.moon()),
        },
        "MoonminingAutomaticFracture" => {
            format!("Moon fractured automatically: {place}, {}.", fields.moon())
        }
        "MoonminingLaserFired" => format!("Moon fractured: {place}, {}.", fields.moon()),
        "MoonminingExtractionCancelled" => {
            format!("Extraction cancelled: {place}, {}.", fields.moon())
        }
        _ => return other(kind, fields, cx),
    };
    Some(text)
}

/// Notifications about the corporation rather than one of its
/// structures: sovereignty and bills, wars, members and projects, and an
/// owner's structures changing hands or reinforcement hour, as
/// aa-structures words them.
fn other(kind: &str, fields: &Fields, cx: &Context<'_>) -> Option<String> {
    let name = |id: Option<i64>| id.and_then(|id| (cx.name)(id)).map(|n| escape(&n));
    let who = |key: &str| name(fields.int(key)).unwrap_or_else(|| "someone".to_owned());
    let system = name(fields.system_id()).unwrap_or_else(|| "a system".to_owned());
    let when = |key: &str| {
        fields.filetime(key).map_or_else(
            || "at an unknown time".to_owned(),
            |t| format!("{} EVE", eve(t)),
        )
    };
    let sov_type = name(fields.sov_type_id()).unwrap_or_else(|| "sovereignty structure".to_owned());
    let owner = cx
        .sender
        .as_deref()
        .map_or_else(|| "(unknown)".to_owned(), escape);
    let text = match kind {
        // Sovereignty and bills.
        "SovStructureReinforced" => format!(
            "Sovereignty structure reinforced: the {sov_type} in {system} belonging to {owner} \
             was reinforced by hostile forces. Its command nodes begin decloaking {}.",
            when("decloakTime")
        ),
        "SovStructureDestroyed" => format!(
            "Sovereignty structure destroyed: the command nodes for the {sov_type} in {system} \
             belonging to {owner} were destroyed by hostile forces."
        ),
        "EntosisCaptureStarted" => format!(
            "Entosis capture started: a capsuleer is influencing the {sov_type} in {system} \
             belonging to {owner} with an Entosis Link."
        ),
        "SovCommandNodeEventStarted" => format!(
            "Command nodes decloaking: command nodes for the {sov_type} in {system} belonging to \
             {owner} can now be found throughout the {} constellation.",
            name(fields.int("constellationID")).unwrap_or_else(|| "system's".to_owned())
        ),
        "SovAllClaimAquiredMsg" => format!(
            "Sovereignty claimed: DED acknowledges that member corporation {} has claimed \
             sovereignty on behalf of {} in {system}.",
            who("corpID"),
            who("allianceID")
        ),
        "SovAllClaimLostMsg" => format!(
            "Sovereignty lost: member corporation {} has lost its claim to sovereignty on behalf \
             of {} in {system}.",
            who("corpID"),
            who("allianceID")
        ),
        "AllAnchoringMsg" => {
            let by = match name(fields.int("allianceID")) {
                Some(alliance) => format!("{} ({alliance})", who("corpID")),
                None => who("corpID"),
            };
            let near = fields
                .moon_id()
                .map(|id| {
                    format!(
                        " near {}",
                        name(Some(id)).unwrap_or_else(|| format!("moon {id}"))
                    )
                })
                .unwrap_or_default();
            format!(
                "Anchored in alliance space: a {} from {by} anchored in {system}{near}.",
                name(fields.type_id()).unwrap_or_else(|| "structure".to_owned())
            )
        }
        "InfrastructureHubBillAboutToExpire" => format!(
            "IHub bill about to expire: the maintenance bill for the Infrastructure Hub in {system} \
             expires {}; unpaid, the Infrastructure Hub self-destructs.",
            when("dueDate")
        ),
        "IHubDestroyedByBillFailure" => {
            let hub = name(fields.type_id()).unwrap_or_else(|| "Infrastructure Hub".to_owned());
            format!(
                "{hub} self-destructed: the {hub} in {system} self-destructed, as its maintenance \
                 bills weren't paid."
            )
        }
        "BillOutOfMoneyMsg" => format!(
            "Insufficient funds for bill: the corporation wallet division for automatic payments \
             can't pay the {} due {}. Transfer funds to it to meet pending automatic bills.",
            bill(fields),
            when("dueDate")
        ),
        "CorpAllBillMsg" => format!(
            "Bill issued: a bill of {} ISK, due {}, owed by {} to {}, was issued {}. It's for {}.",
            isk(fields.float("amount")),
            when("dueDate"),
            who("debtorID"),
            who("creditorID"),
            when("currentDate"),
            bill(fields)
        ),
        // Wars.
        "WarDeclared" => format!(
            "War declared: {} declared war on {} with {} as war headquarters. Within {} hours \
             fighting can legally occur between those involved.",
            who("declaredByID"),
            who("againstID"),
            player_text(fields.text("warHQ").unwrap_or("an unknown structure"), 200),
            fields.int("delayHours").unwrap_or(24)
        ),
        "DeclareWar" => format!(
            "War declared: {} declared war on {}. Within 24 hours fighting can legally occur \
             between those involved.",
            who("entityID"),
            who("defenderID")
        ),
        "WarInherited" => format!(
            "War inherited: {alliance} inherited the war between {} and {} from newly joined {}. \
             Within 24 hours fighting can legally occur with {alliance}.",
            who("declaredByID"),
            who("againstID"),
            who("quitterID"),
            alliance = who("allianceID")
        ),
        "WarAdopted" => format!(
            "War adopted: {against} is no longer a member of {}, so a new war between {} and \
             {against} has begun.",
            who("allianceID"),
            who("declaredByID"),
            against = who("againstID")
        ),
        "AcceptedAlly" => format!(
            "Ally accepted: {} joined the war against {}; {} accepted the offer {} for {} ISK.",
            who("allyID"),
            who("enemyID"),
            who("charID"),
            when("time"),
            isk(fields.float("iskValue"))
        ),
        "AllyJoinedWarAggressorMsg" | "AllyJoinedWarAllyMsg" | "AllyJoinedWarDefenderMsg" => {
            format!(
                "Ally joined a war: {} joined {} in the war against {}, from {}.",
                who("allyID"),
                who("defenderID"),
                who("aggressorID"),
                when("startTime")
            )
        }
        "AllWarCorpJoinedAllianceMsg" => format!(
            "At war with a corporation joining an alliance: {corp} is joining {alliance}. As \
             you're at war with {corp}, in 24 hours you're at war with {alliance} too.",
            corp = who("corpID"),
            alliance = who("allianceID")
        ),
        "AllWarSurrenderMsg" => format!(
            "Surrendered: {} surrendered in the war against {}.",
            who("declaredByID"),
            who("againstID")
        ),
        "CorpWarSurrenderMsg" => format!(
            "War ending: one party surrendered, so the war between {} and {} ends in about 24 \
             hours.",
            who("againstID"),
            who("declaredByID")
        ),
        "OfferedSurrender" => format!(
            "Surrender offered: {} offered to surrender to {}, offering {} ISK. If accepted, the \
             war ends in 24 hours and neither can declare war on the other for 2 weeks.",
            who("charID"),
            who("offeredID"),
            isk(fields.float("iskValue"))
        ),
        "WarSurrenderOfferMsg" => format!(
            "Surrender offered: {} offered to end the war with {} for {} ISK. If accepted, the \
             war ends in 24 hours and neither can declare war on the other for 2 weeks.",
            who("ownerID1"),
            who("ownerID2"),
            isk(fields.float("iskValue"))
        ),
        "OfferedToAlly" => format!(
            "Offered to ally: {} offered to ally with {} in the war against {}, for {} ISK.",
            who("mercID"),
            who("defenderID"),
            who("aggressorID"),
            isk(fields.float("iskValue"))
        ),
        "MercOfferedNegotiationMsg" => format!(
            "Mercenary offer: {} offered {} its services in the war against {} for {} ISK.",
            who("mercID"),
            who("defenderID"),
            who("aggressorID"),
            isk(fields.float("iskValue"))
        ),
        "MercOfferRetractedMsg" => format!(
            "Mercenary offer retracted: {} retracted its offer to support {} in the war against {}.",
            who("mercID"),
            who("defenderID"),
            who("aggressorID")
        ),
        "WarHQRemovedFromSpace" => format!(
            "War HQ lost: the war HQ {} is gone, so CONCORD declared the war by {} against {}, \
             declared {}, invalid; it's in its cooldown period.",
            player_text(fields.text("warHQ").unwrap_or("?"), 200),
            who("declaredByID"),
            who("againstID"),
            when("timeDeclared")
        ),
        "WarInvalid" => format!(
            "War invalid: CONCORD retracted the war between {} and {}, as a party became \
             ineligible for war declarations. Fighting must cease {}.",
            who("declaredByID"),
            who("againstID"),
            when("endDate")
        ),
        "WarRetractedByConcord" => format!(
            "War retracted: CONCORD retracted the war between {} and {}. After {} CONCORD \
             responds to hostilities between them in full force.",
            who("declaredByID"),
            who("againstID"),
            when("endDate")
        ),
        "CorpBecameWarEligible" => "Eligible for war: your corporation or alliance can now take \
             part in formal war declarations, for example because it (or a corporation in the \
             alliance) owns a structure in space."
            .to_owned(),
        "CorpNoLongerWarEligible" => "No longer eligible for war: your corporation or alliance \
             can no longer take part in formal war declarations, as none of its corporations owns \
             a structure in space. A formal war it's in ends in 24 hours."
            .to_owned(),
        // Members and projects.
        "CorpAppNewMsg" => format!(
            "New application: {} applied to join {}.{}",
            who("charID"),
            who("corpID"),
            quote(fields.text("applicationText").unwrap_or(""))
        ),
        "CorpAppInvitedMsg" => format!(
            "Invited: {} was invited to join {} by {}.{}",
            who("charID"),
            who("corpID"),
            who("invokingCharID"),
            quote(fields.text("applicationText").unwrap_or(""))
        ),
        "CharAppWithdrawMsg" => format!(
            "Application withdrawn: {} withdrew their application to join {}.{}",
            who("charID"),
            who("corpID"),
            quote(fields.text("applicationText").unwrap_or(""))
        ),
        "CharAppRejectMsg" => format!(
            "Application rejected: the application from {} to join {} was rejected.",
            who("charID"),
            who("corpID")
        ),
        "CorpAppRejectCustomMsg" => {
            let reason = quote(fields.text("customMessage").unwrap_or(""));
            format!(
                "Application rejected: the application from {} to join {} was rejected.{}{}",
                who("charID"),
                who("corpID"),
                quote(fields.text("applicationText").unwrap_or("")),
                if reason.is_empty() {
                    String::new()
                } else {
                    format!("\nThe reply:{reason}")
                }
            )
        }
        "CharAppAcceptMsg" => format!(
            "Joined: {} is now a member of {}.",
            who("charID"),
            who("corpID")
        ),
        "CharLeftCorpMsg" => format!(
            "Left: {} is no longer a member of {}.",
            who("charID"),
            who("corpID")
        ),
        "CorporationGoalCreated" => format!(
            "New project: {} created the project {}, open for contributions.",
            who("creator_id"),
            player_text(fields.text("goal_name").unwrap_or("?"), 200)
        ),
        "CorporationGoalCompleted" => format!(
            "Project completed: {}, created by {}, reached its target.",
            player_text(fields.text("goal_name").unwrap_or("?"), 200),
            who("creator_id")
        ),
        "CorporationGoalClosed" => format!(
            "Project closed: {} closed the project {}; it takes no further contributions.",
            who("closer_id"),
            player_text(fields.text("goal_name").unwrap_or("?"), 200)
        ),
        // An owner's structures.
        "OwnershipTransferred" => format!(
            "Ownership transferred: the {} {} in {system} was transferred from {} to {} by {}.",
            name(fields.type_id()).unwrap_or_else(|| "structure".to_owned()),
            player_text(fields.text("structureName").unwrap_or("?"), 200),
            who("oldOwnerCorpID"),
            who("newOwnerCorpID"),
            who("charID")
        ),
        "StructuresReinforcementChanged" => {
            const LISTED: usize = 20;
            let all = fields.nested("allStructureInfo");
            let mut list: Vec<String> = all
                .iter()
                .take(LISTED)
                .map(|info| {
                    let structure = player_text(info.get(1).map_or("?", String::as_str), 200);
                    match name(info.get(2).and_then(|t| t.parse().ok())) {
                        Some(kind) => format!("{structure} ({kind})"),
                        None => structure,
                    }
                })
                .collect();
            if all.len() > LISTED {
                list.push(format!("and {} more", all.len() - LISTED));
            }
            format!(
                "Reinforcement hour changed to {}:00 for {}. It takes effect {}.",
                fields
                    .int("hour")
                    .map_or_else(|| "?".to_owned(), |h| h.to_string()),
                if list.is_empty() {
                    "the owner's structures".to_owned()
                } else {
                    list.join(", ")
                },
                when("timestamp")
            )
        }
        _ => return None,
    };
    Some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ATTACK: &str = "allianceID: 99005338\nallianceLinkData:\n- showinfo\n- 16159\n- 99005338\n\
        allianceName: Pandemic Horde\narmorPercentage: 100.0\ncharID: 2112625428\n\
        corpLinkData:\n- showinfo\n- 2\n- 98388312\ncorpName: Horde Vanguard.\n\
        hullPercentage: 100.0\nshieldPercentage: 94.88\nsolarsystemID: 30000142\n\
        structureID: &id001 1035466617946\nstructureShowInfoData:\n- showinfo\n- 35832\n- *id001\n\
        structureTypeID: 35832\n";

    const SHIELDS: &str = "solarsystemID: 30000142\nstructureID: &id001 1035466617946\n\
        structureShowInfoData:\n- showinfo\n- 35832\n- *id001\nstructureTypeID: 35832\n\
        timeLeft: 1728000000000\ntimestamp: 132148470780000000\nvulnerableTime: 9000000000\n";

    const OFFLINE: &str = "listOfServiceModuleIDs:\n- 35894\n- 35878\nsolarsystemID: 30000142\n\
        structureID: &id001 1035466617946\nstructureShowInfoData:\n- showinfo\n- 35832\n- *id001\n\
        structureTypeID: 35832\n";

    const STARTED: &str = "autoTime: 133090956000000000\nmoonID: 40009081\n\
        moonLink: <a href=\"showinfo:14//40009081\">Jita IV - Moon 4</a>\n\
        oreVolumeByType:\n  46676: 1000000.0\nreadyTime: 133090848000000000\n\
        solarSystemID: 30000142\nstartedBy: 2112625428\nstructureID: 1035466617946\n\
        structureName: Jita - Drill One\nstructureTypeID: 35835\n";

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn names(id: i64) -> Option<String> {
        match id {
            30000142 => Some("Jita".into()),
            35832 => Some("Astrahus".into()),
            35894 => Some("Standup Cloning Center I".into()),
            2112625428 => Some("Some Pilot".into()),
            _ => None,
        }
    }

    #[test]
    fn reads_anchored_values_and_lists() {
        let f = Fields::parse(ATTACK);
        assert_eq!(f.structure_id(), Some(1035466617946));
        assert_eq!(f.system_id(), Some(30000142));
        assert_eq!(f.text("corpName"), Some("Horde Vanguard."));
        assert_eq!(f.float("shieldPercentage"), Some(94.88));
        assert_eq!(f.ints("allianceLinkData"), vec![16159, 99005338]);
        let offline = Fields::parse(OFFLINE);
        assert_eq!(offline.ints("listOfServiceModuleIDs"), vec![35894, 35878]);
        assert!(offline.ids().contains(&35894));
    }

    #[test]
    fn attack_message_names_the_place_and_attacker() {
        let f = Fields::parse(ATTACK);
        let cx = Context {
            structure: Some("Jita - Keep".into()),
            sender: None,
            name: &names,
        };
        let text = message("StructureUnderAttack", &f, Utc::now(), &cx).unwrap();
        assert_eq!(
            text,
            "Under attack: Jita - Keep (Astrahus) in Jita by Some Pilot, Horde Vanguard., \
             Pandemic Horde. Shield 95%, armor 100%, hull 100%."
        );
    }

    #[test]
    fn lost_shields_gives_the_armor_timer() {
        let f = Fields::parse(SHIELDS);
        let when = at("2026-09-26T12:00:00Z");
        // 1,728,000,000,000 ticks is two days.
        let t = timer("StructureLostShields", &f, when).unwrap();
        assert_eq!(t.kind, "Armor");
        assert_eq!(t.at, at("2026-09-28T12:00:00Z"));
        let cx = Context {
            structure: None,
            sender: None,
            name: &names,
        };
        let text = message("StructureLostShields", &f, when, &cx).unwrap();
        assert!(
            text.contains("Structure 1035466617946 (Astrahus) in Jita"),
            "{text}"
        );
        assert!(text.contains("2026-09-28 12:00 EVE"), "{text}");
    }

    #[test]
    fn moon_messages_read_file_times_and_the_moon_link() {
        let f = Fields::parse(STARTED);
        let cx = Context {
            structure: None,
            sender: None,
            name: &names,
        };
        let text = message("MoonminingExtractionStarted", &f, Utc::now(), &cx).unwrap();
        assert!(text.contains("Jita - Drill One"), "{text}");
        assert!(text.contains("Jita IV - Moon 4"), "{text}");
        assert!(text.contains("in Jita"), "{text}");
        // 133090848000000000 ticks after 1601 is 2022-10-01 08:00 UTC.
        assert!(text.contains("arrives 2022-10-01 08:00 EVE"), "{text}");
        assert_eq!(
            category("MoonminingExtractionStarted"),
            Some(Category::Moon)
        );
        assert_eq!(category("CorpAppNewMsg"), Some(Category::Corp));
        assert_eq!(category("NotAType"), None);
    }

    #[test]
    fn names_cannot_make_links() {
        assert_eq!(
            escape("[Click](https://x) `x`"),
            "\\[Click\\]\\(https://x\\) \\`x\\`"
        );
        let cx = Context {
            structure: Some("[Keep](https://evil.example)".into()),
            sender: None,
            name: &names,
        };
        let text = message(
            "StructureDestroyed",
            &Fields::parse(ATTACK),
            Utc::now(),
            &cx,
        )
        .unwrap();
        assert!(
            text.starts_with("Destroyed: \\[Keep\\]\\(https://evil.example\\) (Astrahus)"),
            "{text}"
        );
    }

    #[test]
    fn starbase_and_orbital_messages_name_the_moon_or_planet() {
        let lookup = |id: i64| match id {
            40009081 => Some("Jita IV - Moon 4".to_owned()),
            40009077 => Some("Jita IV".to_owned()),
            16213 => Some("Caldari Control Tower".to_owned()),
            2233 => Some("Customs Office".to_owned()),
            other => names(other),
        };
        let tower = Fields::parse(
            "aggressorAllianceID: null\naggressorCorpID: null\naggressorID: 2112625428\n\
             armorValue: 1.0\nhullValue: 1.0\nmoonID: 40009081\nshieldValue: 0.4999\n\
             solarSystemID: 30000142\ntypeID: 16213\n",
        );
        assert_eq!(tower.moon_id(), Some(40009081));
        let cx = Context {
            structure: Some("Home Tower".into()),
            sender: None,
            name: &lookup,
        };
        let text = message("TowerAlertMsg", &tower, Utc::now(), &cx).unwrap();
        assert_eq!(
            text,
            "Starbase under attack: Home Tower (Caldari Control Tower) at Jita IV - Moon 4 in Jita \
             by Some Pilot. Shield 50%, armor 100%, hull 100%."
        );
        assert_eq!(category("TowerAlertMsg"), Some(Category::Attack));
        // Every type the settings list is one Structures sends, once.
        for (i, (kind, _, _, _)) in TYPES.iter().enumerate() {
            assert!(category(kind).is_some(), "{kind}");
            assert!(!TYPES[..i].iter().any(|(k, _, _, _)| k == kind), "{kind}");
        }
        assert_eq!(severity("StructureLostArmor"), Severity::Danger);
        assert_eq!(severity("StructureOnline"), Severity::Info);
        // As aa-structures' embeds: losing shields is danger, a starbase
        // under attack a warning.
        assert_eq!(severity("StructureLostShields"), Severity::Danger);
        assert_eq!(severity("SkyhookLostShields"), Severity::Danger);
        assert_eq!(severity("TowerAlertMsg"), Severity::Warning);
        assert_eq!(category("TowerResourceAlertMsg"), Some(Category::Fuel));

        // 133090848000000000 is 2022-10-01 08:00.
        let reinforced = Fields::parse(
            "aggressorAllianceID: 99005338\naggressorCorpID: 98388312\naggressorID: 2112625428\n\
             planetID: 40009077\nplanetTypeID: 2016\nreinforceExitTime: 133090848000000000\n\
             solarSystemID: 30000142\ntypeID: 2233\n",
        );
        let t = timer("OrbitalReinforced", &reinforced, Utc::now()).unwrap();
        assert_eq!(t.kind, "Final");
        assert_eq!(t.at, at("2022-10-01T08:00:00Z"));
        let cx = Context {
            structure: Some("Customs Office (Jita IV)".into()),
            sender: None,
            name: &lookup,
        };
        let text = message("OrbitalReinforced", &reinforced, Utc::now(), &cx).unwrap();
        assert!(
            text.starts_with(
                "Customs office reinforced: Customs Office \\(Jita IV\\) (Customs Office) at Jita IV in Jita by Some Pilot."
            ),
            "{text}"
        );
        assert!(text.ends_with("2022-10-01 08:00 EVE."), "{text}");
        assert!(reinforced.ids().contains(&2112625428));
        assert!(!reinforced.ids().contains(&40009077));
    }

    #[test]
    fn metenox_reagents_are_fuel() {
        assert_eq!(category("StructureLowReagentsAlert"), Some(Category::Fuel));
        assert_eq!(category("StructureNoReagentsAlert"), Some(Category::Fuel));
        assert_eq!(category("SkyhookOnline"), Some(Category::State));
        assert_eq!(category("SkyhookLostShields"), Some(Category::Attack));
        let f = Fields::parse(OFFLINE);
        let cx = Context {
            structure: Some("Drill".into()),
            sender: None,
            name: &names,
        };
        let text = message("StructureNoReagentsAlert", &f, Utc::now(), &cx).unwrap();
        assert_eq!(
            text,
            "Out of reagents: Drill (Astrahus) in Jita has run out of magmatic gas."
        );
    }

    #[test]
    fn services_offline_lists_the_services() {
        let f = Fields::parse(OFFLINE);
        let cx = Context {
            structure: None,
            sender: None,
            name: &names,
        };
        let text = message("StructureServicesOffline", &f, Utc::now(), &cx).unwrap();
        assert!(
            text.ends_with(": Standup Cloning Center I, type 35878."),
            "{text}"
        );
    }

    /// aa-structures' own test notifications (tests/testdata), and names.
    fn entities(id: i64) -> Option<String> {
        Some(
            match id {
                1001 => "Bruce Wayne",
                1011 => "Lex Luthor",
                2001 => "Wayne Technologies",
                2002 => "Wayne Food",
                2021 => "Quitter Corp",
                3001 => "Wayne Enterprises",
                3002 => "Justice League",
                3011 => "LexCorp",
                30000474 => "1-PGSG",
                20000345 => "Oasa Constellation",
                32226 => "Territorial Claim Unit",
                35825 => "Raitaru",
                16213 => "Caldari Control Tower",
                _ => return None,
            }
            .to_owned(),
        )
    }

    fn render(kind: &str, text: &str, sender: Option<&str>) -> String {
        let cx = Context {
            structure: None,
            sender: sender.map(str::to_owned),
            name: &entities,
        };
        message(kind, &Fields::parse(text), Utc::now(), &cx)
            .unwrap_or_else(|| panic!("{kind} has no message"))
    }

    #[test]
    fn sovereignty_names_the_holder_and_gives_a_timer() {
        let text =
            "campaignEventType: 1\ndecloakTime: 131897990021334067\nsolarSystemID: 30000474\n";
        let message = render("SovStructureReinforced", text, Some("Wayne Enterprises"));
        assert!(
            message.starts_with(
                "Sovereignty structure reinforced: the Territorial Claim Unit in 1-PGSG belonging \
                 to Wayne Enterprises was reinforced by hostile forces."
            ),
            "{message}"
        );
        let (structure, at) = sov_timer("SovStructureReinforced", &Fields::parse(text)).unwrap();
        assert_eq!(structure, "TCU");
        assert!(message.contains(&eve(at)), "{message}");
        assert!(Fields::parse(text).ids().contains(&32226));
        assert!(alliance_level("SovStructureReinforced"));
        assert_eq!(category("SovStructureReinforced"), Some(Category::Sov));
        assert_eq!(severity("SovStructureReinforced"), Severity::Danger);
        let nodes = render(
            "SovCommandNodeEventStarted",
            "campaignEventType: 1\nconstellationID: 20000345\nsolarSystemID: 30000474\n",
            None,
        );
        assert!(nodes.contains("belonging to (unknown)"), "{nodes}");
        assert!(
            nodes.contains("throughout the Oasa Constellation constellation"),
            "{nodes}"
        );
    }

    #[test]
    fn wars_name_both_sides() {
        let declared = render(
            "WarDeclared",
            "againstID: 3001\ncost: 100000000\ndeclaredByID: 3011\ndelayHours: 24\n\
             hostileState: false\ntimeStarted: 132192693000000000\n\
             warHQ: <b>Amamake - Test Structure Alpha</b>\nwarHQ_IdType:\n- 1000000000001\n- 35835\n",
            None,
        );
        assert_eq!(
            declared,
            "War declared: LexCorp declared war on Wayne Enterprises with Amamake - Test Structure \
             Alpha as war headquarters. Within 24 hours fighting can legally occur between those \
             involved."
        );
        let surrender = render(
            "WarSurrenderOfferMsg",
            "iskValue: 10000000.0\nownerID1: 3001\nownerID2: 3011\nwarNegotiationID: 1234567\n",
            None,
        );
        assert!(surrender.contains("for 10,000,000 ISK"), "{surrender}");
        assert!(
            !Fields::parse("warNegotiationID: 1234567\n")
                .ids()
                .contains(&1234567)
        );
        assert!(render("CorpBecameWarEligible", "{}\n", None).starts_with("Eligible for war"));
        assert!(!alliance_level("DeclareWar"));
        assert_eq!(category("DeclareWar"), Some(Category::War));
    }

    #[test]
    fn members_and_projects_quote_what_players_wrote() {
        let new = render(
            "CorpAppNewMsg",
            "applicationText: 'Hi [there](https://x)'\ncharID: 1011\ncorpID: 2001\n",
            None,
        );
        assert_eq!(
            new,
            "New application: Lex Luthor applied to join Wayne Technologies.\n\
             > Hi \\[there\\]\\(https\\://x\\)"
        );
        let custom = render(
            "CorpAppRejectCustomMsg",
            "applicationText: example1\ncharID: 1011\ncorpID: 2001\ncustomMessage: example2\n",
            None,
        );
        assert!(
            custom.ends_with("\n> example1\nThe reply:\n> example2"),
            "{custom}"
        );
        let goal = render(
            "CorporationGoalClosed",
            "closer_id: 1011\ncorporation_id: 2001\ncreator_id: 1001\n\
             goal_id: 287804106856621566338600488094573709306\ngoal_name: Strawberry Jam\n",
            None,
        );
        assert_eq!(
            goal,
            "Project closed: Lex Luthor closed the project Strawberry Jam; it takes no further \
             contributions."
        );
        assert_eq!(category("CharLeftCorpMsg"), Some(Category::Corp));
        assert!(!structure_related("CharLeftCorpMsg"));
    }

    #[test]
    fn reinforcement_changes_list_their_structures() {
        let text = "allStructureInfo:\n- - 1000000000001\n  - Hadozeko - 56 LARGE SHIPS\n  - 35825\n\
                    hour: 19\nnumStructures: 1\ntimestamp: 132141703753688216\nweekday: 255\n";
        let f = Fields::parse(text);
        assert_eq!(
            f.nested("allStructureInfo"),
            [vec![
                "1000000000001".to_owned(),
                "Hadozeko - 56 LARGE SHIPS".to_owned(),
                "35825".to_owned()
            ]]
        );
        assert!(f.ids().contains(&35825));
        let message = render("StructuresReinforcementChanged", text, None);
        assert!(
            message.starts_with(
                "Reinforcement hour changed to 19:00 for Hadozeko - 56 LARGE SHIPS (Raitaru)."
            ),
            "{message}"
        );
        assert!(!structure_related("StructuresReinforcementChanged"));
        assert!(structure_related("OwnershipTransferred"));
        let bill = render(
            "CorpAllBillMsg",
            "amount: 6000000\nbillTypeID: 5\ncreditorID: 2011\ncurrentDate: 133462502887835953\n\
             debtorID: 2001\ndueDate: 133488422887817240\nexternalID: 3001\nexternalID2: -1\n",
            None,
        );
        assert!(
            bill.starts_with("Bill issued: a bill of 6,000,000 ISK"),
            "{bill}"
        );
        assert!(
            bill.contains("owed by Wayne Technologies to someone"),
            "{bill}"
        );
    }

    #[test]
    fn player_text_makes_no_markup_or_links() {
        assert_eq!(escape("a_b*c~d|e\\f"), "a\\_b\\*c\\~d\\|e\\\\f");
        let new = render(
            "CorpAppNewMsg",
            "applicationText: '# Hi https://evil.example'\ncharID: 1011\ncorpID: 2001\n",
            None,
        );
        assert!(new.ends_with("\n> \\# Hi https\\://evil.example"), "{new}");
        assert_eq!(clip("abcdef", 4), "abc…");
        assert_eq!(clip("abc", 4), "abc");
        assert!(names_sender("SovStructureDestroyed"));
        assert!(!names_sender("CorpAppNewMsg"));
    }

    #[test]
    fn every_esi_type_has_a_message() {
        let cx = Context {
            structure: None,
            sender: None,
            name: &entities,
        };
        for (kind, _, _, _) in TYPES {
            if GENERATED.contains(&kind) {
                continue;
            }
            assert!(
                message(kind, &Fields::parse("{}\n"), Utc::now(), &cx).is_some(),
                "{kind}"
            );
        }
    }
}
