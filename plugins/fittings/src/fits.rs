//! Fits: All Fits, a fit's page, and adding, editing and deleting them.

use std::collections::BTreeMap;

use tether_plugin_sdk::identity::Viewer;
use tether_plugin_sdk::storage::{self, Statement, Value as Db};
use tether_plugin_sdk::{
    Card, CodeBlock, Column, Field, Form, Page, PageError, Profile, Stat, Submission, SubmitResult,
    Table, Tone, Value, action, badge, character, item_type, link, log, time,
};

use crate::eft::{self, Problem, Slot};
use crate::lookup::{self, Failure};
use crate::{
    Access, app_links, category_names, clip, doctrine_seen, execute, failed, fit_seen, id_list,
    int, opt_int, query, text,
};

/// Form limits (AA's: a fit's description is at most 500 characters).
const MAX_EFT: u32 = 10_000;
/// Stored EFT, at most (the host shows 16 KiB of text to copy).
const MAX_EFT_BYTES: usize = 16_000;
const MAX_ROLE: u32 = 40;
const MAX_DESCRIPTION: u32 = 500;
/// Rows in All Fits (the host's limit per table).
const LIST_ROWS: i64 = 500;
/// Options in a select (the host's limit), one of them "none".
const SELECT_OPTIONS: i64 = 99;

/// The categories a fit is in (its own and its doctrines') that the viewer
/// sees, for [`category_names`]: `$3` is the fits.
pub(crate) fn fit_categories_sql() -> String {
    format!(
        "SELECT sf2.fit_id, c.name FROM fit_categories sf2 JOIN categories c ON c.id = sf2.category_id \
         WHERE sf2.fit_id = ANY(string_to_array($3, ',')::bigint[]) AND {} \
         ORDER BY lower(c.name), c.id",
        crate::CATEGORY_SEEN
    )
}

// ---- where an item goes ---------------------------------------------------------

/// The groups a fit's page shows, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Place {
    High,
    Mid,
    Low,
    Rig,
    Subsystem,
    Service,
    Drones,
    Fighters,
    Cargo,
    Implants,
}

impl Place {
    fn title(self) -> &'static str {
        match self {
            Place::High => "High slots",
            Place::Mid => "Mid slots",
            Place::Low => "Low slots",
            Place::Rig => "Rigs",
            Place::Subsystem => "Subsystems",
            Place::Service => "Service slots",
            Place::Drones => "Drones",
            Place::Fighters => "Fighters",
            Place::Cargo => "Cargo",
            Place::Implants => "Implants and boosters",
        }
    }

    /// Modules fitted in slots (with charges and an online state).
    fn fitted(self) -> bool {
        self <= Place::Service
    }

    /// Counts toward the fit's required skills (cargo doesn't).
    fn needs_skills(self) -> bool {
        self != Place::Cargo
    }
}

/// EVE's item categories that settle where an item after the rigs goes.
const SUBSYSTEM: i64 = 32;
const STRUCTURE_MODULE: i64 = 66;
const DRONE: i64 = 18;
const FIGHTER: i64 = 87;
const IMPLANT: i64 = 20;

/// Where an item goes: the EFT's slot sections as they are; after the rigs,
/// its category once known, else the section order's guess.
pub(crate) fn placement(slot: Slot, category: Option<i64>) -> Place {
    match slot {
        Slot::Low => Place::Low,
        Slot::Mid => Place::Mid,
        Slot::High => Place::High,
        Slot::Rig => Place::Rig,
        _ => match category {
            Some(SUBSYSTEM) => Place::Subsystem,
            Some(STRUCTURE_MODULE) => Place::Service,
            Some(DRONE) => Place::Drones,
            Some(FIGHTER) => Place::Fighters,
            Some(IMPLANT) => Place::Implants,
            Some(_) => Place::Cargo,
            None => match slot {
                Slot::Subsystem => Place::Subsystem,
                Slot::Service => Place::Service,
                Slot::Other => Place::Implants,
                _ => Place::Cargo,
            },
        },
    }
}

// ---- reading ------------------------------------------------------------------

struct Fit {
    name: String,
    hull_id: i64,
    hull: String,
    role: String,
    description: String,
    eft: String,
    creator_id: i64,
    creator: String,
    created_at: String,
    updated_at: String,
}

/// A fit the viewer may see, or not found.
fn seen_fit(access: &Access, id: i64) -> Result<Fit, PageError> {
    query(
        &format!(
            "SELECT f.name, f.hull_type_id, t.name, f.role, f.description, f.eft, \
             f.created_by_character_id, f.created_by_name, f.created_at, f.updated_at \
             FROM fits f JOIN types t ON t.type_id = f.hull_type_id \
             WHERE f.id = $3 AND {}",
            fit_seen()
        ),
        &access.with(vec![id.into()]),
    )?
    .first()
    .map(|r| Fit {
        name: text(r, 0),
        hull_id: int(r, 1),
        hull: text(r, 2),
        role: text(r, 3),
        description: text(r, 4),
        eft: text(r, 5),
        creator_id: int(r, 6),
        creator: text(r, 7),
        created_at: text(r, 8),
        updated_at: text(r, 9),
    })
    .ok_or(PageError::NotFound)
}

/// One line of the fit, as stored, with what's known of its type.
struct Line {
    place: Place,
    type_id: i64,
    name: String,
    charge: Option<(i64, String)>,
    quantity: i64,
    offline: bool,
}

fn lines(fit: i64) -> Result<Vec<Line>, PageError> {
    Ok(query(
        "SELECT i.slot, i.type_id, t.name, i.charge_type_id, c.name, i.quantity, i.offline, \
         g.category_id FROM fit_items i JOIN types t ON t.type_id = i.type_id \
         LEFT JOIN types c ON c.type_id = i.charge_type_id \
         LEFT JOIN item_groups g ON g.group_id = t.group_id \
         WHERE i.fit_id = $1 ORDER BY i.position",
        &[fit.into()],
    )?
    .iter()
    .map(|r| Line {
        place: placement(
            Slot::parse(&text(r, 0)).unwrap_or(Slot::Other),
            opt_int(r, 7),
        ),
        type_id: int(r, 1),
        name: text(r, 2),
        charge: opt_int(r, 3).map(|id| (id, text(r, 4))),
        quantity: int(r, 5),
        offline: r.get(6).and_then(Db::as_bool).unwrap_or_default(),
    })
    .collect())
}

/// The doctrines a fit is in that the viewer sees: id, name, description.
fn doctrines_of(access: &Access, fit: i64) -> Result<Vec<(i64, String, String)>, PageError> {
    Ok(query(
        &format!(
            "SELECT d.id, d.name, d.description FROM doctrine_fits df \
             JOIN doctrines d ON d.id = df.doctrine_id \
             WHERE df.fit_id = $3 AND {} ORDER BY lower(d.name), d.id",
            doctrine_seen()
        ),
        &access.with(vec![fit.into()]),
    )?
    .iter()
    .map(|r| (int(r, 0), text(r, 1), text(r, 2)))
    .collect())
}

// ---- All Fits -----------------------------------------------------------------

pub(crate) fn list(access: &Access, q: &str) -> Result<Page, PageError> {
    let q: String = q.trim().to_lowercase().chars().take(100).collect();
    let rows = query(
        &format!(
            "SELECT f.id, f.name, f.hull_type_id, t.name, f.role, f.updated_at, \
             coalesce((SELECT string_agg(d.name, ', ' ORDER BY lower(d.name)) \
                       FROM doctrine_fits df JOIN doctrines d ON d.id = df.doctrine_id \
                       WHERE df.fit_id = f.id AND {doctrine}), '') \
             FROM fits f JOIN types t ON t.type_id = f.hull_type_id \
             WHERE {fit} AND ($3 = '' OR strpos(lower(f.name), $3) > 0 \
                OR strpos(lower(t.name), $3) > 0 OR strpos(lower(f.role), $3) > 0 \
                OR EXISTS (SELECT 1 FROM doctrine_fits df JOIN doctrines d ON d.id = df.doctrine_id \
                           WHERE df.fit_id = f.id AND {doctrine} AND strpos(lower(d.name), $3) > 0) \
                OR EXISTS (SELECT 1 FROM fit_categories sf2 JOIN categories c ON c.id = sf2.category_id \
                           WHERE sf2.fit_id = f.id AND {category} AND strpos(lower(c.name), $3) > 0)) \
             ORDER BY lower(t.name), lower(f.name), f.id LIMIT $4",
            doctrine = doctrine_seen(),
            fit = fit_seen(),
            category = crate::CATEGORY_SEEN,
        ),
        &access.with(vec![q.clone().into(), LIST_ROWS.into()]),
    )?;
    let ids: Vec<i64> = rows.iter().map(|r| int(r, 0)).collect();
    let categories = category_names(access, &fit_categories_sql(), &ids)?;
    let mut columns = vec![
        Column::text("Hull"),
        Column::text("Fit"),
        Column::text("Role"),
        Column::text("Categories"),
        Column::text("Doctrines"),
        Column::numeric("Updated"),
    ];
    if access.manage {
        columns.push(Column::text("Action"));
        columns.push(Column::text(""));
    }
    let mut table = Table::new(columns)
        .title(if q.is_empty() {
            "Fits".to_owned()
        } else {
            format!("Fits matching \"{}\"", clip(&q, 100))
        })
        .empty(if q.is_empty() {
            "No fits yet."
        } else {
            "No fits match."
        });
    for r in &rows {
        let id = int(r, 0);
        let mut row: Vec<Value> = vec![
            item_type(int(r, 2), text(r, 3)).into(),
            link(clip(&text(r, 1), 200), format!("fit/{id}")).into(),
            clip(&text(r, 4), 200).into(),
            categories.get(&id).cloned().unwrap_or_default().into(),
            clip(&text(r, 6), 1000).into(),
            time(text(r, 5)),
        ];
        if access.manage {
            row.push(link("Edit", format!("edit/fit/{id}")).into());
            row.push(delete_button(id, &text(r, 1), &text(r, 3)));
        }
        table = table.row(row);
    }
    let total = query(
        &format!("SELECT count(*) FROM fits f WHERE {}", fit_seen()),
        &access.params(),
    )?
    .first()
    .map_or(0, |r| int(r, 0));
    Ok(app_links(
        Page::new("All Fits").description("Every fit, by hull. Open one to copy it into EVE."),
        access,
        None,
    )
    .stats(vec![Stat::new("Fits", total)])
    .form(
        Form::new("search", "Search").field(
            Field::text("q", "Search", 100)
                .value(q)
                .help("Part of a fit's name, hull, role, doctrine or category."),
        ),
    )
    .table(table))
}

/// A fit's Delete, asking first.
fn delete_button(id: i64, name: &str, hull: &str) -> Value {
    action("Delete", "delete_fit")
        .field("fit", id.to_string())
        .tone(Tone::Danger)
        .confirm(clip(
            &format!(
                "The {hull} fit \"{name}\" is deleted, and taken out of its doctrines and categories."
            ),
            400,
        ))
        .into()
}

// ---- a fit's page ---------------------------------------------------------------

/// A group of the same item (and charge, and state) in one place.
struct Stack {
    type_id: i64,
    name: String,
    charge: Option<(i64, String)>,
    offline: bool,
    quantity: i64,
}

/// The lines by place, identical ones stacked, in the order pasted.
fn by_place(lines: &[Line]) -> BTreeMap<Place, Vec<Stack>> {
    let mut places: BTreeMap<Place, Vec<Stack>> = BTreeMap::new();
    for line in lines {
        let stacks = places.entry(line.place).or_default();
        let charge = line.charge.as_ref().map(|(id, _)| *id);
        match stacks.iter_mut().find(|s| {
            s.type_id == line.type_id
                && s.charge.as_ref().map(|(id, _)| *id) == charge
                && s.offline == line.offline
        }) {
            Some(stack) => stack.quantity += line.quantity,
            None => stacks.push(Stack {
                type_id: line.type_id,
                name: line.name.clone(),
                charge: line.charge.clone(),
                offline: line.offline,
                quantity: line.quantity,
            }),
        }
    }
    places
}

fn place_table(place: Place, stacks: &[Stack]) -> Table {
    if place.fitted() {
        let mut table = Table::new(vec![
            Column::text("Module"),
            Column::text("Charge"),
            Column::numeric("Count"),
            Column::text("State"),
        ])
        .title(place.title());
        for s in stacks {
            table = table.row(vec![
                item_type(s.type_id, s.name.clone()).into(),
                s.charge.as_ref().map_or_else(
                    || "".into(),
                    |(id, name)| item_type(*id, name.clone()).into(),
                ),
                s.quantity.into(),
                if s.offline {
                    badge("Offline", Tone::Warning).into()
                } else {
                    "".into()
                },
            ]);
        }
        table
    } else {
        let mut table = Table::new(vec![Column::text("Item"), Column::numeric("Quantity")])
            .title(place.title());
        for s in stacks {
            table = table.row(vec![
                item_type(s.type_id, s.name.clone()).into(),
                s.quantity.into(),
            ]);
        }
        table
    }
}

/// The highest level of each skill the hull and everything but cargo
/// need, by skill name.
fn required_skills(fit: &Fit, lines: &[Line]) -> Result<Vec<(i64, String, i64)>, PageError> {
    let mut ids = vec![fit.hull_id];
    for line in lines.iter().filter(|l| l.place.needs_skills()) {
        ids.push(line.type_id);
        ids.extend(line.charge.as_ref().map(|(id, _)| *id));
    }
    ids.sort_unstable();
    ids.dedup();
    let rows = query(
        "SELECT (s->>0)::bigint, max((s->>1)::bigint), coalesce(max(k.name), '') \
         FROM types t, jsonb_array_elements(t.skills) s \
         LEFT JOIN types k ON k.type_id = (s->>0)::bigint \
         WHERE t.type_id = ANY(string_to_array($1, ',')::bigint[]) \
         GROUP BY 1 ORDER BY 3, 1",
        &[id_list(&ids).into()],
    )?;
    Ok(rows
        .iter()
        .map(|r| (int(r, 0), text(r, 2), int(r, 1)))
        .collect())
}

fn roman(level: i64) -> &'static str {
    match level {
        1 => "I",
        2 => "II",
        3 => "III",
        4 => "IV",
        _ => "V",
    }
}

pub(crate) fn page(access: &Access, id: i64) -> Result<Page, PageError> {
    let fit = seen_fit(access, id)?;
    let lines = lines(id)?;
    let doctrines = doctrines_of(access, id)?;
    let categories = category_names(access, &fit_categories_sql(), &[id])?;
    let mut types: Vec<i64> = std::iter::once(fit.hull_id)
        .chain(lines.iter().map(|l| l.type_id))
        .chain(
            lines
                .iter()
                .filter_map(|l| l.charge.as_ref().map(|(c, _)| *c)),
        )
        .collect();
    types.sort_unstable();
    types.dedup();
    let pending = lookup::pending(&types);

    let mut profile = Profile::new(item_type(fit.hull_id, fit.hull.clone()))
        .fact("Fit", clip(&fit.name, 200))
        .fact(
            "Categories",
            categories
                .get(&id)
                .cloned()
                .unwrap_or_else(|| "None".to_owned()),
        )
        .fact("Created by", character(fit.creator_id, fit.creator.clone()))
        .fact("Created", time(fit.created_at.clone()))
        .fact("Updated", time(fit.updated_at.clone()));
    if !fit.role.is_empty() {
        profile = profile.subtitle(clip(&fit.role, 200));
    }
    let page = Page::new(clip(&fit.name, 200)).description(if fit.role.is_empty() {
        fit.hull.clone()
    } else {
        clip(&format!("{} · {}", fit.hull, fit.role), 400)
    });
    let edit = format!("edit/fit/{id}");
    let mut page = app_links(page, access, Some(("Edit Fit", &edit))).profile(profile);
    if pending {
        // Filled in by the `details` job, usually within seconds.
        page = page
            .text("Item details and required skills are still being looked up; this page fills in by itself.")
            .refresh(15);
    }
    for (place, stacks) in &by_place(&lines) {
        page = page.table(place_table(*place, stacks));
    }
    if lines.is_empty() {
        page = page.text("This fit is just the hull.");
    }
    page = page.code(
        CodeBlock::new(fit.eft.clone())
            .title("EFT")
            .copy_label("Copy EFT"),
    );
    if !fit.description.is_empty() {
        page = page.card(Card::new("Notes").description(clip(&fit.description, 2000)));
    }
    let mut in_doctrines = Table::new(vec![Column::text("Doctrine"), Column::text("About")])
        .title("Doctrines")
        .empty("This fit isn't in a doctrine.");
    for (doctrine, name, about) in &doctrines {
        in_doctrines = in_doctrines.row(vec![
            link(clip(name, 200), format!("doctrine/{doctrine}")).into(),
            clip(about, 300).into(),
        ]);
    }
    page = page.table(in_doctrines);
    let skills = required_skills(&fit, &lines)?;
    let mut skill_table = Table::new(vec![Column::text("Skill"), Column::numeric("Level")])
        .title("Required skills")
        .empty(if pending {
            "Still being looked up."
        } else {
            "None found."
        });
    for (skill, name, level) in &skills {
        let name = if name.is_empty() {
            format!("Skill {skill}")
        } else {
            name.clone()
        };
        skill_table = skill_table.row(vec![item_type(*skill, name).into(), roman(*level).into()]);
    }
    Ok(page.table(skill_table).text(
        "Required skills: the highest level of each skill the hull, modules, charges, drones, \
         fighters and implants need (not cargo), without the skills those need in turn.",
    ))
}

// ---- adding and editing -------------------------------------------------------------

/// What the fit form shows: stored, or as just posted.
#[derive(Default)]
pub(crate) struct Values {
    eft: String,
    role: String,
    description: String,
    doctrine: String,
}

impl Values {
    fn posted(submission: &Submission) -> Self {
        Self {
            eft: submission.value("eft").to_owned(),
            role: submission.value("role").to_owned(),
            description: submission.value("description").to_owned(),
            doctrine: submission.value("doctrine").to_owned(),
        }
    }
}

fn fit_form(values: &Values, submit: &str, doctrines: Option<Vec<(String, String)>>) -> Form {
    let mut form = Form::new("fit", submit)
        .description(
            "Paste the fit as EVE copies it (Fitting window, Save Fitting, Copy to Clipboard) or as \
             Pyfa exports it. Its header, [Hull, Name], names it.",
        )
        .field(
            Field::textarea("eft", "EFT", MAX_EFT)
                .required()
                .value(values.eft.clone()),
        )
        .field(
            Field::text("role", "Role", MAX_ROLE)
                .help("Optional, e.g. DPS, Logistics, Tackle.")
                .value(values.role.clone()),
        )
        .field(
            Field::textarea("description", "Notes", MAX_DESCRIPTION)
                .help("How to fly it, alternatives, what to bring.")
                .value(values.description.clone()),
        );
    if let Some(options) = doctrines {
        form = form.field(
            Field::select("doctrine", "Doctrine", options)
                .help("More from the doctrine's own page.")
                .value(values.doctrine.clone()),
        );
    }
    form
}

/// Doctrines to pick from (a manager sees them all), but those the fit is
/// already in, with "none" first.
fn doctrine_options(fit: i64) -> Result<Vec<(String, String)>, PageError> {
    let mut options = vec![(String::new(), "No doctrine".to_owned())];
    options.extend(
        query(
            "SELECT d.id, d.name FROM doctrines d WHERE NOT EXISTS \
             (SELECT 1 FROM doctrine_fits df WHERE df.doctrine_id = d.id AND df.fit_id = $1) \
             ORDER BY lower(d.name), d.id LIMIT $2",
            &[fit.into(), SELECT_OPTIONS.into()],
        )?
        .iter()
        .map(|r| (int(r, 0).to_string(), clip(&text(r, 1), 200))),
    );
    Ok(options)
}

/// What's wrong with a posted fit, line by line.
fn problems_table(problems: &[Problem]) -> Table {
    let mut table = Table::new(vec![Column::numeric("Line"), Column::text("Problem")])
        .title("The fit wasn't saved");
    for p in problems {
        table = table.row(vec![
            if p.line == 0 {
                "".into()
            } else {
                i64::try_from(p.line).unwrap_or_default().into()
            },
            clip(&p.text, 1000).into(),
        ]);
    }
    table
}

pub(crate) fn add_page(
    access: &Access,
    again: Option<(&[Problem], Values)>,
) -> Result<Page, PageError> {
    let mut page = app_links(
        Page::new("Add Fit").description("A new fit, from its EFT text"),
        access,
        None,
    );
    let values = match again {
        Some((problems, values)) => {
            page = page.table(problems_table(problems));
            values
        }
        None => Values::default(),
    };
    Ok(page.form(fit_form(&values, "Add Fit", Some(doctrine_options(0)?))))
}

pub(crate) fn edit_page(
    access: &Access,
    id: i64,
    again: Option<(&[Problem], Values)>,
) -> Result<Page, PageError> {
    let fit = seen_fit(access, id)?;
    let doctrines = doctrines_of(access, id)?;
    let mut page = app_links(
        Page::new(format!("Edit {}", clip(&fit.name, 200)))
            .description(fit.hull.clone())
            .link("Fit", format!("fit/{id}")),
        access,
        None,
    );
    let values = match again {
        Some((problems, values)) => {
            page = page.table(problems_table(problems));
            values
        }
        None => Values {
            eft: fit.eft.clone(),
            role: fit.role.clone(),
            description: fit.description.clone(),
            doctrine: String::new(),
        },
    };
    page = page.form(fit_form(&values, "Save Fit", None));
    let options = doctrine_options(id)?;
    if options.len() > 1 {
        page = page.form(
            Form::new("add_doctrine", "Add to Doctrine")
                .field(Field::select("doctrine", "Doctrine", options[1..].to_vec()).required()),
        );
    }
    let mut table = Table::new(vec![Column::text("Doctrine"), Column::text("")])
        .title("Doctrines")
        .empty("This fit isn't in a doctrine.");
    for (doctrine, name, _) in &doctrines {
        table = table.row(vec![
            link(clip(name, 200), format!("doctrine/{doctrine}")).into(),
            action("Remove", "remove_doctrine")
                .field("doctrine", doctrine.to_string())
                .confirm(clip(
                    &format!("\"{}\" is taken out of the {name} doctrine.", fit.name),
                    400,
                ))
                .into(),
        ]);
    }
    Ok(page.table(table).card(
        Card::new("Delete this fit")
            .description("It's taken out of its doctrines and categories too.")
            .field("", delete_button(id, &fit.name, &fit.hull)),
    ))
}

/// The form again, with what's wrong.
fn again(
    access: &Access,
    id: Option<i64>,
    problems: &[Problem],
    submission: &Submission,
) -> Result<SubmitResult, PageError> {
    let values = Values::posted(submission);
    Ok(SubmitResult::Page(match id {
        Some(id) => edit_page(access, id, Some((problems, values)))?,
        None => add_page(access, Some((problems, values)))?,
    }))
}

/// A fit's lines, stored from JSON rows (see `save`).
const ITEM_COLUMNS: &str = "position, slot, type_id, charge_type_id, quantity, offline";
const ITEM_VALUES: &str = "x.position, x.slot, x.type_id, x.charge_type_id, x.quantity, x.offline";
const ITEM_RECORD: &str = "x(position integer, slot text, type_id bigint, \
                           charge_type_id bigint, quantity integer, offline boolean)";

pub(crate) fn save(
    access: &Access,
    viewer: &Viewer,
    id: Option<i64>,
    submission: &Submission,
) -> Result<SubmitResult, PageError> {
    let one = |text: &str| {
        vec![Problem {
            line: 0,
            text: text.to_owned(),
        }]
    };
    if let Some(id) = id {
        seen_fit(access, id)?;
    }
    let text = eft::normalise(submission.value("eft"));
    if text.len() > MAX_EFT_BYTES {
        return again(access, id, &one("That EFT text is too long."), submission);
    }
    let parsed = match eft::parse(&text) {
        Ok(parsed) => parsed,
        Err(problems) => return again(access, id, &problems, submission),
    };
    let mut names = vec![parsed.hull.clone()];
    for item in &parsed.items {
        names.push(item.name.clone());
        names.extend(item.charge.clone());
    }
    let found = match lookup::resolve(&names) {
        Ok(found) => found,
        Err(Failure::TooMany) => {
            return again(
                access,
                id,
                &one("That fit names too many different items to look up at once."),
                submission,
            );
        }
        Err(Failure::Unavailable(why)) => {
            log::warn(format!("looking up item names: {why}"));
            return again(
                access,
                id,
                &one("EVE's item lookup didn't answer. Try again in a minute."),
                submission,
            );
        }
    };
    let lookup_id = |name: &str| found.get(&name.to_lowercase()).map(|(id, _)| *id);
    let unknown = |line: usize, name: &str| Problem {
        line,
        text: format!("\"{name}\" isn't an item EVE knows: check its spelling."),
    };
    let mut problems = Vec::new();
    let hull = lookup_id(&parsed.hull);
    if hull.is_none() {
        problems.push(unknown(1, &parsed.hull));
    }
    let mut items = Vec::new();
    for (position, item) in parsed.items.iter().enumerate() {
        let type_id = lookup_id(&item.name);
        if type_id.is_none() {
            problems.push(unknown(item.line, &item.name));
        }
        let charge = match &item.charge {
            Some(charge) => {
                let charge_id = lookup_id(charge);
                if charge_id.is_none() {
                    problems.push(unknown(item.line, charge));
                }
                charge_id
            }
            None => None,
        };
        items.push(serde_json::json!({
            "position": position,
            "slot": item.slot.as_str(),
            "type_id": type_id,
            "charge_type_id": charge,
            "quantity": item.quantity,
            "offline": item.offline,
        }));
    }
    let Some(hull) = hull.filter(|_| problems.is_empty()) else {
        problems.truncate(20);
        return again(access, id, &problems, submission);
    };
    // As EVE writes the hull's name.
    let hull_name = found
        .get(&parsed.hull.to_lowercase())
        .map_or_else(|| parsed.hull.clone(), |(_, n)| n.clone());
    let role = submission.value("role").trim().to_owned();
    let description = submission.value("description").trim().to_owned();
    let items = Db::json(serde_json::Value::Array(items).to_string());
    let saved = match id {
        None => {
            // An empty or unknown doctrine is none.
            let doctrine: Option<i64> = submission.value("doctrine").parse().ok();
            storage::query(
                &format!(
                    "WITH f AS (INSERT INTO fits (name, hull_type_id, role, description, eft, \
                       created_by_character_id, created_by_name) \
                       VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING id), \
                     items AS (INSERT INTO fit_items (fit_id, {ITEM_COLUMNS}) \
                       SELECT f.id, {ITEM_VALUES} FROM f, json_to_recordset($8::json) AS {ITEM_RECORD}), \
                     joined AS (INSERT INTO doctrine_fits (doctrine_id, fit_id) \
                       SELECT d.id, f.id FROM f, doctrines d WHERE d.id = $9) \
                     SELECT id FROM f"
                ),
                &[
                    parsed.name.clone().into(),
                    hull.into(),
                    role.into(),
                    description.into(),
                    text.clone().into(),
                    viewer.main.id.into(),
                    viewer.main.name.clone().into(),
                    items,
                    doctrine.into(),
                ],
            )
            .map(|r| r.rows.first().map_or(0, |r| int(r, 0)))
        }
        Some(id) => storage::transaction(&[
            Statement::new(
                "UPDATE fits SET name = $2, hull_type_id = $3, role = $4, description = $5, \
                 eft = $6, updated_at = now() WHERE id = $1",
                vec![
                    id.into(),
                    parsed.name.clone().into(),
                    hull.into(),
                    role.into(),
                    description.into(),
                    text.clone().into(),
                ],
            ),
            Statement::new("DELETE FROM fit_items WHERE fit_id = $1", vec![id.into()]),
            Statement::new(
                format!(
                    "INSERT INTO fit_items (fit_id, {ITEM_COLUMNS}) SELECT $1, {ITEM_VALUES} \
                     FROM json_to_recordset($2::json) AS {ITEM_RECORD}"
                ),
                vec![id.into(), items],
            ),
        ])
        .map(|_| id),
    };
    let saved = match saved {
        Ok(saved) => saved,
        Err(storage::Error::Database(e)) if e.code == "23505" => {
            return again(
                access,
                id,
                &one(&format!(
                    "There's already a {hull_name} fit named \"{}\": give this one another name in its header.",
                    parsed.name
                )),
                submission,
            );
        }
        Err(e) => return Err(failed("saving the fit", e)),
    };
    log::info(format!(
        "fit {saved} ({hull_name}, {}) {} by {} ({})",
        parsed.name,
        if id.is_some() { "edited" } else { "added" },
        viewer.main.name,
        viewer.main.id
    ));
    lookup::queue_details();
    Ok(SubmitResult::Redirect(format!("fit/{saved}")))
}

pub(crate) fn add_to_doctrine(id: i64, doctrine: i64) -> Result<SubmitResult, PageError> {
    execute(
        "adding the fit to a doctrine",
        "INSERT INTO doctrine_fits (doctrine_id, fit_id) SELECT d.id, f.id FROM doctrines d, fits f \
         WHERE d.id = $1 AND f.id = $2 ON CONFLICT DO NOTHING",
        &[doctrine.into(), id.into()],
    )?;
    Ok(SubmitResult::Redirect(format!("edit/fit/{id}")))
}

pub(crate) fn remove_from_doctrine(id: i64, doctrine: i64) -> Result<SubmitResult, PageError> {
    execute(
        "taking the fit out of a doctrine",
        "DELETE FROM doctrine_fits WHERE doctrine_id = $1 AND fit_id = $2",
        &[doctrine.into(), id.into()],
    )?;
    Ok(SubmitResult::Redirect(format!("edit/fit/{id}")))
}

pub(crate) fn delete(viewer: &Viewer, id: i64) -> Result<SubmitResult, PageError> {
    if execute(
        "deleting the fit",
        "DELETE FROM fits WHERE id = $1",
        &[id.into()],
    )? == 0
    {
        return Err(PageError::NotFound);
    }
    log::info(format!(
        "fit {id} deleted by {} ({})",
        viewer.main.name, viewer.main.id
    ));
    Ok(SubmitResult::Redirect("fits".to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn items_after_the_rigs_go_by_category() {
        // The slot sections are the EFT's, whatever the category.
        assert_eq!(placement(Slot::Low, Some(DRONE)), Place::Low);
        assert_eq!(placement(Slot::High, None), Place::High);
        // After the rigs, the category once known.
        assert_eq!(placement(Slot::Bay, Some(DRONE)), Place::Drones);
        assert_eq!(placement(Slot::Bay, Some(FIGHTER)), Place::Fighters);
        assert_eq!(placement(Slot::Bay, Some(8)), Place::Cargo);
        assert_eq!(
            placement(Slot::Subsystem, Some(SUBSYSTEM)),
            Place::Subsystem
        );
        // Pyfa's implants in the fifth section of a hull without subsystems.
        assert_eq!(placement(Slot::Subsystem, Some(IMPLANT)), Place::Implants);
        assert_eq!(
            placement(Slot::Subsystem, Some(STRUCTURE_MODULE)),
            Place::Service
        );
        // Until then, the section order's guess.
        assert_eq!(placement(Slot::Bay, None), Place::Cargo);
        assert_eq!(placement(Slot::Subsystem, None), Place::Subsystem);
        assert_eq!(placement(Slot::Other, None), Place::Implants);
    }

    #[test]
    fn identical_lines_stack() {
        let line = |type_id, charge: Option<i64>, offline| Line {
            place: Place::High,
            type_id,
            name: format!("t{type_id}"),
            charge: charge.map(|c| (c, format!("c{c}"))),
            quantity: 1,
            offline,
        };
        let lines = vec![
            line(1, Some(10), false),
            line(1, Some(10), false),
            line(1, Some(11), false),
            line(1, Some(10), true),
            line(2, None, false),
        ];
        let places = by_place(&lines);
        let high = &places[&Place::High];
        let counts: Vec<i64> = high.iter().map(|s| s.quantity).collect();
        assert_eq!(counts, vec![2, 1, 1, 1]);
        assert_eq!(roman(4), "IV");
    }
}
