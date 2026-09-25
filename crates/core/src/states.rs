//! Access states, Alliance Auth style (F4): Member, Blue and Guest built
//! in, plus any an admin creates. Each lists the alliances, corporations
//! and characters it covers; an account's state is the highest-priority
//! state that covers its main, else Guest.

use std::collections::{BTreeMap, HashSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct StateId(pub i64);

/// The states every instance has. They can't be renamed or deleted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Builtin {
    Member,
    Blue,
    Guest,
}

impl Builtin {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Member => "member",
            Self::Blue => "blue",
            Self::Guest => "guest",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "member" => Some(Self::Member),
            "blue" => Some(Self::Blue),
            "guest" => Some(Self::Guest),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct State {
    pub id: StateId,
    pub name: String,
    pub builtin: Option<Builtin>,
    /// Higher wins. Guest is always 0.
    pub priority: i32,
}

impl State {
    pub fn is_guest(&self) -> bool {
        self.builtin == Some(Builtin::Guest)
    }

    /// For badges: `member`, `blue`, `guest` or `custom`.
    pub fn style(&self) -> &'static str {
        self.builtin.map_or("custom", Builtin::as_str)
    }
}

/// Longest state name.
pub const MAX_NAME: usize = 40;

/// Checks a state name an admin typed; returns it trimmed.
pub fn check_name(name: &str) -> Result<&str, &'static str> {
    let name = name.trim();
    if name.is_empty() {
        return Err("Give the state a name.");
    }
    if name.chars().count() > MAX_NAME {
        return Err("State names are at most 40 characters.");
    }
    if name.chars().any(char::is_control) {
        return Err("State names can't contain control characters.");
    }
    Ok(name)
}

/// A character's corporation and, if any, alliance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Affiliation {
    pub corporation_id: i64,
    pub alliance_id: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EntityKind {
    Alliance,
    Corporation,
    Character,
}

impl EntityKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Alliance => "alliance",
            Self::Corporation => "corporation",
            Self::Character => "character",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "alliance" => Some(Self::Alliance),
            "corporation" => Some(Self::Corporation),
            "character" => Some(Self::Character),
            _ => None,
        }
    }
}

/// What a state is matched against: the account's main.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Main {
    pub character_id: i64,
    /// `None` until ESI has been asked.
    pub affiliation: Option<Affiliation>,
}

#[derive(Debug, Clone)]
struct Rule {
    id: StateId,
    priority: i32,
    covers: HashSet<(EntityKind, i64)>,
}

/// Every state with what it covers, for evaluation.
#[derive(Debug, Clone)]
pub struct StateRules {
    guest: StateId,
    /// Highest priority first.
    rules: Vec<Rule>,
}

impl StateRules {
    pub fn new(guest: StateId) -> Self {
        Self {
            guest,
            rules: Vec::new(),
        }
    }

    pub fn guest(&self) -> StateId {
        self.guest
    }

    /// Adds a state (not Guest, which covers everyone else).
    pub fn add_state(&mut self, id: StateId, priority: i32) {
        if id == self.guest || self.rules.iter().any(|r| r.id == id) {
            return;
        }
        self.rules.push(Rule {
            id,
            priority,
            covers: HashSet::new(),
        });
        self.sort();
    }

    pub fn remove_state(&mut self, id: StateId) {
        self.rules.retain(|r| r.id != id);
    }

    pub fn set_priority(&mut self, id: StateId, priority: i32) {
        if let Some(rule) = self.rules.iter_mut().find(|r| r.id == id) {
            rule.priority = priority;
        }
        self.sort();
    }

    /// Adds an entity to a state added before.
    pub fn add(&mut self, state: StateId, kind: EntityKind, entity_id: i64) {
        if let Some(rule) = self.rules.iter_mut().find(|r| r.id == state) {
            rule.covers.insert((kind, entity_id));
        }
    }

    pub fn remove(&mut self, state: StateId, kind: EntityKind, entity_id: i64) {
        if let Some(rule) = self.rules.iter_mut().find(|r| r.id == state) {
            rule.covers.remove(&(kind, entity_id));
        }
    }

    fn sort(&mut self) {
        // Ties can't happen in the database (priorities are unique); the id
        // keeps evaluation deterministic anyway.
        self.rules
            .sort_by(|a, b| b.priority.cmp(&a.priority).then(a.id.cmp(&b.id)));
    }

    /// The highest-priority state covering the main by character,
    /// corporation or alliance; Guest if none does or there's no main.
    pub fn evaluate(&self, main: Option<Main>) -> StateId {
        let Some(main) = main else {
            return self.guest;
        };
        self.rules
            .iter()
            .find(|rule| {
                rule.covers
                    .contains(&(EntityKind::Character, main.character_id))
                    || main.affiliation.is_some_and(|a| {
                        rule.covers
                            .contains(&(EntityKind::Corporation, a.corporation_id))
                            || a.alliance_id
                                .is_some_and(|id| rule.covers.contains(&(EntityKind::Alliance, id)))
                    })
            })
            .map_or(self.guest, |rule| rule.id)
    }
}

/// How a change to the rules would move accounts: `(from, to) -> count`,
/// only for accounts that move.
pub fn impact(
    mains: &[Option<Main>],
    before: &StateRules,
    after: &StateRules,
) -> BTreeMap<(StateId, StateId), usize> {
    let mut moves = BTreeMap::new();
    for main in mains {
        let (from, to) = (before.evaluate(*main), after.evaluate(*main));
        if from != to {
            *moves.entry((from, to)).or_insert(0) += 1;
        }
    }
    moves
}

#[cfg(test)]
mod tests {
    use super::*;

    const NMU: i64 = 99_000_001;
    const NMU_CORP: i64 = 98_000_001;
    const BLUE: i64 = 99_000_002;
    const BLUE_CORP: i64 = 98_000_002;
    const PILOT: i64 = 2_100_000_001;

    const MEMBER: StateId = StateId(1);
    const BLUE_STATE: StateId = StateId(2);
    const GUEST: StateId = StateId(3);

    fn rules() -> StateRules {
        let mut rules = StateRules::new(GUEST);
        rules.add_state(BLUE_STATE, 1);
        rules.add_state(MEMBER, 2);
        rules.add(MEMBER, EntityKind::Alliance, NMU);
        rules.add(MEMBER, EntityKind::Corporation, NMU_CORP);
        rules.add(BLUE_STATE, EntityKind::Alliance, BLUE);
        rules.add(BLUE_STATE, EntityKind::Corporation, BLUE_CORP);
        rules
    }

    fn main(corporation_id: i64, alliance_id: Option<i64>) -> Option<Main> {
        Some(Main {
            character_id: PILOT,
            affiliation: Some(Affiliation {
                corporation_id,
                alliance_id,
            }),
        })
    }

    #[test]
    fn member_by_alliance_or_corporation() {
        assert_eq!(rules().evaluate(main(1, Some(NMU))), MEMBER);
        assert_eq!(rules().evaluate(main(NMU_CORP, None)), MEMBER);
    }

    #[test]
    fn blue_by_alliance_or_corporation() {
        assert_eq!(rules().evaluate(main(1, Some(BLUE))), BLUE_STATE);
        assert_eq!(rules().evaluate(main(BLUE_CORP, Some(5))), BLUE_STATE);
    }

    #[test]
    fn highest_priority_wins() {
        // A Member corporation inside a Blue alliance.
        assert_eq!(rules().evaluate(main(NMU_CORP, Some(BLUE))), MEMBER);
        let mut swapped = rules();
        swapped.set_priority(BLUE_STATE, 3);
        assert_eq!(swapped.evaluate(main(NMU_CORP, Some(BLUE))), BLUE_STATE);
    }

    #[test]
    fn a_character_is_covered_on_its_own() {
        let mut rules = rules();
        rules.add(BLUE_STATE, EntityKind::Character, PILOT);
        assert_eq!(rules.evaluate(main(1, None)), BLUE_STATE);
        // Even before ESI has told us its corporation.
        let unknown = Some(Main {
            character_id: PILOT,
            affiliation: None,
        });
        assert_eq!(rules.evaluate(unknown), BLUE_STATE);
        // Member still wins by priority.
        assert_eq!(rules.evaluate(main(NMU_CORP, None)), MEMBER);
    }

    #[test]
    fn everyone_else_is_guest() {
        assert_eq!(rules().evaluate(main(1_000_167, None)), GUEST);
        assert_eq!(rules().evaluate(main(1, Some(2))), GUEST);
        assert_eq!(rules().evaluate(None), GUEST);
        assert_eq!(
            StateRules::new(GUEST).evaluate(main(NMU_CORP, Some(NMU))),
            GUEST
        );
    }

    #[test]
    fn kinds_do_not_cross() {
        let mut rules = StateRules::new(GUEST);
        rules.add_state(MEMBER, 1);
        rules.add(MEMBER, EntityKind::Corporation, 42);
        assert_eq!(rules.evaluate(main(1, Some(42))), GUEST);
        let mut rules = StateRules::new(GUEST);
        rules.add_state(MEMBER, 1);
        rules.add(MEMBER, EntityKind::Character, 42);
        assert_eq!(rules.evaluate(main(42, None)), GUEST);
    }

    #[test]
    fn guest_takes_no_rules() {
        let mut rules = StateRules::new(GUEST);
        rules.add_state(GUEST, 5);
        rules.add(GUEST, EntityKind::Corporation, 1);
        assert_eq!(rules.evaluate(main(1, None)), GUEST);
    }

    #[test]
    fn impact_counts_only_moves() {
        let before = rules();
        let mut after = rules();
        after.remove(MEMBER, EntityKind::Corporation, NMU_CORP);
        after.add(BLUE_STATE, EntityKind::Corporation, 7);
        let mains = [
            main(NMU_CORP, None), // Member -> Guest
            main(NMU_CORP, None), // Member -> Guest
            main(1, Some(NMU)),   // stays Member
            main(7, None),        // Guest -> Blue
            None,                 // stays Guest
        ];
        let moves = impact(&mains, &before, &after);
        assert_eq!(moves.len(), 2);
        assert_eq!(moves[&(MEMBER, GUEST)], 2);
        assert_eq!(moves[&(GUEST, BLUE_STATE)], 1);
    }

    #[test]
    fn removing_a_state_moves_its_accounts_down() {
        let before = rules();
        let mut after = rules();
        after.remove_state(MEMBER);
        let moves = impact(&[main(NMU_CORP, Some(BLUE))], &before, &after);
        assert_eq!(moves[&(MEMBER, BLUE_STATE)], 1);
    }

    #[test]
    fn names_are_checked() {
        assert_eq!(check_name("  Trial  "), Ok("Trial"));
        assert!(check_name(" ").is_err());
        assert!(check_name(&"x".repeat(41)).is_err());
        assert!(check_name("a\nb").is_err());
    }
}
