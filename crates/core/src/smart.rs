//! Secure Groups' filters (allianceauth-secure-groups, AA's "Smart
//! Groups"): what an account must pass to be in a group. Every filter must
//! pass; a reversed filter must fail. An expression combines two filters
//! (AND, OR or XOR, maybe negated), as AA's `FilterExpression`. Exempt
//! corporations and alliances pass a filter whatever it says, as AA's
//! alt and group filters. Pure: the facts come from the database.

use std::collections::{BTreeMap, BTreeSet};

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
    /// alliances (AA's alt corporation and alt alliance filters).
    AnyAffiliation {
        entities: Vec<i64>,
        /// Mains in these corporations or alliances pass anyway.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        exempt: Vec<i64>,
    },
    /// The main is at least this many days old.
    CharacterAge { days: u32 },
    /// In all (or any) of these groups (AA's user in group filter).
    Groups {
        groups: Vec<i64>,
        all: bool,
        /// Mains in these corporations or alliances pass anyway.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        exempt: Vec<i64>,
    },
    /// Every character registered with the state's scopes.
    Compliant {},
    /// Any of the account's characters is in one of these factions'
    /// militias (AA's alt faction filter).
    Faction { factions: Vec<i64> },
    /// The account uses a service (AA's service filters). Tether's only
    /// service is Discord.
    Service { service: Service },
    /// Two filters combined (AA's filter expression).
    Expression {
        operator: Operator,
        negate: bool,
        first: Box<Term>,
        second: Box<Term>,
    },
    /// An app's filter (Member Audit's skills, FAT's attendance): the app
    /// reports a value per character; `sum` adds an account's characters
    /// up and needs `at_least`, else any character with a value passes.
    App {
        plugin: String,
        name: String,
        /// The admin's settings, as the app gets them (JSON text).
        config: String,
        sum: bool,
        at_least: i64,
        /// What it asks, for people: "Member Audit: has skill set Capitals".
        label: String,
    },
}

/// A service an account can have (AA's service filters).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Service {
    Discord,
}

/// How an expression combines its two filters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Operator {
    And,
    Or,
    Xor,
}

impl Operator {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "and" => Some(Self::And),
            "or" => Some(Self::Or),
            "xor" => Some(Self::Xor),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::And => "AND",
            Self::Or => "OR",
            Self::Xor => "XOR",
        }
    }
}

/// One side of an expression: a filter and whether it's reversed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Term {
    pub filter: Filter,
    #[serde(default)]
    pub reversed: bool,
}

/// The key an app filter's values are kept under.
pub fn app_key(plugin: &str, name: &str, config: &str) -> String {
    format!("{plugin}\u{1f}{name}\u{1f}{config}")
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
            Self::Faction { .. } => "faction",
            Self::Service { .. } => "service",
            Self::Expression { .. } => "expression",
            Self::App { .. } => "app",
        }
    }

    /// The filters it's made of: itself, or an expression's, all the way
    /// down.
    pub fn leaves(&self) -> Vec<&Filter> {
        match self {
            Self::Expression { first, second, .. } => {
                let mut out = first.filter.leaves();
                out.extend(second.filter.leaves());
                out
            }
            leaf => vec![leaf],
        }
    }

    /// Corporations and alliances whose mains pass it anyway.
    pub fn exempt(&self) -> &[i64] {
        match self {
            Self::AnyAffiliation { exempt, .. } | Self::Groups { exempt, .. } => exempt,
            _ => &[],
        }
    }

    /// Whether the facts pass it (before reversing or exemptions).
    pub fn passes(&self, facts: &Facts) -> bool {
        match self {
            Self::State { states } => states.contains(&facts.state),
            Self::MainAffiliation { entities } => facts
                .main_affiliation
                .iter()
                .any(|id| entities.contains(id)),
            Self::AnyAffiliation { entities, .. } => {
                facts.affiliations.iter().any(|id| entities.contains(id))
            }
            Self::CharacterAge { days } => facts
                .main_age_days
                .is_some_and(|age| age >= i64::from(*days)),
            Self::Groups { groups, all, .. } => {
                if *all {
                    groups.iter().all(|g| facts.groups.contains(g))
                } else {
                    groups.iter().any(|g| facts.groups.contains(g))
                }
            }
            Self::Compliant {} => facts.compliant,
            Self::Faction { factions } => factions.iter().any(|f| facts.factions.contains(f)),
            Self::Service {
                service: Service::Discord,
            } => facts.discord,
            Self::Expression {
                operator,
                negate,
                first,
                second,
            } => {
                let (a, b) = (
                    judge(&first.filter, first.reversed, facts),
                    judge(&second.filter, second.reversed, facts),
                );
                let result = match operator {
                    Operator::And => a && b,
                    Operator::Or => a || b,
                    Operator::Xor => a ^ b,
                };
                result != *negate
            }
            Self::App {
                plugin,
                name,
                config,
                sum,
                at_least,
                ..
            } => {
                let (max, total, _) = facts
                    .app
                    .get(&app_key(plugin, name, config))
                    .copied()
                    .unwrap_or((0, 0, 0));
                if *sum { total >= *at_least } else { max > 0 }
            }
        }
    }
}

/// Whether a filter, reversed or not, lets the account through.
pub fn judge(filter: &Filter, reversed: bool, facts: &Facts) -> bool {
    // Exempt mains pass, whatever the filter says (AA: "exempt regardless
    // of the above").
    if !filter.exempt().is_empty()
        && facts
            .main_affiliation
            .iter()
            .any(|id| filter.exempt().contains(id))
    {
        return true;
    }
    // A reversed app filter gates on absence, and an app only reports
    // characters it has data for: pass only when every character of the
    // account was reported, or missing data would let anyone in.
    if reversed
        && let Filter::App {
            plugin,
            name,
            config,
            ..
        } = filter
    {
        let reported = facts
            .app
            .get(&app_key(plugin, name, config))
            .map_or(0, |(_, _, n)| *n);
        if reported < facts.characters {
            return false;
        }
    }
    filter.passes(facts) != reversed
}

/// What the filters look at, for one account.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Facts {
    pub state: i64,
    /// The main's corporation and alliance.
    pub main_affiliation: Vec<i64>,
    /// Every character's corporation and alliance.
    pub affiliations: BTreeSet<i64>,
    /// Every character's militia.
    pub factions: BTreeSet<i64>,
    /// `None` until ESI has told us the main's birthday.
    pub main_age_days: Option<i64>,
    pub groups: BTreeSet<i64>,
    pub compliant: bool,
    /// Linked to Discord.
    pub discord: bool,
    /// App filter values, by [`app_key`]: the highest of the account's
    /// characters, their sum, and how many were reported.
    pub app: BTreeMap<String, (i64, i64, i64)>,
    /// How many characters the account has.
    pub characters: i64,
}

/// A stored filter with its reversal and grace period.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    pub id: i64,
    pub filter: Filter,
    pub reversed: bool,
    /// Days a member failing it keeps the group (AA's grace period per
    /// filter), when the group allows grace.
    pub grace_days: i32,
}

impl Rule {
    pub fn passes(&self, facts: &Facts) -> bool {
        judge(&self.filter, self.reversed, facts)
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
            factions: [500001].into(),
            main_age_days: Some(400),
            groups: [7].into(),
            compliant: true,
            discord: true,
            app: [(app_key("fat", "fats", "{\"days\":30}"), (4, 9, 3))].into(),
            characters: 3,
        }
    }

    fn rule(filter: Filter, reversed: bool) -> Rule {
        Rule {
            id: 1,
            filter,
            reversed,
            grace_days: 5,
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
                entities: vec![300],
                exempt: vec![],
            }
            .passes(&f)
        );
        assert!(Filter::CharacterAge { days: 365 }.passes(&f));
        assert!(!Filter::CharacterAge { days: 500 }.passes(&f));
        assert!(
            Filter::Groups {
                groups: vec![7, 8],
                all: false,
                exempt: vec![],
            }
            .passes(&f)
        );
        assert!(
            !Filter::Groups {
                groups: vec![7, 8],
                all: true,
                exempt: vec![],
            }
            .passes(&f)
        );
        assert!(Filter::Compliant {}.passes(&f));
        assert!(
            Filter::Faction {
                factions: vec![500001]
            }
            .passes(&f)
        );
        assert!(
            Filter::Service {
                service: Service::Discord
            }
            .passes(&f)
        );
        // Reversed: an alt in a hostile corporation keeps you out.
        let rules = vec![rule(
            Filter::AnyAffiliation {
                entities: vec![300],
                exempt: vec![],
            },
            true,
        )];
        assert_eq!(failing(&rules, &f).len(), 1);
        // Unless the main's corporation is exempt.
        let exempt = vec![rule(
            Filter::AnyAffiliation {
                entities: vec![300],
                exempt: vec![100],
            },
            true,
        )];
        assert!(failing(&exempt, &f).is_empty());
        // Unknown age never passes an age filter.
        let unknown = Facts {
            main_age_days: None,
            ..facts()
        };
        assert!(!Filter::CharacterAge { days: 1 }.passes(&unknown));
    }

    #[test]
    fn expressions_combine_two_filters() {
        let f = facts();
        let term = |filter: Filter, reversed: bool| Box::new(Term { filter, reversed });
        let expression = |operator, negate| Filter::Expression {
            operator,
            negate,
            first: term(Filter::State { states: vec![1] }, false),
            second: term(Filter::CharacterAge { days: 500 }, false),
        };
        assert!(!expression(Operator::And, false).passes(&f));
        assert!(expression(Operator::And, true).passes(&f));
        assert!(expression(Operator::Or, false).passes(&f));
        assert!(expression(Operator::Xor, false).passes(&f));
        // A reversed term is judged reversed.
        let reversed = Filter::Expression {
            operator: Operator::And,
            negate: false,
            first: term(Filter::State { states: vec![1] }, false),
            second: term(Filter::CharacterAge { days: 500 }, true),
        };
        assert!(reversed.passes(&f));
        assert_eq!(reversed.leaves().len(), 2);
    }

    #[test]
    fn app_filters_read_the_hosts_values() {
        let f = facts();
        let fats = |at_least| Filter::App {
            plugin: "fat".into(),
            name: "fats".into(),
            config: "{\"days\":30}".into(),
            sum: true,
            at_least,
            label: "FAT: FATs".into(),
        };
        assert!(fats(9).passes(&f));
        assert!(!fats(10).passes(&f));
        // Unknown settings never pass.
        let other = Filter::App {
            plugin: "fat".into(),
            name: "fats".into(),
            config: "{\"days\":7}".into(),
            sum: false,
            at_least: 0,
            label: String::new(),
        };
        assert!(!other.passes(&f));
        // Reversed, it passes only when every character was reported.
        let not_active = rule(fats(20), true);
        assert!(not_active.passes(&f));
        let partly = Facts {
            characters: 4,
            ..facts()
        };
        assert!(!not_active.passes(&partly));
    }

    #[test]
    fn filters_round_trip_as_stored() {
        let filter = Filter::Groups {
            groups: vec![3],
            all: true,
            exempt: vec![],
        };
        let json = serde_json::to_value(&filter).unwrap();
        assert_eq!(json["kind"], "groups");
        assert_eq!(json["config"]["groups"][0], 3);
        // Stored before exemptions: still reads.
        assert!(json["config"].get("exempt").is_none());
        assert_eq!(serde_json::from_value::<Filter>(json).unwrap(), filter);
        let compliant = serde_json::to_value(Filter::Compliant {}).unwrap();
        assert_eq!(
            serde_json::from_value::<Filter>(compliant).unwrap(),
            Filter::Compliant {}
        );
        let service = serde_json::to_value(Filter::Service {
            service: Service::Discord,
        })
        .unwrap();
        assert_eq!(service["config"]["service"], "discord");
    }
}
