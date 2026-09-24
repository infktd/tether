//! Access tiers (F4): Member, Allied or Guest, from the main's affiliation.

use std::collections::HashSet;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tier {
    Member,
    Allied,
    Guest,
}

impl Tier {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Member => "member",
            Self::Allied => "allied",
            Self::Guest => "guest",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "member" => Some(Self::Member),
            "allied" => Some(Self::Allied),
            "guest" => Some(Self::Guest),
            _ => None,
        }
    }
}

impl fmt::Display for Tier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // `pad` honours width and alignment, so tiers line up in tables.
        f.pad(self.as_str())
    }
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
}

impl EntityKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Alliance => "alliance",
            Self::Corporation => "corporation",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "alliance" => Some(Self::Alliance),
            "corporation" => Some(Self::Corporation),
            _ => None,
        }
    }
}

/// The alliances and corporations that grant Member or Allied.
#[derive(Debug, Clone, Default)]
pub struct TierRules {
    member: HashSet<(EntityKind, i64)>,
    allied: HashSet<(EntityKind, i64)>,
}

impl TierRules {
    /// Adds a rule. `Tier::Guest` is the default and needs no rule.
    pub fn add(&mut self, kind: EntityKind, entity_id: i64, tier: Tier) {
        match tier {
            Tier::Member => {
                self.member.insert((kind, entity_id));
            }
            Tier::Allied => {
                self.allied.insert((kind, entity_id));
            }
            Tier::Guest => {}
        }
    }

    /// Member if the main's alliance or corporation is listed as Member,
    /// else Allied if listed as Allied, else Guest. An unknown affiliation
    /// is Guest.
    pub fn evaluate(&self, main: Option<Affiliation>) -> Tier {
        let Some(main) = main else {
            return Tier::Guest;
        };
        let matches = |set: &HashSet<(EntityKind, i64)>| {
            set.contains(&(EntityKind::Corporation, main.corporation_id))
                || main
                    .alliance_id
                    .is_some_and(|a| set.contains(&(EntityKind::Alliance, a)))
        };
        if matches(&self.member) {
            Tier::Member
        } else if matches(&self.allied) {
            Tier::Allied
        } else {
            Tier::Guest
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NMU: i64 = 99_000_001;
    const NMU_CORP: i64 = 98_000_001;
    const BLUE: i64 = 99_000_002;
    const BLUE_CORP: i64 = 98_000_002;

    fn rules() -> TierRules {
        let mut rules = TierRules::default();
        rules.add(EntityKind::Alliance, NMU, Tier::Member);
        rules.add(EntityKind::Corporation, NMU_CORP, Tier::Member);
        rules.add(EntityKind::Alliance, BLUE, Tier::Allied);
        rules.add(EntityKind::Corporation, BLUE_CORP, Tier::Allied);
        rules
    }

    fn aff(corporation_id: i64, alliance_id: Option<i64>) -> Option<Affiliation> {
        Some(Affiliation {
            corporation_id,
            alliance_id,
        })
    }

    #[test]
    fn member_by_alliance_or_corporation() {
        assert_eq!(rules().evaluate(aff(1, Some(NMU))), Tier::Member);
        assert_eq!(rules().evaluate(aff(NMU_CORP, None)), Tier::Member);
    }

    #[test]
    fn allied_by_alliance_or_corporation() {
        assert_eq!(rules().evaluate(aff(1, Some(BLUE))), Tier::Allied);
        assert_eq!(rules().evaluate(aff(BLUE_CORP, Some(5))), Tier::Allied);
    }

    #[test]
    fn member_wins_over_allied() {
        // A Member corporation inside an Allied alliance.
        assert_eq!(rules().evaluate(aff(NMU_CORP, Some(BLUE))), Tier::Member);
    }

    #[test]
    fn everyone_else_is_guest() {
        assert_eq!(rules().evaluate(aff(1_000_167, None)), Tier::Guest);
        assert_eq!(rules().evaluate(aff(1, Some(2))), Tier::Guest);
        assert_eq!(rules().evaluate(None), Tier::Guest);
        assert_eq!(
            TierRules::default().evaluate(aff(NMU_CORP, Some(NMU))),
            Tier::Guest
        );
    }

    #[test]
    fn display_honours_padding() {
        assert_eq!(format!("[{:<7}]", Tier::Member), "[member ]");
    }

    #[test]
    fn corporation_rule_does_not_match_an_alliance_with_the_same_number() {
        let mut rules = TierRules::default();
        rules.add(EntityKind::Corporation, 42, Tier::Member);
        assert_eq!(rules.evaluate(aff(1, Some(42))), Tier::Guest);
    }
}
