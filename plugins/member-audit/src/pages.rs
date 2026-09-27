//! My Characters and the Character Finder, and what the character pages
//! share: entities, the app's page links, freshness.

use std::collections::BTreeMap;

use chrono::{Duration, Utc};
use tether_plugin_sdk::esi;
use tether_plugin_sdk::identity::Viewer;
use tether_plugin_sdk::storage::Value as Db;
use tether_plugin_sdk::{
    CardGrid, Column, Field, Form, Page, PageError, Profile, Request, Stat, Table, Tone, Value,
    alliance, badge, character, corporation, faction, isk, item_type, link, progress,
};

use crate::access::Access;
use crate::{
    count, float, int, name_of, opt_int, query, rfc3339, text, time_or_blank, when, with_rows,
};

/// A queue ending sooner than this is flagged.
const QUEUE_WARNING: Duration = Duration::hours(24);

/// The app's own pages beside the title.
pub(crate) fn app_links(page: Page, access: &Access) -> Page {
    let page = page
        .link("My Characters", "")
        .link("Skill Sets", "skill-sets");
    if access.finder {
        page.link("Character Finder", "finder")
            .link("Reports", "reports")
    } else {
        page
    }
}

/// An id as the entity its `names` category says it is, or its name.
pub(crate) fn entity(id: i64, name: String, category: &str) -> Value {
    match category {
        "character" => character(id, name).into(),
        "corporation" => corporation(id, name).into(),
        "alliance" => alliance(id, name).into(),
        "faction" => faction(id, name).into(),
        "inventory_type" => item_type(id, name).into(),
        _ => name.into(),
    }
}

/// SQL for an id's name and category (two columns), from `column`.
pub(crate) fn named(column: &str) -> String {
    format!(
        "{name}, coalesce((SELECT category FROM names WHERE id = {column}), '')",
        name = name_of(column)
    )
}

/// The skill in training: its name and level, and from when to when.
pub(crate) struct Training {
    pub skill_id: i64,
    pub label: String,
    pub start: Option<chrono::DateTime<Utc>>,
    pub finish: Option<chrono::DateTime<Utc>>,
}

impl Training {
    /// A progress bar that fills live, or the queue's state.
    pub fn value(&self) -> Value {
        match (self.start, self.finish) {
            (Some(start), Some(finish)) if finish > start && finish > Utc::now() => progress(0.0)
                .between(rfc3339(start), rfc3339(finish))
                .label(self.label.clone())
                .into(),
            _ => badge("Paused", Tone::Warning).into(),
        }
    }
}

/// What each character is training (the queue's first skill not yet
/// done), by character id, of `ids`.
pub(crate) fn training(ids: &Db) -> Result<Vec<(i64, Training)>, PageError> {
    let rows = query(
        &format!(
            "SELECT DISTINCT ON (q.character_id) q.character_id, q.skill_id, {skill}, q.level, q.start, q.finish \
             FROM queue q WHERE q.character_id = ANY(string_to_array($1, ',')::bigint[]) \
               AND (q.finish IS NULL OR q.finish > now()) \
             ORDER BY q.character_id, q.position",
            skill = name_of("q.skill_id")
        ),
        std::slice::from_ref(ids),
    )?;
    Ok(rows
        .iter()
        .map(|r| {
            (
                int(r, 0),
                Training {
                    skill_id: int(r, 1),
                    label: format!("{} {}", text(r, 2), roman(int(r, 3))),
                    start: when(r, 4),
                    finish: when(r, 5),
                },
            )
        })
        .collect())
}

pub(crate) fn roman(level: i64) -> &'static str {
    match level {
        1 => "I",
        2 => "II",
        3 => "III",
        4 => "IV",
        5 => "V",
        _ => "0",
    }
}

fn ids_param(viewer: &Viewer) -> Db {
    let ids: Vec<String> = viewer.characters.iter().map(|c| c.id.to_string()).collect();
    ids.join(",").into()
}

/// A character's row as the cards and the Finder read it.
const CHARACTER_COLUMNS: &str = "c.character_id, c.name, c.corporation_id, c.alliance_id, \
    coalesce(c.total_sp, 0), coalesce(c.wallet, 0), c.system_id, c.ship_type_id, c.synced_at, \
    EXISTS (SELECT 1 FROM section_syncs y WHERE y.character_id = c.character_id AND NOT y.ok)";

fn character_names() -> String {
    format!(
        "{corp}, {ally}, {system}, {ship}",
        corp = name_of("c.corporation_id"),
        ally = name_of("c.alliance_id"),
        system = name_of("c.system_id"),
        ship = name_of("c.ship_type_id"),
    )
}

/// Columns 0-9 as `CHARACTER_COLUMNS`, then 10-13 the names.
fn character_select(filter: &str) -> String {
    format!(
        "SELECT {CHARACTER_COLUMNS}, {names} FROM characters c {filter}",
        names = character_names()
    )
}

/// The card of a character (a row of [`character_select`]).
fn card(row: &[Db], training: Option<&Training>, main: bool) -> Profile {
    let id = int(row, 0);
    let mut profile = Profile::new(character(id, text(row, 1)))
        .corporation(corporation(int(row, 2), text(row, 10)));
    if let Some(ally) = opt_int(row, 3).filter(|a| *a > 0) {
        profile = profile.alliance(alliance(ally, text(row, 11)));
    }
    if main {
        profile = profile.badge(badge("Main", Tone::Neutral));
    }
    if row.get(9).and_then(Db::as_bool).unwrap_or(false) {
        profile = profile.badge(badge("Update issues", Tone::Warning));
    }
    let synced = when(row, 8);
    if synced.is_none() {
        return profile
            .badge(badge("Syncing", Tone::Neutral))
            .subtitle("Its first read is on its way.");
    }
    profile = profile
        .fact(
            "Location",
            if opt_int(row, 6).is_some() {
                text(row, 12).into()
            } else {
                Value::from("")
            },
        )
        .fact(
            "Ship",
            match opt_int(row, 7) {
                Some(ship) => item_type(ship, text(row, 13)).into(),
                None => Value::from(""),
            },
        )
        .fact("Wallet", isk(float(row, 5)))
        .fact("Skill points", int(row, 4))
        .fact(
            "Training",
            training.map_or_else(
                || badge("Not training", Tone::Warning).into(),
                Training::value,
            ),
        )
        .fact("Last update", time_or_blank(row, 8));
    profile
}

/// My Characters: Tether's Register Character card, then a card per
/// character, and all of them together. Also the Dashboard's widget.
pub(crate) fn my_characters(viewer: &Viewer) -> Result<Page, PageError> {
    let access = Access::of(viewer);
    let mine = ids_param(viewer);
    let rows = query(
        &character_select(
            "WHERE c.character_id = ANY(string_to_array($1, ',')::bigint[]) ORDER BY c.name",
        ),
        std::slice::from_ref(&mine),
    )?;
    let training = training(&mine)?;
    let (sp, wallet) = rows
        .iter()
        .fold((0i64, 0f64), |(sp, w), r| (sp + int(r, 4), w + float(r, 5)));
    let now = Utc::now();
    let ending = rows
        .iter()
        .filter(|r| {
            let id = int(r, 0);
            training
                .iter()
                .find(|(c, _)| *c == id)
                .and_then(|(_, t)| t.finish)
                .is_none_or(|end| end - now < QUEUE_WARNING)
        })
        .count();
    let mut grid = CardGrid::new().register();
    for row in &rows {
        let id = int(row, 0);
        let t = training.iter().find(|(c, _)| *c == id).map(|(_, t)| t);
        grid = grid.linked(
            card(row, t, id == viewer.main.id),
            format!("character/{id}"),
        );
    }
    // Characters on the account that Member Audit doesn't have: not
    // registered with its scopes (yet), as aa-memberaudit's warning.
    for c in &viewer.characters {
        if !rows.iter().any(|r| int(r, 0) == c.id) {
            let mut profile = Profile::new(character(c.id, c.name.clone()))
                .badge(badge("Not registered", Tone::Warning))
                .subtitle("Register it with Member Audit's scopes to see it here.");
            if c.id == viewer.main.id {
                profile = profile.badge(badge("Main", Tone::Neutral));
            }
            grid = grid.card(profile);
        }
    }
    let registered = rows.len();
    Ok(app_links(
        Page::new("My Characters").description("Your characters, and all of them together"),
        &access,
    )
    .stats(vec![
        Stat::new("Characters", count(registered))
            .caption(format!("of {} on your account", viewer.characters.len())),
        Stat::new("Wallets", isk(wallet)),
        Stat::new("Skill points", sp),
        Stat::new("Queues ending", count(ending)).caption("within a day, or empty"),
    ])
    .cards(grid))
}

/// The Character Finder: the characters in the viewer's scope, searched by
/// character, corporation or alliance name.
pub(crate) fn finder(access: &Access, request: &Request) -> Result<Page, PageError> {
    let q = request
        .query
        .iter()
        .find(|(k, _)| k == "q")
        .map(|(_, v)| v.as_str())
        .unwrap_or_default();
    finder_page(access, q)
}

pub(crate) fn finder_page(access: &Access, q: &str) -> Result<Page, PageError> {
    if !access.finder {
        return Err(PageError::NotFound);
    }
    let q: String = q.trim().to_lowercase().chars().take(100).collect();
    let (scope, mut params) = access.listed(1);
    let scope_params = params.len();
    let search = if q.is_empty() {
        String::new()
    } else {
        params.push(q.clone().into());
        let n = params.len();
        // Their owner's main's name too, as aa-memberaudit's search.
        params.push(crate::id_list(&access.mains_named(&q)).into());
        let mains = params.len();
        format!(
            " AND (strpos(lower(c.name), ${n}) > 0 \
               OR strpos(lower({corp}), ${n}) > 0 \
               OR strpos(lower({ally}), ${n}) > 0 \
               OR c.character_id = ANY(string_to_array(${mains}, ',')::bigint[]))",
            corp = name_of("c.corporation_id"),
            ally = name_of("c.alliance_id"),
        )
    };
    let rows = query(
        &character_select(&format!("WHERE {scope}{search} ORDER BY c.name LIMIT 500")),
        &params,
    )?;
    let total = query(
        &format!("SELECT count(*) FROM characters c WHERE {scope}"),
        &params[..scope_params],
    )?;
    let owners: Vec<_> = rows.iter().map(|r| access.owner(int(r, 0))).collect();
    let organisations = corporation_names(
        owners
            .iter()
            .flatten()
            .map(|o| o.main.corporation_id)
            .collect(),
    )?;
    let table = with_rows(
        Table::new(vec![
            Column::text("Character"),
            Column::text("Corporation"),
            Column::text("Alliance"),
            Column::text("Main"),
            Column::text("Main organisation"),
            Column::text("State"),
            Column::text("Location"),
            Column::text("Ship"),
            Column::numeric("Skill points"),
            Column::numeric("Last update"),
        ])
        .title(if q.is_empty() {
            "Characters".to_owned()
        } else {
            format!("Characters matching \"{q}\"")
        })
        .empty("No characters match."),
        rows.iter().zip(&owners).map(|(r, owner)| {
            let id = int(r, 0);
            let corp = int(r, 2);
            let ally = opt_int(r, 3).filter(|a| *a > 0);
            let (main, organisation, state) = match owner {
                Some(o) => (
                    character(o.main.id, o.main.name.clone()).into(),
                    corporation(
                        o.main.corporation_id,
                        organisations
                            .get(&o.main.corporation_id)
                            .cloned()
                            .unwrap_or_default(),
                    )
                    .into(),
                    o.state.name.clone().into(),
                ),
                None => ("".into(), "".into(), "".into()),
            };
            vec![
                if access.may_open(id) {
                    link(text(r, 1), format!("character/{id}")).into()
                } else {
                    character(id, text(r, 1)).into()
                },
                corporation(corp, text(r, 10)).into(),
                match ally {
                    Some(a) => alliance(a, text(r, 11)).into(),
                    None => "".into(),
                },
                main,
                organisation,
                state,
                if opt_int(r, 6).is_some() {
                    text(r, 12).into()
                } else {
                    "".into()
                },
                match opt_int(r, 7) {
                    Some(ship) => item_type(ship, text(r, 13)).into(),
                    None => "".into(),
                },
                int(r, 4).into(),
                time_or_blank(r, 8),
            ]
        }),
    );
    Ok(app_links(
        Page::new("Character Finder").description(format!(
            "Member characters registered with Member Audit: {}",
            access.scope_words()
        )),
        access,
    )
    .stats(vec![
        Stat::new("Characters", total.first().map_or(0, |r| int(r, 0))).caption("in your scope"),
    ])
    .form(
        Form::new("search", "Search").field(
            Field::text("q", "Character, corporation, alliance or main", 100)
                .value(q.clone())
                .help("Part of a name is enough."),
        ),
    )
    .table(table))
}

/// Names for corporations (a main's may be no member character's):
/// stored, else asked of ESI (a page can't store them), else none.
fn corporation_names(mut ids: Vec<i64>) -> Result<BTreeMap<i64, String>, PageError> {
    ids.retain(|id| *id > 0);
    ids.sort_unstable();
    ids.dedup();
    if ids.is_empty() {
        return Ok(BTreeMap::new());
    }
    let mut names: BTreeMap<i64, String> = query(
        "SELECT id, name FROM names WHERE id = ANY(string_to_array($1, ',')::bigint[])",
        &[crate::id_list(&ids).into()],
    )?
    .iter()
    .map(|r| (int(r, 0), text(r, 1)))
    .collect();
    let missing: Vec<i64> = ids
        .into_iter()
        .filter(|id| !names.contains_key(id))
        .take(1000)
        .collect();
    if !missing.is_empty()
        && let Ok(found) = esi::names(&missing)
    {
        names.extend(found.into_iter().map(|n| (n.id, n.name)));
    }
    Ok(names)
}
