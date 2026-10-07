//! Reports (aa-memberaudit's `reports_access`, `views/reports.py`), over
//! the characters in the viewer's view scope:
//!
//! - **Skill sets**: each set's count of characters who can use it, by
//!   group; then aa-memberaudit's table, a row per group and character:
//!   its main, the main's state and organisation, the group's sets it can
//!   use, whether it's the main and whether the group is a doctrine, with
//!   aa-memberaudit's filters. As aa-memberaudit, Guests' characters are
//!   left out.

use std::collections::{BTreeMap, BTreeSet};

use tether_plugin_sdk::identity::{Builtin, Member};
use tether_plugin_sdk::storage::Value as Db;
use tether_plugin_sdk::{
    Column, Page, PageError, Request, Stat, Table, Tone, Toolbar, Value, alliance, badge,
    character, corporation,
};

use crate::access::Access;
use crate::sets::{SetGroup, UNGROUPED, grouped, set_groups, skill_sets};
use crate::{count, int, query, text, with_rows};

/// Rows in a table, at most (the host's limit).
const MAX_ROWS: usize = 500;

/// Which skill sets each character can use (every required level), of
/// the characters `condition` picks over `characters c` (its parameters
/// from `$1`): the first [`MAX_ROWS`] by name.
fn usable(condition: &str, params: &[Db]) -> Result<Vec<(i64, String, BTreeSet<i64>)>, PageError> {
    let sql = format!(
        "SELECT c.character_id, c.name, array_to_string(ARRAY( \
           SELECT s.id FROM skill_sets s WHERE NOT EXISTS ( \
             SELECT 1 FROM skill_set_skills k WHERE k.set_id = s.id \
             AND k.required_level IS NOT NULL AND NOT EXISTS ( \
               SELECT 1 FROM skills x WHERE x.character_id = c.character_id \
                 AND x.skill_id = k.skill_id AND x.active_level >= k.required_level))), ',') \
         FROM characters c WHERE {condition} ORDER BY c.name LIMIT {MAX_ROWS}"
    );
    Ok(query(&sql, params)?
        .iter()
        .map(|r| {
            let sets = text(r, 2)
                .split(',')
                .filter_map(|id| id.parse().ok())
                .collect();
            (int(r, 0), text(r, 1), sets)
        })
        .collect())
}

/// How many characters of those `condition` picks can use each set.
fn counts(condition: &str, params: &[Db]) -> Result<BTreeMap<i64, i64>, PageError> {
    let sql = format!(
        "SELECT s.id, (SELECT count(*) FROM characters c WHERE {condition} AND NOT EXISTS ( \
           SELECT 1 FROM skill_set_skills k WHERE k.set_id = s.id \
           AND k.required_level IS NOT NULL AND NOT EXISTS ( \
             SELECT 1 FROM skills x WHERE x.character_id = c.character_id \
               AND x.skill_id = k.skill_id AND x.active_level >= k.required_level))) \
         FROM skill_sets s"
    );
    Ok(query(&sql, params)?
        .iter()
        .map(|r| (int(r, 0), int(r, 1)))
        .collect())
}

/// The characters the reports cover, as SQL over `characters c` with its
/// parameters from `$1`: those in the viewer's scope, without Guests'
/// (aa-memberaudit's reports leave out the Guest state).
pub(crate) fn covered(access: &Access) -> (String, Vec<Db>) {
    let (scope, mut params) = access.listed(1);
    let guests: Vec<i64> = access
        .all_owners()
        .filter(|o| o.state.builtin == Some(Builtin::Guest))
        .map(|o| o.character_id)
        .collect();
    params.push(crate::id_list(&guests).into());
    let n = params.len();
    (
        format!("({scope}) AND NOT c.character_id = ANY(string_to_array(${n}, ',')::bigint[])"),
        params,
    )
}

fn yes_no(yes: bool) -> Value {
    if yes {
        badge("Yes", Tone::Success).into()
    } else {
        badge("No", Tone::Neutral).into()
    }
}

/// Distinct `(value, label)` choices, by label, at most 100 (the host's).
fn choices(mut pairs: Vec<(String, String)>) -> Vec<(String, String)> {
    pairs.sort_by_key(|(value, label)| (label.to_lowercase(), value.clone()));
    pairs.dedup();
    pairs.truncate(100);
    pairs
}

/// The links between the reports.
pub(crate) fn report_page(title: &str, description: String) -> Page {
    Page::new(title)
        .description(description)
        .link("Skill sets", "reports")
        .link("User compliance", "reports/users")
        .link("Corporation compliance", "reports/corporations")
}

/// A member account as the compliance reports count it.
struct Counted<'a> {
    member: &'a Member,
    total: usize,
    unregistered: usize,
}

/// The member accounts in the viewer's scope, without Guests, with their
/// characters counted (aa-memberaudit's reports).
fn counted<'a>(access: &'a Access) -> Vec<Counted<'a>> {
    access
        .members_in_scope()
        .filter(|m| m.state.builtin != Some(Builtin::Guest))
        .map(|member| Counted {
            member,
            total: member.characters.len(),
            unregistered: member.characters.iter().filter(|c| !c.registered).count(),
        })
        .collect()
}

/// A share of characters registered, as a whole percentage (0 for none).
fn percent(registered: usize, total: usize) -> i64 {
    if total == 0 {
        return 0;
    }
    ((registered as f64 / total as f64) * 100.0).round() as i64
}

/// aa-memberaudit's colour code: fully, partly (85% and up) or not
/// compliant.
fn compliance(percent: i64) -> Value {
    let tone = match percent {
        100 => Tone::Success,
        85.. => Tone::Warning,
        _ => Tone::Danger,
    };
    badge(format!("{percent}%"), tone).into()
}

/// User Compliance: a row per pilot in the viewer's scope (their main),
/// whether any of their characters is registered with Member Audit and
/// whether all are.
pub(crate) fn user_compliance(access: &Access, request: &Request) -> Result<Page, PageError> {
    if !access.reports {
        return Err(PageError::NotFound);
    }
    let users = counted(access);
    let names = crate::pages::names_of(
        users
            .iter()
            .flat_map(|u| {
                [
                    Some(u.member.main.corporation_id),
                    u.member.main.alliance_id,
                ]
            })
            .flatten()
            .collect(),
    )?;
    let named = |id: i64| names.get(&id).cloned().unwrap_or_else(|| id.to_string());
    let state = request.param("state");
    let corp: Option<i64> = request.param("corporation").parse().ok();
    let ally: Option<i64> = request.param("alliance").parse().ok();
    let registered = request.param("registered");
    let compliant = request.param("compliant");
    let mut shown: Vec<&Counted> = users
        .iter()
        .filter(|u| state.is_empty() || u.member.state.name == state)
        .filter(|u| corp.is_none_or(|c| u.member.main.corporation_id == c))
        .filter(|u| ally.is_none_or(|a| u.member.main.alliance_id == Some(a)))
        .filter(|u| registered.is_empty() || (registered == "yes") == (u.unregistered < u.total))
        .filter(|u| compliant.is_empty() || (compliant == "yes") == (u.unregistered == 0))
        .collect();
    shown.sort_by_key(|u| u.member.main.name.to_lowercase());
    let cut = shown.len() > MAX_ROWS;
    let rows = shown.iter().take(MAX_ROWS).map(|u| {
        let main = &u.member.main;
        vec![
            if access.may_open(main.id) {
                character(main.id, main.name.clone())
                    .link(format!("character/{}", main.id))
                    .into()
            } else {
                character(main.id, main.name.clone()).into()
            },
            u.member.state.name.clone().into(),
            corporation(main.corporation_id, named(main.corporation_id)).into(),
            yes_no(u.unregistered < u.total),
            yes_no(u.unregistered == 0),
            count(u.total),
            count(u.unregistered),
        ]
    });
    let mut table = Table::new(vec![
        Column::text("User"),
        Column::text("State"),
        Column::text("Organisation"),
        Column::text("Registered?"),
        Column::text("Compliant?"),
        Column::numeric("Characters"),
        Column::numeric("Unregistered"),
    ])
    .empty("No pilots match.");
    if cut {
        table = table.title(format!(
            "The first {MAX_ROWS} of {} pilots, by main. Filters narrow them.",
            shown.len()
        ));
    }
    let yes_no_choices = || {
        vec![
            ("yes".to_owned(), "Yes".to_owned()),
            ("no".to_owned(), "No".to_owned()),
        ]
    };
    let toolbar = Toolbar::new()
        .filter(
            "state",
            "State",
            choices(
                users
                    .iter()
                    .map(|u| (u.member.state.name.clone(), u.member.state.name.clone()))
                    .collect(),
            ),
        )
        .filter(
            "alliance",
            "Alliance",
            choices(
                users
                    .iter()
                    .filter_map(|u| u.member.main.alliance_id)
                    .map(|a| (a.to_string(), named(a)))
                    .collect(),
            ),
        )
        .filter(
            "corporation",
            "Corporation",
            choices(
                users
                    .iter()
                    .map(|u| {
                        let c = u.member.main.corporation_id;
                        (c.to_string(), named(c))
                    })
                    .collect(),
            ),
        )
        .filter("registered", "Registered?", yes_no_choices())
        .filter("compliant", "Compliant?", yes_no_choices());
    let fully = users.iter().filter(|u| u.unregistered == 0).count();
    Ok(report_page(
        "Reports",
        format!(
            "User compliance: whether every character of each pilot is registered with Member \
             Audit, of {}",
            access.scope_words()
        ),
    )
    .stats(vec![
        Stat::new("Pilots", count(users.len())),
        Stat::new("Compliant", count(fully)),
        Stat::new("Not compliant", count(users.len() - fully)),
    ])
    .toolbar(toolbar)
    .table(with_rows(table, rows)))
}

/// Corporation Compliance: a row per corporation of the mains in the
/// viewer's scope, with its pilots, their characters and the share
/// registered with Member Audit.
pub(crate) fn corporation_compliance(
    access: &Access,
    request: &Request,
) -> Result<Page, PageError> {
    if !access.reports {
        return Err(PageError::NotFound);
    }
    let users = counted(access);
    // Each corporation: its alliance, mains, characters, unregistered.
    let mut corporations: BTreeMap<i64, (Option<i64>, usize, usize, usize)> = BTreeMap::new();
    for u in &users {
        let row = corporations.entry(u.member.main.corporation_id).or_insert((
            u.member.main.alliance_id,
            0,
            0,
            0,
        ));
        row.1 += 1;
        row.2 += u.total;
        row.3 += u.unregistered;
    }
    let names = crate::pages::names_of(
        corporations
            .iter()
            .flat_map(|(c, (a, ..))| [Some(*c), *a])
            .flatten()
            .collect(),
    )?;
    let named = |id: i64| names.get(&id).cloned().unwrap_or_else(|| id.to_string());
    let corp: Option<i64> = request.param("corporation").parse().ok();
    let ally: Option<i64> = request.param("alliance").parse().ok();
    let compliant = request.param("compliant");
    let mut shown: Vec<(i64, Option<i64>, usize, usize, i64)> = corporations
        .iter()
        .map(|(c, (a, mains, total, unregistered))| {
            (
                *c,
                *a,
                *mains,
                *total,
                percent(total - unregistered, *total),
            )
        })
        .filter(|(c, ..)| corp.is_none_or(|x| x == *c))
        .filter(|(_, a, ..)| ally.is_none_or(|x| Some(x) == *a))
        .filter(|(.., p)| compliant.is_empty() || (compliant == "yes") == (*p == 100))
        .collect();
    shown.sort_by_key(|(c, ..)| named(*c).to_lowercase());
    let rows = shown.iter().take(MAX_ROWS).map(|(c, a, mains, total, p)| {
        vec![
            corporation(*c, named(*c)).into(),
            a.map_or_else(|| "".into(), |a| alliance(a, named(a)).into()),
            count(*mains),
            count(*total),
            compliance(*p),
        ]
    });
    let toolbar = Toolbar::new()
        .filter(
            "alliance",
            "Alliance",
            choices(
                corporations
                    .values()
                    .filter_map(|(a, ..)| *a)
                    .map(|a| (a.to_string(), named(a)))
                    .collect(),
            ),
        )
        .filter(
            "corporation",
            "Corporation",
            choices(
                corporations
                    .keys()
                    .map(|c| (c.to_string(), named(*c)))
                    .collect(),
            ),
        )
        .filter(
            "compliant",
            "Compliant?",
            vec![
                ("yes".to_owned(), "Yes".to_owned()),
                ("no".to_owned(), "No".to_owned()),
            ],
        );
    Ok(report_page(
        "Reports",
        format!(
            "Corporation compliance: the share of each corporation's characters registered \
             with Member Audit, by their pilots' mains, of {}",
            access.scope_words()
        ),
    )
    .toolbar(toolbar)
    .table(with_rows(
        Table::new(vec![
            Column::text("Organisation"),
            Column::text("Alliance"),
            Column::numeric("Pilots"),
            Column::numeric("Characters"),
            Column::numeric("Compliance"),
        ])
        .empty("No corporations match."),
        rows,
    ))
    .text(
        "Compliance: 100% fully compliant, 85% and up partly compliant, below that not \
         compliant.",
    ))
}

/// The Skill Sets report.
pub(crate) fn skill_sets_report(access: &Access, request: &Request) -> Result<Page, PageError> {
    if !access.reports {
        return Err(PageError::NotFound);
    }
    let sets = skill_sets()?;
    let groups = set_groups()?;
    let (scope, scope_params) = covered(access);
    let counts = counts(&scope, &scope_params)?;
    let mut summary = Vec::new();
    for (group, members) in grouped(&sets, &groups) {
        for set in members {
            if summary.len() >= MAX_ROWS {
                break;
            }
            summary.push(vec![
                group
                    .map_or_else(|| UNGROUPED.to_owned(), SetGroup::label)
                    .into(),
                set.value(),
                yes_no(group.is_some_and(|g| g.doctrine)),
                counts.get(&set.id).copied().unwrap_or(0).into(),
            ]);
        }
    }

    // aa-memberaudit's filters: by the owner's main (state, corporation,
    // alliance) in SQL, the rest on the rows.
    let state = request.param("state");
    let corp: Option<i64> = request.param("corporation").parse().ok();
    let ally: Option<i64> = request.param("alliance").parse().ok();
    let main_only = request.param("main");
    let mut condition = scope.clone();
    let mut params = scope_params.clone();
    if !state.is_empty() || corp.is_some() || ally.is_some() {
        let picked: Vec<i64> = access
            .all_owners()
            .filter(|o| state.is_empty() || o.state.name == state)
            .filter(|o| corp.is_none_or(|c| o.main.corporation_id == c))
            .filter(|o| ally.is_none_or(|a| o.main.alliance_id == Some(a)))
            .map(|o| o.character_id)
            .collect();
        params.push(crate::id_list(&picked).into());
        condition = format!(
            "{condition} AND c.character_id = ANY(string_to_array(${}, ',')::bigint[])",
            params.len()
        );
    }
    let characters = usable(&condition, &params)?;
    let orgs = crate::pages::names_of(
        access
            .all_owners()
            .flat_map(|o| [Some(o.main.corporation_id), o.main.alliance_id])
            .flatten()
            .collect(),
    )?;
    let set_names: BTreeMap<i64, &str> = sets.iter().map(|s| (s.id, s.name.as_str())).collect();
    let want_group = request.param("group");
    let want_has = request.param("has");
    let want_doctrine = request.param("doctrine");
    let mut rows = Vec::new();
    let mut cut = characters.len() >= MAX_ROWS;
    'groups: for (group, members) in grouped(&sets, &groups) {
        let key = group.map_or_else(|| "0".to_owned(), |g| g.id.to_string());
        if !want_group.is_empty() && want_group != key {
            continue;
        }
        let doctrine = group.is_some_and(|g| g.doctrine);
        if !want_doctrine.is_empty() && (want_doctrine == "yes") != doctrine {
            continue;
        }
        let ids: BTreeSet<i64> = members.iter().map(|s| s.id).collect();
        for (id, name, can) in &characters {
            let has: Vec<&str> = can
                .iter()
                .filter(|s| ids.contains(s))
                .filter_map(|s| set_names.get(s).copied())
                .collect();
            if !want_has.is_empty() && (want_has == "yes") != !has.is_empty() {
                continue;
            }
            let owner = access.owner(*id);
            let is_main = owner.is_some_and(|o| o.main.id == *id);
            if !main_only.is_empty() && (main_only == "yes") != is_main {
                continue;
            }
            if rows.len() >= MAX_ROWS {
                cut = true;
                break 'groups;
            }
            let mut sets_used: Vec<&str> = has;
            sets_used.sort_by_key(|n| n.to_lowercase());
            rows.push(vec![
                group
                    .map_or_else(|| UNGROUPED.to_owned(), SetGroup::label)
                    .into(),
                if access.may_open(*id) {
                    character(*id, name.clone())
                        .link(format!("character/{id}/skills"))
                        .into()
                } else {
                    character(*id, name.clone()).into()
                },
                owner.map_or_else(
                    || "".into(),
                    |o| character(o.main.id, o.main.name.clone()).into(),
                ),
                owner.map_or_else(|| "".into(), |o| o.state.name.clone().into()),
                owner.map_or_else(
                    || "".into(),
                    |o| {
                        corporation(
                            o.main.corporation_id,
                            orgs.get(&o.main.corporation_id)
                                .cloned()
                                .unwrap_or_default(),
                        )
                        .into()
                    },
                ),
                if sets_used.is_empty() {
                    badge("None", Tone::Neutral).into()
                } else {
                    crate::clip(&sets_used.join(", "), 1500).into()
                },
                yes_no(is_main),
                yes_no(doctrine),
            ]);
        }
    }

    let owners: Vec<_> = access
        .all_owners()
        .filter(|o| o.state.builtin != Some(Builtin::Guest))
        .collect();
    let named = |id: i64| orgs.get(&id).cloned().unwrap_or_else(|| id.to_string());
    let yes_no_choices = || {
        vec![
            ("yes".to_owned(), "Yes".to_owned()),
            ("no".to_owned(), "No".to_owned()),
        ]
    };
    let mut group_choices: Vec<(String, String)> = groups
        .iter()
        .map(|g| (g.id.to_string(), g.label()))
        .collect();
    group_choices.push(("0".to_owned(), UNGROUPED.to_owned()));
    let toolbar = Toolbar::new()
        .filter("group", "Group", choices(group_choices))
        .filter(
            "state",
            "State",
            choices(
                owners
                    .iter()
                    .map(|o| (o.state.name.clone(), o.state.name.clone()))
                    .collect(),
            ),
        )
        .filter(
            "corporation",
            "Corporation",
            choices(
                owners
                    .iter()
                    .map(|o| {
                        (
                            o.main.corporation_id.to_string(),
                            named(o.main.corporation_id),
                        )
                    })
                    .collect(),
            ),
        )
        .filter(
            "alliance",
            "Alliance",
            choices(
                owners
                    .iter()
                    .filter_map(|o| o.main.alliance_id)
                    .map(|a| (a.to_string(), named(a)))
                    .collect(),
            ),
        )
        .filter("has", "Required skills", yes_no_choices())
        .filter("doctrine", "Doctrine", yes_no_choices())
        .filter("main", "Main", yes_no_choices());
    let mut table = Table::new(vec![
        Column::text("Group"),
        Column::text("Character"),
        Column::text("Main"),
        Column::text("State"),
        Column::text("Organisation"),
        Column::text("Has the required skills of"),
        Column::text("Main?"),
        Column::text("Doctrine"),
    ])
    .empty("No characters match.");
    if cut {
        table = table.title(format!(
            "Characters: the first {MAX_ROWS} rows, by group and name. Filters narrow them."
        ));
    } else {
        table = table.title("Characters");
    }
    Ok(report_page(
        "Reports",
        format!(
            "Skill sets: which characters can use each, of {}",
            access.scope_words()
        ),
    )
    .toolbar(toolbar)
    .table(with_rows(
        Table::new(vec![
            Column::text("Group"),
            Column::text("Skill set"),
            Column::text("Doctrine"),
            Column::numeric("Characters"),
        ])
        .title("Skill sets")
        .empty("No skill sets yet."),
        summary,
    ))
    .table(with_rows(table, rows)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compliance_is_aa_s_rounded_share_and_colour() {
        assert_eq!(percent(2, 3), 67);
        assert_eq!(percent(0, 0), 0);
        assert_eq!(percent(5, 5), 100);
        let tone = |p| match compliance(p) {
            Value::Badge(b) => b.tone,
            _ => unreachable!("a badge"),
        };
        assert_eq!(tone(100), Tone::Success);
        assert_eq!(tone(85), Tone::Warning);
        assert_eq!(tone(84), Tone::Danger);
    }
}
