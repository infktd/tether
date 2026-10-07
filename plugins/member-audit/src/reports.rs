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

use tether_plugin_sdk::identity::Builtin;
use tether_plugin_sdk::storage::Value as Db;
use tether_plugin_sdk::{
    Column, Page, PageError, Request, Table, Tone, Toolbar, Value, badge, character, corporation,
};

use crate::access::Access;
use crate::sets::{SetGroup, UNGROUPED, grouped, set_groups, skill_sets};
use crate::{int, query, text, with_rows};

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
