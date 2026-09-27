//! EFT text, as EVE's "Copy to Clipboard" and Pyfa write it:
//!
//! ```text
//! [Rifter, Fast Tackle]
//! Damage Control II
//! [Empty Low slot]
//!
//! 5MN Microwarpdrive II
//! Warp Scrambler II /OFFLINE
//!
//! 200mm AutoCannon II, Republic Fleet EMP S
//!
//! Small Polycarbon Engine Housing I
//!
//!
//! Warrior II x3
//!
//! Nanite Repair Paste x50
//! ```
//!
//! The header names the hull and the fit. Sections, split by blank lines,
//! come in EVE's order: low, mid and high slots, rigs, then subsystems (a
//! Strategic Cruiser's) or services (a structure's); then the bays, whose
//! lines all carry a quantity (drones, fighters, cargo); Pyfa puts implants
//! and boosters, without quantities, among them. A section with an
//! `[Empty Med slot]` line is that slot's, whatever its place. Where an
//! item goes after the rigs is settled by its category once it's looked up
//! (see `fit::placement`); the section order is only the first guess.

/// Where the EFT's section order puts a line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Slot {
    Low,
    Mid,
    High,
    Rig,
    Subsystem,
    Service,
    /// A section whose lines all have a quantity: drones, fighters, cargo.
    Bay,
    /// A section without quantities after the bays (Pyfa's implants and
    /// boosters), or past the six slot sections.
    Other,
}

/// The slot sections, in EFT order.
const ORDER: [Slot; 6] = [
    Slot::Low,
    Slot::Mid,
    Slot::High,
    Slot::Rig,
    Slot::Subsystem,
    Slot::Service,
];

impl Slot {
    pub fn as_str(self) -> &'static str {
        match self {
            Slot::Low => "low",
            Slot::Mid => "mid",
            Slot::High => "high",
            Slot::Rig => "rig",
            Slot::Subsystem => "subsystem",
            Slot::Service => "service",
            Slot::Bay => "bay",
            Slot::Other => "other",
        }
    }

    pub fn parse(text: &str) -> Option<Slot> {
        [ORDER.as_slice(), &[Slot::Bay, Slot::Other]]
            .concat()
            .into_iter()
            .find(|s| s.as_str() == text)
    }

    /// The slot an `[Empty ... slot]` line names.
    fn of_placeholder(line: &str) -> Option<Slot> {
        let inner = line.strip_prefix('[')?.strip_suffix(']')?.trim();
        let lower = inner.to_ascii_lowercase();
        let kind = lower.strip_prefix("empty ")?.strip_suffix(" slot")?.trim();
        match kind {
            "low" => Some(Slot::Low),
            "med" | "mid" | "medium" => Some(Slot::Mid),
            "high" => Some(Slot::High),
            "rig" => Some(Slot::Rig),
            "subsystem" => Some(Slot::Subsystem),
            "service" => Some(Slot::Service),
            _ => None,
        }
    }
}

/// One line naming an item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    /// Its line in the text, from 1.
    pub line: usize,
    pub name: String,
    /// The charge loaded in it (`Module, Charge`).
    pub charge: Option<String>,
    pub quantity: i64,
    /// Marked `/OFFLINE` (Pyfa).
    pub offline: bool,
    pub slot: Slot,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Eft {
    pub hull: String,
    pub name: String,
    pub items: Vec<Item>,
}

/// What's wrong with a line (0: the text as a whole).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub line: usize,
    pub text: String,
}

impl Problem {
    fn new(line: usize, text: impl Into<String>) -> Self {
        Self {
            line,
            text: text.into(),
        }
    }
}

/// Item lines in one fit, at most (a big cargo list fits well within).
pub const MAX_ITEMS: usize = 300;
/// The most of one item a line may give.
pub const MAX_QUANTITY: i64 = 1_000_000_000;
/// A name, at most (ESI's limit for a name to look up).
pub const MAX_NAME: usize = 100;
/// Problems reported, at most.
const MAX_PROBLEMS: usize = 20;

/// The text with its line breaks as `\n`, without a byte-order mark or
/// blank lines around it: what is stored and shown.
pub fn normalise(text: &str) -> String {
    text.trim_start_matches('\u{feff}')
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
        .trim_matches('\n')
        .to_owned()
}

/// The first line: `[Hull, Fit name]` (the name may hold commas).
fn header(line: &str) -> Option<(String, String)> {
    let inner = line.strip_prefix('[')?.strip_suffix(']')?;
    let (hull, name) = inner.split_once(',')?;
    Some((hull.trim().to_owned(), name.trim().to_owned()))
}

/// A line naming an item, without its slot yet.
struct Entry {
    line: usize,
    name: String,
    charge: Option<String>,
    quantity: Option<i64>,
    offline: bool,
}

/// `x5` (the last word of a bay's line).
fn quantity(word: &str) -> Option<&str> {
    let digits = word.strip_prefix('x').or_else(|| word.strip_prefix('X'))?;
    (!digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())).then_some(digits)
}

/// A mutated module's reference, as Pyfa writes it: `Name [1]`.
fn strip_mutation(text: &str) -> &str {
    if let Some(open) = text.rfind(" [")
        && let Some(inner) = text[open + 2..].strip_suffix(']')
        && !inner.is_empty()
        && inner.bytes().all(|b| b.is_ascii_digit())
    {
        return text[..open].trim_end();
    }
    text
}

fn entry(number: usize, line: &str) -> Result<Entry, Problem> {
    let mut text = line;
    let mut offline = false;
    let lower = text.to_ascii_lowercase();
    if lower.ends_with("/offline") {
        offline = true;
        text = text[..text.len() - "/offline".len()].trim_end();
    }
    text = strip_mutation(text);
    let mut amount = None;
    if let Some((rest, last)) = text.rsplit_once(char::is_whitespace)
        && let Some(digits) = quantity(last)
    {
        let n: i64 = digits
            .parse()
            .ok()
            .filter(|n| (1..=MAX_QUANTITY).contains(n))
            .ok_or_else(|| {
                Problem::new(
                    number,
                    format!("\"{last}\" isn't a quantity from x1 to x{MAX_QUANTITY}."),
                )
            })?;
        amount = Some(n);
        text = rest.trim_end();
    }
    let (name, charge) = match text.split_once(',') {
        Some((module, charge)) => {
            let charge = charge.trim();
            (
                module.trim(),
                (!charge.is_empty()).then(|| charge.to_owned()),
            )
        }
        None => (text.trim(), None),
    };
    if name.is_empty() {
        return Err(Problem::new(number, "This line names no item."));
    }
    for part in std::iter::once(name).chain(charge.as_deref()) {
        if part.chars().any(char::is_control) {
            return Err(Problem::new(
                number,
                "This line has a control character in it.",
            ));
        }
        if part.chars().count() > MAX_NAME {
            return Err(Problem::new(
                number,
                format!("\"{part}\" is too long to be an item's name."),
            ));
        }
    }
    Ok(Entry {
        line: number,
        name: name.to_owned(),
        charge,
        quantity: amount,
        offline,
    })
}

/// Reads EFT text, or says every line that's wrong (up to 20).
pub fn parse(text: &str) -> Result<Eft, Vec<Problem>> {
    let text = normalise(text);
    let lines: Vec<(usize, &str)> = text
        .lines()
        .enumerate()
        .map(|(i, l)| (i + 1, l.trim()))
        .collect();
    let Some(&(first, head)) = lines.first() else {
        return Err(vec![Problem::new(0, "Paste a fit in EFT format.")]);
    };
    let Some((hull, name)) = header(head) else {
        return Err(vec![Problem::new(
            first,
            "The first line must be the fit's header: [Hull, Fit name].",
        )]);
    };
    let mut problems = Vec::new();
    if hull.is_empty() {
        problems.push(Problem::new(first, "The header names no hull."));
    }
    if name.is_empty() {
        problems.push(Problem::new(
            first,
            "The header has no fit name: [Hull, Fit name].",
        ));
    }
    if hull.chars().chain(name.chars()).any(char::is_control) {
        problems.push(Problem::new(
            first,
            "The header has a control character in it.",
        ));
    }
    if hull.chars().count() > MAX_NAME || name.chars().count() > MAX_NAME {
        problems.push(Problem::new(
            first,
            format!("The hull and the fit name are at most {MAX_NAME} characters each."),
        ));
    }

    // Sections, split by blank lines.
    let mut sections: Vec<Vec<(usize, &str)>> = Vec::new();
    let mut current = Vec::new();
    for &(number, line) in &lines[1..] {
        if line.is_empty() {
            if !current.is_empty() {
                sections.push(std::mem::take(&mut current));
            }
        } else {
            current.push((number, line));
        }
    }
    if !current.is_empty() {
        sections.push(current);
    }

    let mut items = Vec::new();
    let mut next_slot = 0usize;
    let mut after_bays = false;
    for section in sections {
        // Pyfa's mutated modules: "[1] Name" then the base module, the
        // mutaplasmid and the attributes. Not items of the fit.
        if section.first().is_some_and(|(_, l)| mutation_block(l)) {
            continue;
        }
        let mut placeholder = None;
        let mut entries = Vec::new();
        for &(number, line) in &section {
            if line.starts_with('[') {
                match Slot::of_placeholder(line) {
                    Some(slot) => {
                        placeholder.get_or_insert(slot);
                    }
                    None => problems.push(Problem::new(
                        number,
                        "Only the first line is a header: paste one fit at a time.",
                    )),
                }
                continue;
            }
            match entry(number, line) {
                Ok(e) => entries.push(e),
                Err(p) => problems.push(p),
            }
        }
        let bay = placeholder.is_none()
            && !entries.is_empty()
            && entries.iter().all(|e| e.quantity.is_some());
        let slot = if bay {
            after_bays = true;
            Slot::Bay
        } else if let Some(slot) = placeholder {
            if let Some(i) = ORDER.iter().position(|s| *s == slot) {
                next_slot = i + 1;
            }
            slot
        } else if after_bays || next_slot >= ORDER.len() {
            Slot::Other
        } else {
            next_slot += 1;
            ORDER[next_slot - 1]
        };
        items.extend(entries.into_iter().map(|e| Item {
            line: e.line,
            name: e.name,
            charge: e.charge,
            quantity: e.quantity.unwrap_or(1),
            offline: e.offline,
            slot,
        }));
    }
    if items.len() > MAX_ITEMS {
        problems.push(Problem::new(
            0,
            format!("A fit has at most {MAX_ITEMS} item lines."),
        ));
    }
    if problems.is_empty() {
        Ok(Eft { hull, name, items })
    } else {
        problems.truncate(MAX_PROBLEMS);
        Err(problems)
    }
}

/// `[1] ...`: the start of one of Pyfa's mutated module blocks.
fn mutation_block(line: &str) -> bool {
    line.strip_prefix('[')
        .and_then(|rest| rest.split_once(']'))
        .is_some_and(|(n, _)| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn slots(eft: &Eft) -> Vec<(&str, Slot)> {
        eft.items
            .iter()
            .map(|i| (i.name.as_str(), i.slot))
            .collect()
    }

    #[test]
    fn an_in_game_export_is_read_by_section() {
        let eft = parse(
            "[Rifter, Fast Tackle]\r\n\
             Damage Control II\r\n\
             [Empty Low slot]\r\n\
             \r\n\
             5MN Microwarpdrive II\r\n\
             Warp Scrambler II\r\n\
             \r\n\
             200mm AutoCannon II, Republic Fleet EMP S\r\n\
             200mm AutoCannon II, Republic Fleet EMP S\r\n\
             [Empty High slot]\r\n\
             \r\n\
             Small Polycarbon Engine Housing I\r\n\
             \r\n\
             \r\n\
             Warrior II x3\r\n\
             \r\n\
             Nanite Repair Paste x50\r\n",
        )
        .unwrap();
        assert_eq!(eft.hull, "Rifter");
        assert_eq!(eft.name, "Fast Tackle");
        assert_eq!(
            slots(&eft),
            vec![
                ("Damage Control II", Slot::Low),
                ("5MN Microwarpdrive II", Slot::Mid),
                ("Warp Scrambler II", Slot::Mid),
                ("200mm AutoCannon II", Slot::High),
                ("200mm AutoCannon II", Slot::High),
                ("Small Polycarbon Engine Housing I", Slot::Rig),
                ("Warrior II", Slot::Bay),
                ("Nanite Repair Paste", Slot::Bay),
            ]
        );
        let gun = &eft.items[3];
        assert_eq!(gun.charge.as_deref(), Some("Republic Fleet EMP S"));
        assert_eq!(gun.line, 8);
        assert_eq!(eft.items[6].quantity, 3);
        assert_eq!(eft.items[7].quantity, 50);
        assert!(eft.items.iter().all(|i| !i.offline));
    }

    #[test]
    fn empty_slot_lines_say_whose_section_it_is() {
        // No mid slot modules at all: the placeholders keep the highs
        // from being read as mids.
        let eft = parse(
            "[Catalyst, Gank]\n\
             Magnetic Field Stabilizer II\n\n\
             [Empty Med slot]\n[Empty Med slot]\n\n\
             Light Neutron Blaster II, Void S\n\n\
             [Empty Rig slot]\n",
        )
        .unwrap();
        assert_eq!(
            slots(&eft),
            vec![
                ("Magnetic Field Stabilizer II", Slot::Low),
                ("Light Neutron Blaster II", Slot::High),
            ]
        );
        // An empty section can come alone, out of order.
        let eft = parse("[Rifter, Odd]\n[Empty High slot]\n\nGyrostabilizer II").unwrap();
        assert_eq!(slots(&eft), vec![("Gyrostabilizer II", Slot::Rig)]);
    }

    #[test]
    fn offline_quantities_and_charges() {
        let eft = parse(
            "[Rifter, Test]\n\
             Damage Control II /OFFLINE\n\n\
             Warp Scrambler II /offline\n\n\
             200mm AutoCannon II, Republic Fleet EMP S /OFFLINE\n\
             Rocket Launcher II,\n\n\n\
             Hobgoblin II x5\n\
             Nanite Repair Paste X100\n",
        )
        .unwrap();
        let [dc, scram, gun, launcher, drones, paste] = eft.items.as_slice() else {
            panic!("{:?}", eft.items);
        };
        assert!(dc.offline && scram.offline && gun.offline && !launcher.offline);
        assert_eq!(gun.charge.as_deref(), Some("Republic Fleet EMP S"));
        // A comma with no charge after it.
        assert_eq!(launcher.name, "Rocket Launcher II");
        assert_eq!(launcher.charge, None);
        assert_eq!((drones.quantity, drones.slot), (5, Slot::Bay));
        assert_eq!((paste.quantity, paste.slot), (100, Slot::Bay));
        // A word that only looks like a quantity is part of the name.
        let eft = parse("[Rifter, Test]\n\nFoo xl\n").unwrap();
        assert_eq!(eft.items[0].name, "Foo xl");
        assert_eq!(eft.items[0].slot, Slot::Low);
    }

    #[test]
    fn strategic_cruisers_have_subsystems_after_the_rigs() {
        let eft = parse(
            "[Loki, Fleet]\n\
             Damage Control II\n\n\
             10MN Afterburner II\n\n\
             Heavy Assault Missile Launcher II, Scourge Rage Heavy Assault Missile\n\n\
             Medium Core Defense Field Extender I\n\n\
             Loki Core - Augmented Nuclear Reactor\n\
             Loki Defensive - Covert Reconfiguration\n\
             Loki Offensive - Launcher Efficiency Configuration\n\
             Loki Propulsion - Wake Limiter\n\n\
             Hornet EC-300 x5\n",
        )
        .unwrap();
        let subsystems: Vec<_> = eft
            .items
            .iter()
            .filter(|i| i.slot == Slot::Subsystem)
            .map(|i| i.name.as_str())
            .collect();
        assert_eq!(subsystems.len(), 4);
        assert_eq!(subsystems[0], "Loki Core - Augmented Nuclear Reactor");
        assert_eq!(eft.items.last().unwrap().slot, Slot::Bay);
    }

    #[test]
    fn pyfa_implants_and_mutated_modules() {
        let eft = parse(
            "[Rifter, Pyfa]\n\
             Entropic Radiation Sink II [1]\n\n\
             [Empty Med slot]\n\n\
             [Empty High slot]\n\n\
             [Empty Rig slot]\n\n\n\
             Warrior II x2\n\n\
             Zainou 'Deadeye' Small Projectile Turret SP-601\n\n\
             Agency 'Pyrolancea' DB5 Dose II\n\n\
             Nanite Repair Paste x10\n\n\n\
             [1] Entropic Radiation Sink II\n\
             \x20 Unstable Entropic Radiation Sink Mutaplasmid\n\
             \x20 cpu 23.5, damageMultiplier 1.12\n",
        )
        .unwrap();
        assert_eq!(
            slots(&eft),
            vec![
                ("Entropic Radiation Sink II", Slot::Low),
                ("Warrior II", Slot::Bay),
                (
                    "Zainou 'Deadeye' Small Projectile Turret SP-601",
                    Slot::Other
                ),
                ("Agency 'Pyrolancea' DB5 Dose II", Slot::Other),
                ("Nanite Repair Paste", Slot::Bay),
            ]
        );
    }

    #[test]
    fn the_header_names_hull_and_fit() {
        let eft = parse("\n\n[ Rifter ,  Fast, cheap ]\n").unwrap();
        assert_eq!(
            (eft.hull.as_str(), eft.name.as_str()),
            ("Rifter", "Fast, cheap")
        );
        assert!(eft.items.is_empty());
        for (text, problem) in [
            ("", "Paste a fit"),
            ("Damage Control II", "first line must be the fit's header"),
            ("[Rifter]", "first line must be the fit's header"),
            ("[Rifter, ]", "no fit name"),
            ("[, Name]", "no hull"),
        ] {
            let problems = parse(text).unwrap_err();
            assert!(problems[0].text.contains(problem), "{text:?}: {problems:?}");
        }
    }

    #[test]
    fn every_bad_line_is_reported() {
        let problems = parse(
            "[Rifter, Two]\n\
             Damage Control II\n\
             , Republic Fleet EMP S\n\n\
             [Rifter, Another fit]\n\n\
             Warrior II x0\n\
             Warrior II x99999999999\n",
        )
        .unwrap_err();
        let lines: Vec<usize> = problems.iter().map(|p| p.line).collect();
        assert_eq!(lines, vec![3, 5, 7, 8], "{problems:?}");
        assert!(problems[1].text.contains("one fit at a time"));
        let long = format!("[Rifter, Long]\n{}", "x".repeat(MAX_NAME + 1));
        assert!(parse(&long).unwrap_err()[0].text.contains("too long"));
        let tab = parse("[Rifter, Tab]\nDamage\tControl II").unwrap_err();
        assert!(tab[0].text.contains("control character"), "{tab:?}");
        let many = format!(
            "[Rifter, Many]\n{}",
            "Warrior II x1\n".repeat(MAX_ITEMS + 1)
        );
        assert!(parse(&many).unwrap_err()[0].text.contains("at most"));
    }

    #[test]
    fn slots_round_trip_and_text_is_normalised() {
        for slot in ORDER.iter().chain(&[Slot::Bay, Slot::Other]) {
            assert_eq!(Slot::parse(slot.as_str()), Some(*slot));
        }
        assert_eq!(Slot::parse("cargo"), None);
        assert_eq!(normalise("\u{feff}\n[Rifter, A]  \n\n"), "[Rifter, A]");
    }
}
