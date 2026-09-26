//! EVE's structure notifications: reading their text and turning them
//! into a Discord message and, for some, a timer.
//!
//! A notification's text is YAML, but flat: `key: value` lines, with a few
//! lists (`- item` lines under a `key:` line). Values may carry an anchor
//! (`&id001 1035466617946`); list items may be aliases (`*id001`). That's
//! all that's read here, which keeps a YAML parser out of the plugin.

use std::collections::BTreeMap;

use chrono::{DateTime, Duration, Utc};

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
        for line in text.lines() {
            if line.trim().is_empty() {
                continue;
            }
            // A list item of the key above (EVE writes them unindented or
            // indented by two). Nested lists aren't read.
            if let Some(item) = line.trim_start().strip_prefix("- ") {
                if let Some(key) = &list
                    && !item.starts_with("- ")
                {
                    fields
                        .lists
                        .entry(key.clone())
                        .or_default()
                        .push(clean(item));
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

    /// Every id `/universe/names` can name: the system, the structure's
    /// type, the attacker, and the services that went offline. Moons and
    /// planets aren't among them (Structures names those itself).
    pub fn ids(&self) -> Vec<i64> {
        let mut ids: Vec<i64> = [
            self.system_id(),
            self.type_id(),
            self.int("charID"),
            self.int("aggressorID"),
            self.int("aggressorCorpID"),
            self.int("aggressorAllianceID"),
        ]
        .into_iter()
        .flatten()
        .collect();
        ids.extend(self.ints("listOfServiceModuleIDs"));
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
}

impl Category {
    pub const ALL: [Category; 4] = [
        Category::Attack,
        Category::Fuel,
        Category::State,
        Category::Moon,
    ];

    /// Its name in storage (`owner_channels.category`).
    pub fn name(self) -> &'static str {
        match self {
            Category::Attack => "attack",
            Category::Fuel => "fuel",
            Category::State => "state",
            Category::Moon => "moon",
        }
    }

    /// What a manager sees.
    pub fn label(self) -> &'static str {
        match self {
            Category::Attack => "Attacks",
            Category::Fuel => "Fuel and services",
            Category::State => "State changes",
            Category::Moon => "Moon extractions",
        }
    }
}

pub fn category(kind: &str) -> Option<Category> {
    Some(match kind {
        "StructureUnderAttack"
        | "StructureLostShields"
        | "StructureLostArmor"
        | "StructureDestroyed" => Category::Attack,
        "TowerAlertMsg" | "OrbitalAttacked" | "OrbitalReinforced" | "SkyhookUnderAttack"
        | "SkyhookLostShields" | "SkyhookDestroyed" => Category::Attack,
        "StructureFuelAlert"
        | "StructureServicesOffline"
        | "StructureWentLowPower"
        | "StructureLowReagentsAlert"
        | "StructureNoReagentsAlert"
        | "TowerResourceAlertMsg" => Category::Fuel,
        "StructureWentHighPower"
        | "StructureOnline"
        | "StructureAnchoring"
        | "StructureUnanchoring"
        | "SkyhookDeployed"
        | "SkyhookOnline" => Category::State,
        "MoonminingExtractionStarted"
        | "MoonminingExtractionFinished"
        | "MoonminingAutomaticFracture"
        | "MoonminingLaserFired"
        | "MoonminingExtractionCancelled" => Category::Moon,
        _ => return None,
    })
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

/// What the message needs to know besides the notification.
pub struct Context<'a> {
    /// The structure's name, if Structures has it.
    pub structure: Option<String>,
    /// A name for an id, if known.
    pub name: &'a dyn Fn(i64) -> Option<String>,
}

/// A player-chosen name made safe for Discord: no links or code spans
/// built from its brackets or backticks.
pub fn escape(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        if matches!(c, '[' | ']' | '(' | ')' | '`') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

fn eve(t: DateTime<Utc>) -> String {
    t.format("%Y-%m-%d %H:%M").to_string()
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
        assert_eq!(category("CorpAppNewMsg"), None);
    }

    #[test]
    fn names_cannot_make_links() {
        assert_eq!(
            escape("[Click](https://x) `x`"),
            "\\[Click\\]\\(https://x\\) \\`x\\`"
        );
        let cx = Context {
            structure: Some("[Keep](https://evil.example)".into()),
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
            name: &lookup,
        };
        let text = message("TowerAlertMsg", &tower, Utc::now(), &cx).unwrap();
        assert_eq!(
            text,
            "Starbase under attack: Home Tower (Caldari Control Tower) at Jita IV - Moon 4 in Jita \
             by Some Pilot. Shield 50%, armor 100%, hull 100%."
        );
        assert_eq!(category("TowerAlertMsg"), Some(Category::Attack));
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
            name: &names,
        };
        let text = message("StructureServicesOffline", &f, Utc::now(), &cx).unwrap();
        assert!(
            text.ends_with(": Standup Cloning Center I, type 35878."),
            "{text}"
        );
    }
}
