//! My Characters and the Character Finder, and what the character pages
//! share: entities, the app's page links, freshness.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{Duration, Utc};
use tether_plugin_sdk::esi;
use tether_plugin_sdk::identity::{Character, Member, MemberCharacter, Viewer};
use tether_plugin_sdk::storage::Value as Db;
use tether_plugin_sdk::{
    CardGrid, Column, Page, PageError, Profile, Request, Stat, Table, Tone, Toolbar, Value,
    alliance, badge, character, corporation, faction, isk, item_type, progress,
};

use crate::access::Access;
use crate::{
    boolean, count, float, int, name_of, opt_int, query, rfc3339, text, time_or_blank, when,
};

/// A queue ending sooner than this is flagged.
const QUEUE_WARNING: Duration = Duration::hours(24);

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
    EXISTS (SELECT 1 FROM section_syncs y WHERE y.character_id = c.character_id AND NOT y.ok), \
    c.is_shared";

fn character_names() -> String {
    format!(
        "{corp}, {ally}, {system}, {ship}",
        corp = name_of("c.corporation_id"),
        ally = name_of("c.alliance_id"),
        system = name_of("c.system_id"),
        ship = name_of("c.ship_type_id"),
    )
}

/// Columns 0-10 as `CHARACTER_COLUMNS`, then 11-14 the names.
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
        .corporation(corporation(int(row, 2), text(row, 11)));
    if let Some(ally) = opt_int(row, 3).filter(|a| *a > 0) {
        profile = profile.alliance(alliance(ally, text(row, 12)));
    }
    if main {
        profile = profile.badge(badge("Main", Tone::Neutral));
    }
    if row.get(9).and_then(Db::as_bool).unwrap_or(false) {
        profile = profile.badge(badge("Update issues", Tone::Warning));
    }
    if row.get(10).and_then(Db::as_bool).unwrap_or(false) {
        profile = profile.badge(badge("Shared", Tone::Neutral));
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
                text(row, 13).into()
            } else {
                Value::from("")
            },
        )
        .fact(
            "Ship",
            match opt_int(row, 7) {
                Some(ship) => item_type(ship, text(row, 14)).into(),
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

/// My characters: a card per character (Tether draws them as rows), Tether's
/// Register another character, and all of them together. Tether shows it
/// as the Dashboard.
pub(crate) fn my_characters(viewer: &Viewer) -> Result<Page, PageError> {
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
            // No status badge: on the Dashboard, Tether's footer under the
            // card says it and links to Register Character.
            let mut profile = Profile::new(character(c.id, c.name.clone()))
                .subtitle("Register it to see its skills, assets and wallet here.");
            if c.id == viewer.main.id {
                profile = profile.badge(badge("Main", Tone::Neutral));
            }
            grid = grid.card(profile);
        }
    }
    let registered = rows.len();
    Ok(Page::new("My characters")
        .description("Your characters, and all of them together")
        .stats(vec![
            Stat::new("Characters", count(registered))
                .caption(format!("of {} on your account", viewer.characters.len())),
            Stat::new("Wallets", isk(wallet)),
            Stat::new("Skill points", sp),
            Stat::new("Queues ending", count(ending)).caption("within a day, or empty"),
        ])
        .cards(grid))
}

/// The Character Finder (aa-memberaudit's): every character of the pilots
/// in the viewer's scope, those not registered with Member Audit flagged,
/// each with its main (the main marked), the main's organisation and
/// state; shared characters for `view_shared_characters`. Its search is
/// the toolbar's, in the address, and finds by what the table doesn't
/// show too (a main's name); aa-memberaudit's filters beside it.
pub(crate) fn finder(access: &Access, request: &Request) -> Result<Page, PageError> {
    if !access.finder {
        return Err(PageError::NotFound);
    }
    let q: String = request
        .search()
        .trim()
        .to_lowercase()
        .chars()
        .take(100)
        .collect();
    let filters = FinderFilters::of(request);
    let (scope, scope_params) = access.found(1);
    let total = query(
        &format!("SELECT count(*) FROM characters c WHERE {scope}"),
        &scope_params,
    )?
    .first()
    .map_or(0, |r| int(r, 0));

    // Members' characters Member Audit hasn't read: not registered (or
    // not read yet), from the host, of the pilots in scope.
    let in_scope: Vec<i64> = access
        .members_in_scope()
        .flat_map(|m| m.characters.iter().map(|c| c.character.id))
        .collect();
    // A few thousand at a time, within storage's rows per answer.
    let mut read: BTreeSet<i64> = BTreeSet::new();
    for chunk in in_scope.chunks(4000) {
        read.extend(
            query(
                "SELECT character_id FROM characters \
                 WHERE character_id = ANY(string_to_array($1, ',')::bigint[])",
                &[crate::id_list(chunk).into()],
            )?
            .iter()
            .map(|r| int(r, 0)),
        );
    }
    let others: Vec<(&Member, &MemberCharacter)> = access
        .members_in_scope()
        .flat_map(|m| m.characters.iter().map(move |c| (m, c)))
        .filter(|(_, c)| !read.contains(&c.character.id))
        .collect();
    let unregistered = others.iter().filter(|(_, c)| !c.registered).count();

    // Names of every organisation a row or a filter may show.
    let corporations: Vec<(i64, Option<i64>)> = query(
        &format!("SELECT DISTINCT c.corporation_id, c.alliance_id FROM characters c WHERE {scope}"),
        &scope_params,
    )?
    .iter()
    .map(|r| (int(r, 0), opt_int(r, 1).filter(|a| *a > 0)))
    .chain(
        others
            .iter()
            .map(|(_, c)| (c.character.corporation_id, c.character.alliance_id)),
    )
    .collect();
    let mains: Vec<&Character> = access
        .owners_in_scope(true)
        .map(|o| &o.main)
        .chain(access.members_in_scope().map(|m| &m.main))
        .collect();
    let names = names_of(
        corporations
            .iter()
            .flat_map(|(c, a)| [Some(*c), *a])
            .chain(
                mains
                    .iter()
                    .flat_map(|m| [Some(m.corporation_id), m.alliance_id]),
            )
            .flatten()
            .collect(),
    )?;
    let named = |id: i64| names.get(&id).cloned().unwrap_or_default();

    // Registered characters, read by Member Audit: the filters on the
    // character in SQL, those on its owner by the ids they leave.
    let mut rows: Vec<(String, Found)> = Vec::new();
    if filters.unregistered != Some(true) {
        let mut params = scope_params.clone();
        let mut condition = scope.clone();
        if let Some(corp) = filters.corporation {
            params.push(corp.into());
            condition.push_str(&format!(" AND c.corporation_id = ${}", params.len()));
        }
        if let Some(ally) = filters.alliance {
            params.push(ally.into());
            condition.push_str(&format!(" AND c.alliance_id = ${}", params.len()));
        }
        if filters.by_owner() {
            let ids: Vec<i64> = access
                .owners_in_scope(true)
                .filter(|o| {
                    filters.owner_passes(&o.main, &o.state.name, o.main.id == o.character_id)
                })
                .map(|o| o.character_id)
                .collect();
            params.push(crate::id_list(&ids).into());
            condition.push_str(&format!(
                " AND c.character_id = ANY(string_to_array(${}, ',')::bigint[])",
                params.len()
            ));
        }
        if !q.is_empty() {
            params.push(q.clone().into());
            let n = params.len();
            // Their owner's main's name too, as aa-memberaudit's search.
            params.push(crate::id_list(&access.mains_named(&q)).into());
            let by_main = params.len();
            condition.push_str(&format!(
                " AND (strpos(lower(c.name), ${n}) > 0 \
                   OR strpos(lower({corp}), ${n}) > 0 \
                   OR strpos(lower({ally}), ${n}) > 0 \
                   OR c.character_id = ANY(string_to_array(${by_main}, ',')::bigint[]))",
                corp = name_of("c.corporation_id"),
                ally = name_of("c.alliance_id"),
            ));
        }
        let found = query(
            &character_select(&format!(
                "WHERE {condition} ORDER BY c.name LIMIT {MAX_FOUND}"
            )),
            &params,
        )?;
        for r in &found {
            let id = int(r, 0);
            let owner = access.owner(id);
            let main = owner.map(|o| (&o.main, o.state.name.clone()));
            rows.push((
                text(r, 1),
                Found {
                    id,
                    name: text(r, 1),
                    corporation: (int(r, 2), text(r, 11)),
                    alliance: opt_int(r, 3).filter(|a| *a > 0).map(|a| (a, text(r, 12))),
                    owner: main,
                    status: if boolean(r, 10) {
                        badge("Shared", Tone::Neutral).into()
                    } else {
                        "".into()
                    },
                    read: Some(vec![
                        if opt_int(r, 6).is_some() {
                            text(r, 13).into()
                        } else {
                            "".into()
                        },
                        match opt_int(r, 7) {
                            Some(ship) => item_type(ship, text(r, 14)).into(),
                            None => "".into(),
                        },
                        int(r, 4).into(),
                        time_or_blank(r, 8),
                    ]),
                },
            ));
        }
    }
    if filters.unregistered != Some(false) {
        for (member, c) in &others {
            let ch = &c.character;
            let is_main = member.main.id == ch.id;
            if filters.corporation.is_some_and(|x| x != ch.corporation_id)
                || filters.alliance.is_some_and(|x| Some(x) != ch.alliance_id)
                || !filters.owner_passes(&member.main, &member.state.name, is_main)
            {
                continue;
            }
            if !q.is_empty()
                && !ch.name.to_lowercase().contains(&q)
                && !named(ch.corporation_id).to_lowercase().contains(&q)
                && !ch
                    .alliance_id
                    .is_some_and(|a| named(a).to_lowercase().contains(&q))
                && !member.main.name.to_lowercase().contains(&q)
            {
                continue;
            }
            rows.push((
                ch.name.clone(),
                Found {
                    id: ch.id,
                    name: ch.name.clone(),
                    corporation: (ch.corporation_id, named(ch.corporation_id)),
                    alliance: ch.alliance_id.map(|a| (a, named(a))),
                    owner: Some((&member.main, member.state.name.clone())),
                    status: if c.registered {
                        badge("Not read yet", Tone::Neutral).into()
                    } else {
                        badge("Unregistered", Tone::Warning).into()
                    },
                    read: None,
                },
            ));
        }
    }
    rows.sort_by_key(|(name, _)| name.to_lowercase());
    let cut = rows.len() > MAX_FOUND;
    rows.truncate(MAX_FOUND);

    let mut table = Table::new(vec![
        Column::text("Character"),
        Column::text(""),
        Column::text("Corporation"),
        Column::text("Alliance"),
        Column::text("Main"),
        Column::text("Main?"),
        Column::text("Main organisation"),
        Column::text("State"),
        Column::text("Location"),
        Column::text("Ship"),
        Column::numeric("Skill points"),
        Column::numeric("Last update"),
    ])
    .title(match (q.is_empty(), cut) {
        (true, false) => "Characters".to_owned(),
        (false, false) => format!("Characters matching \"{q}\""),
        (_, true) => format!("The first {MAX_FOUND} characters by name: search or filter for more"),
    })
    .empty("No characters match.");
    for (_, row) in rows {
        table = table.row(row.cells(access, &named));
    }

    // aa-memberaudit's filters.
    let mut corporation_choices: Vec<(String, String)> = corporations
        .iter()
        .map(|(c, _)| (c.to_string(), named(*c)))
        .collect();
    let mut alliance_choices: Vec<(String, String)> = corporations
        .iter()
        .filter_map(|(_, a)| *a)
        .map(|a| (a.to_string(), named(a)))
        .collect();
    let mut main_corporations: Vec<(String, String)> = mains
        .iter()
        .map(|m| (m.corporation_id.to_string(), named(m.corporation_id)))
        .collect();
    let mut main_alliances: Vec<(String, String)> = mains
        .iter()
        .filter_map(|m| m.alliance_id)
        .map(|a| (a.to_string(), named(a)))
        .collect();
    let mut states: Vec<(String, String)> = access
        .owners_in_scope(true)
        .map(|o| &o.state.name)
        .chain(access.members_in_scope().map(|m| &m.state.name))
        .map(|s| (s.clone(), s.clone()))
        .collect();
    for list in [
        &mut corporation_choices,
        &mut alliance_choices,
        &mut main_corporations,
        &mut main_alliances,
        &mut states,
    ] {
        list.sort_by_key(|(value, label)| (label.to_lowercase(), value.clone()));
        list.dedup();
        list.truncate(100);
    }
    let yes_no = || {
        vec![
            ("yes".to_owned(), "Yes".to_owned()),
            ("no".to_owned(), "No".to_owned()),
        ]
    };
    let mut toolbar = Toolbar::new().search("Search characters, corporations, alliances, mains");
    for (param, label, choices) in [
        ("state", "State", states),
        ("corporation", "Corporation", corporation_choices),
        ("alliance", "Alliance", alliance_choices),
        ("main_corporation", "Main corporation", main_corporations),
        ("main_alliance", "Main alliance", main_alliances),
        ("main", "Main", yes_no()),
        ("unregistered", "Unregistered", yes_no()),
    ] {
        if !choices.is_empty() {
            toolbar = toolbar.filter(param, label, choices);
        }
    }
    Ok(Page::new("Character finder")
        .description(format!(
            "The characters of {}{}",
            access.scope_words(),
            if access.shared {
                ", and characters their pilots share"
            } else {
                ""
            }
        ))
        .stats(vec![
            Stat::new("Registered", total).caption("in your scope"),
            Stat::new("Unregistered", count(unregistered)).caption("of the pilots in your scope"),
        ])
        .toolbar(toolbar)
        .table(table))
}

/// Characters the Finder lists, at most (the host's rows per table).
const MAX_FOUND: usize = 500;

/// aa-memberaudit's Finder filters, from the address.
struct FinderFilters {
    state: Option<String>,
    corporation: Option<i64>,
    alliance: Option<i64>,
    main_corporation: Option<i64>,
    main_alliance: Option<i64>,
    is_main: Option<bool>,
    unregistered: Option<bool>,
}

impl FinderFilters {
    fn of(request: &Request) -> Self {
        let id = |name: &str| request.param(name).parse::<i64>().ok();
        let yes = |name: &str| match request.param(name) {
            "yes" => Some(true),
            "no" => Some(false),
            _ => None,
        };
        Self {
            state: Some(request.param("state").to_owned()).filter(|s| !s.is_empty()),
            corporation: id("corporation"),
            alliance: id("alliance"),
            main_corporation: id("main_corporation"),
            main_alliance: id("main_alliance"),
            is_main: yes("main"),
            unregistered: yes("unregistered"),
        }
    }

    /// Whether any filter goes by the character's owner.
    fn by_owner(&self) -> bool {
        self.state.is_some()
            || self.main_corporation.is_some()
            || self.main_alliance.is_some()
            || self.is_main.is_some()
    }

    /// Whether a character with this main and state passes the filters on
    /// its owner.
    fn owner_passes(&self, main: &Character, state: &str, is_main: bool) -> bool {
        self.state.as_deref().is_none_or(|s| s == state)
            && self
                .main_corporation
                .is_none_or(|c| c == main.corporation_id)
            && self
                .main_alliance
                .is_none_or(|a| Some(a) == main.alliance_id)
            && self.is_main.is_none_or(|m| m == is_main)
    }
}

/// A row of the Finder, before it's drawn.
struct Found<'a> {
    id: i64,
    name: String,
    corporation: (i64, String),
    alliance: Option<(i64, String)>,
    /// Its owner's main and state, if Tether says.
    owner: Option<(&'a Character, String)>,
    /// Shared, unregistered, not read yet.
    status: Value,
    /// Location, ship, skill points and last update, for a character
    /// Member Audit has read; none for one it hasn't (no sheet to open).
    read: Option<Vec<Value>>,
}

impl Found<'_> {
    fn cells(self, access: &Access, named: &dyn Fn(i64) -> String) -> Vec<Value> {
        let id = self.id;
        let (main, is_main, organisation, state) = match self.owner {
            Some((main, state)) => (
                character(main.id, main.name.clone()).into(),
                if main.id == id {
                    badge("Main", Tone::Neutral).into()
                } else {
                    "".into()
                },
                corporation(main.corporation_id, named(main.corporation_id)).into(),
                state.into(),
            ),
            // Tether names the main and state only of characters that
            // serve the app now: one whose registration or a scope lapsed
            // says so, rather than three blanks.
            None => (
                "".into(),
                "".into(),
                "".into(),
                badge("Not registered now", Tone::Warning).into(),
            ),
        };
        let (corp, corp_name) = self.corporation;
        let mut row = vec![
            // The name opens the character's sheet, for those who may
            // open it.
            if self.read.is_some() && access.may_open(id) {
                character(id, self.name)
                    .link(format!("character/{id}"))
                    .into()
            } else {
                character(id, self.name).into()
            },
            self.status,
            corporation(corp, corp_name).into(),
            match self.alliance {
                Some((a, n)) => alliance(a, n).into(),
                None => "".into(),
            },
            main,
            is_main,
            organisation,
            state,
        ];
        row.extend(
            self.read
                .unwrap_or_else(|| vec!["".into(), "".into(), "".into(), "".into()]),
        );
        row
    }
}

/// Names for corporations and alliances (a main's may be no member
/// character's):
/// stored, else asked of ESI (a page can't store them), else none.
pub(crate) fn names_of(mut ids: Vec<i64>) -> Result<BTreeMap<i64, String>, PageError> {
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
