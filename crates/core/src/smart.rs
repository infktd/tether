//! Secure Groups' filters (aa-securegroups, AA's "Smart Groups"): what an
//! account must pass to be in a group. Every filter must pass; a reversed
//! filter must fail. Pure: the facts come from the database.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

/// One filter, as stored (`kind` and `config`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "config", rename_all = "snake_case")]
pub enum Filter {
    /// The account's state is one of these.
    State { states: Vec<i64> },
    /// The main's corporation or alliance is one of these.
    MainAffiliation { entities: Vec<i64> },
    /// Any of the account's characters is in one of these corporations or
    /// alliances.
    AnyAffiliation { entities: Vec<i64> },
    /// The main is at least this many days old.
    CharacterAge { days: u32 },
    /// In all (or any) of these groups.
    Groups { groups: Vec<i64>, all: bool },
    /// Every character registered with the state's scopes.
    Compliant {},
}

impl Filter {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::State { .. } => "state",
            Self::MainAffiliation { .. } => "main_affiliation",
            Self::AnyAffiliation { .. } => "any_affiliation",
            Self::CharacterAge { .. } => "character_age",
            Self::Groups { .. } => "groups",
            Self::Compliant {} => "compliant",
        }
    }

    /// Whether the facts pass it (before reversing).
    pub fn passes(&self, facts: &Facts) -> bool {
        match self {
            Self::State { states } => states.contains(&facts.state),
            Self::MainAffiliation { entities } => facts
                .main_affiliation
                .iter()
                .any(|id| entities.contains(id)),
            Self::AnyAffiliation { entities } => {
                facts.affiliations.iter().any(|id| entities.contains(id))
            }
            Self::CharacterAge { days } => facts
                .main_age_days
                .is_some_and(|age| age >= i64::from(*days)),
            Self::Groups { groups, all } => {
                if *all {
                    groups.iter().all(|g| facts.groups.contains(g))
                } else {
                    groups.iter().any(|g| facts.groups.contains(g))
                }
            }
            Self::Compliant {} => facts.compliant,
        }
    }
}

/// What the filters look at, for one account.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Facts {
    pub state: i64,
    /// The main's corporation and alliance.
    pub main_affiliation: Vec<i64>,
    /// Every character's corporation and alliance.
    pub affiliations: BTreeSet<i64>,
    /// `None` until ESI has told us the main's birthday.
    pub main_age_days: Option<i64>,
    pub groups: BTreeSet<i64>,
    pub compliant: bool,
}

/// A stored filter with its reversal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    pub id: i64,
    pub filter: Filter,
    pub reversed: bool,
}

impl Rule {
    pub fn passes(&self, facts: &Facts) -> bool {
        self.filter.passes(facts) != self.reversed
    }
}

/// The rules an account fails; none means it may be in the group.
pub fn failing<'a>(rules: &'a [Rule], facts: &Facts) -> Vec<&'a Rule> {
    rules.iter().filter(|r| !r.passes(facts)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts() -> Facts {
        Facts {
            state: 1,
            main_affiliation: vec![100, 200],
            affiliations: [100, 200, 300].into(),
            main_age_days: Some(400),
            groups: [7].into(),
            compliant: true,
        }
    }

    fn rule(filter: Filter, reversed: bool) -> Rule {
        Rule {
            id: 1,
            filter,
            reversed,
        }
    }

    #[test]
    fn filters_pass_and_reverse() {
        let f = facts();
        assert!(Filter::State { states: vec![1, 2] }.passes(&f));
        assert!(
            Filter::MainAffiliation {
                entities: vec![200]
            }
            .passes(&f)
        );
        assert!(
            !Filter::MainAffiliation {
                entities: vec![300]
            }
            .passes(&f)
        );
        assert!(
            Filter::AnyAffiliation {
                entities: vec![300]
            }
            .passes(&f)
        );
        assert!(Filter::CharacterAge { days: 365 }.passes(&f));
        assert!(!Filter::CharacterAge { days: 500 }.passes(&f));
        assert!(
            Filter::Groups {
                groups: vec![7, 8],
                all: false
            }
            .passes(&f)
        );
        assert!(
            !Filter::Groups {
                groups: vec![7, 8],
                all: true
            }
            .passes(&f)
        );
        assert!(Filter::Compliant {}.passes(&f));
        // Reversed: an alt in a hostile corporation keeps you out.
        let rules = vec![rule(
            Filter::AnyAffiliation {
                entities: vec![300],
            },
            true,
        )];
        assert_eq!(failing(&rules, &f).len(), 1);
        // Unknown age never passes an age filter.
        let unknown = Facts {
            main_age_days: None,
            ..facts()
        };
        assert!(!Filter::CharacterAge { days: 1 }.passes(&unknown));
    }

    #[test]
    fn filters_round_trip_as_stored() {
        let filter = Filter::Groups {
            groups: vec![3],
            all: true,
        };
        let json = serde_json::to_value(&filter).unwrap();
        assert_eq!(json["kind"], "groups");
        assert_eq!(json["config"]["groups"][0], 3);
        assert_eq!(serde_json::from_value::<Filter>(json).unwrap(), filter);
        let compliant = serde_json::to_value(Filter::Compliant {}).unwrap();
        assert_eq!(
            serde_json::from_value::<Filter>(compliant).unwrap(),
            Filter::Compliant {}
        );
    }
}
