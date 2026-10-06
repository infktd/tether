//! Doctrines: the app's front page (AA's dashboard: a card per doctrine),
//! a doctrine's page, and adding, editing and deleting them.

use tether_plugin_sdk::doctrines::Doctrine as SharedDoctrine;
use tether_plugin_sdk::identity::Viewer;
use tether_plugin_sdk::storage::{self, Statement, Value as Db};
use tether_plugin_sdk::{
    Card, CardGrid, Column, Field, Form, Page, PageError, Profile, Stat, Submission, SubmitResult,
    Table, Tone, action, item_type, link, log, time,
};

use crate::fits::fit_categories_sql;
use crate::{
    Access, CATEGORY_SEEN, category_names, clip, doctrine_seen, execute, failed, fit_seen, int,
    opt_int, primary, query, text,
};

/// AA's lengths.
const MAX_NAME: u32 = 255;
const MAX_DESCRIPTION: u32 = 1_000;
/// Cards in the grid (the host's limit).
const CARDS: i64 = 100;
/// Options in a select (the host's limit), one of them "automatic".
const SELECT_OPTIONS: i64 = 99;

/// A doctrine's icon: the hull its designer picked (AA's icon), else its
/// main hull, the one most of its fits are (the first added of those
/// tied), of the fits the viewer sees. `d` is the doctrine; `$1`, `$2`:
/// [`Access::params`].
fn icon() -> String {
    format!(
        "coalesce(d.icon_type_id, (SELECT f.hull_type_id FROM doctrine_fits df \
         JOIN fits f ON f.id = df.fit_id WHERE df.doctrine_id = d.id AND {} \
         GROUP BY f.hull_type_id \
         ORDER BY count(*) DESC, min(df.added_at), f.hull_type_id LIMIT 1))",
        fit_seen()
    )
}

/// Its hulls' names (AA's "Ships"), of the fits the viewer sees.
fn hulls() -> String {
    format!(
        "coalesce((SELECT string_agg(DISTINCT t.name, ', ' ORDER BY t.name) \
         FROM doctrine_fits df JOIN fits f ON f.id = df.fit_id \
         JOIN types t ON t.type_id = f.hull_type_id WHERE df.doctrine_id = d.id AND {}), '')",
        fit_seen()
    )
}

/// The categories of doctrines the viewer sees, for [`category_names`].
fn doctrine_categories_sql() -> String {
    format!(
        "SELECT cd.doctrine_id, c.name FROM category_doctrines cd \
         JOIN categories c ON c.id = cd.category_id \
         WHERE cd.doctrine_id = ANY(string_to_array($3, ',')::bigint[]) AND {CATEGORY_SEEN} \
         ORDER BY lower(c.name), c.id"
    )
}

fn fits_word(n: i64) -> String {
    if n == 1 {
        "1 fit".to_owned()
    } else {
        format!("{n} fits")
    }
}

// ---- the list -----------------------------------------------------------------

pub(crate) fn list(access: &Access) -> Result<Page, PageError> {
    let rows = query(
        &format!(
            "SELECT d.id, d.name, d.description, d.updated_at, \
             (SELECT count(*) FROM doctrine_fits df JOIN fits f ON f.id = df.fit_id \
              WHERE df.doctrine_id = d.id AND {fit}), {icon}, {hulls} \
             FROM doctrines d WHERE {doctrine} ORDER BY lower(d.name), d.id LIMIT $3",
            fit = fit_seen(),
            icon = icon(),
            hulls = hulls(),
            doctrine = doctrine_seen(),
        ),
        &access.with(vec![CARDS.into()]),
    )?;
    let totals = query(
        &format!(
            "SELECT (SELECT count(*) FROM doctrines d WHERE {}), \
                    (SELECT count(*) FROM fits f WHERE {})",
            doctrine_seen(),
            fit_seen()
        ),
        &access.params(),
    )?;
    let (doctrines, fits) = totals.first().map_or((0, 0), |r| (int(r, 0), int(r, 1)));
    let ids: Vec<i64> = rows.iter().map(|r| int(r, 0)).collect();
    let categories = category_names(access, &doctrine_categories_sql(), &ids)?;
    let mut page = primary(
        Page::new("Doctrines").description("Doctrines and the fits in them"),
        access,
        "New doctrine",
        "add-doctrine",
    )
    .stats(vec![
        Stat::new("Doctrines", doctrines),
        Stat::new("Fits", fits),
    ]);
    if rows.is_empty() {
        return Ok(page.text(if access.manage {
            "No doctrines yet. Add fits, then a doctrine to put them in."
        } else {
            "No doctrines yet."
        }));
    }
    let mut grid = CardGrid::new();
    for r in &rows {
        let id = int(r, 0);
        let mut profile = Profile::new(item_type(
            opt_int(r, 5).unwrap_or(0),
            clip(&text(r, 1), 200),
        ))
        .fact("Fits", int(r, 4))
        .fact("Ships", clip(&text(r, 6), 300));
        if let Some(names) = categories.get(&id) {
            profile = profile.fact("Categories", names.clone());
        }
        profile = profile.fact("Updated", time(text(r, 3)));
        let description = text(r, 2);
        if !description.is_empty() {
            profile = profile.subtitle(clip(&description, 200));
        }
        grid = grid.linked(profile, format!("doctrine/{id}"));
    }
    page = page.cards(grid);
    if doctrines > CARDS {
        page = page.text(format!(
            "The first {CARDS} doctrines by name are shown. All fits lists every fit."
        ));
    }
    Ok(page)
}

// ---- a doctrine's page ------------------------------------------------------------

struct Doctrine {
    name: String,
    description: String,
    icon_choice: Option<i64>,
    created_at: String,
    updated_at: String,
    icon: i64,
    hulls: String,
}

fn seen_doctrine(access: &Access, id: i64) -> Result<Doctrine, PageError> {
    let rows = query(
        &format!(
            "SELECT d.name, d.description, d.icon_type_id, d.created_at, d.updated_at, {}, \
             {} FROM doctrines d WHERE d.id = $3 AND {}",
            icon(),
            hulls(),
            doctrine_seen()
        ),
        &access.with(vec![id.into()]),
    )?;
    let r = rows.first().ok_or(PageError::NotFound)?;
    Ok(Doctrine {
        name: text(r, 0),
        description: text(r, 1),
        icon_choice: opt_int(r, 2),
        created_at: text(r, 3),
        updated_at: text(r, 4),
        icon: opt_int(r, 5).unwrap_or(0),
        hulls: text(r, 6),
    })
}

/// Its fits the viewer sees: id, hull id, hull, name, role.
fn fits_of(access: &Access, id: i64) -> Result<Vec<Vec<Db>>, PageError> {
    query(
        &format!(
            "SELECT f.id, f.hull_type_id, t.name, f.name, f.role FROM doctrine_fits df \
             JOIN fits f ON f.id = df.fit_id JOIN types t ON t.type_id = f.hull_type_id \
             WHERE df.doctrine_id = $3 AND {} ORDER BY lower(t.name), lower(f.name), f.id",
            fit_seen()
        ),
        &access.with(vec![id.into()]),
    )
}

pub(crate) fn page(access: &Access, id: i64) -> Result<Page, PageError> {
    let doctrine = seen_doctrine(access, id)?;
    let fits = fits_of(access, id)?;
    let categories = category_names(access, &doctrine_categories_sql(), &[id])?;
    let fit_ids: Vec<i64> = fits.iter().map(|r| int(r, 0)).collect();
    let fit_categories = category_names(access, &fit_categories_sql(), &fit_ids)?;
    let edit = format!("edit/doctrine/{id}");
    let profile = Profile::new(item_type(doctrine.icon, clip(&doctrine.name, 200)))
        .subtitle(fits_word(i64::try_from(fits.len()).unwrap_or(i64::MAX)))
        .fact("Ships", clip(&doctrine.hulls, 300))
        .fact(
            "Categories",
            categories
                .get(&id)
                .cloned()
                .unwrap_or_else(|| "None".to_owned()),
        )
        .fact("Created", time(doctrine.created_at.clone()))
        .fact("Updated", time(doctrine.updated_at.clone()));
    let mut page = primary(
        Page::new(clip(&doctrine.name, 200)).description("Doctrine"),
        access,
        "Edit doctrine",
        &edit,
    )
    .profile(profile);
    if !doctrine.description.is_empty() {
        page = page.card(Card::new("About").description(clip(&doctrine.description, 2000)));
    }
    let mut table = Table::new(vec![
        Column::text("Hull"),
        Column::text("Fit"),
        Column::text("Role"),
        Column::text("Categories"),
    ])
    .title("Fits")
    .empty("No fits in this doctrine yet.");
    for r in &fits {
        let fit = int(r, 0);
        table = table.row(vec![
            item_type(int(r, 1), text(r, 2)).into(),
            link(clip(&text(r, 3), 200), format!("fit/{fit}")).into(),
            clip(&text(r, 4), 200).into(),
            fit_categories.get(&fit).cloned().unwrap_or_default().into(),
        ]);
    }
    Ok(page.table(table))
}

// ---- adding and editing ---------------------------------------------------------------

#[derive(Default)]
pub(crate) struct Values {
    name: String,
    description: String,
    icon: String,
}

impl Values {
    fn posted(submission: &Submission) -> Self {
        Self {
            name: submission.value("name").to_owned(),
            description: submission.value("description").to_owned(),
            icon: submission.value("icon").to_owned(),
        }
    }
}

/// Hulls to pick the icon from (AA's: every fit's ship), "automatic"
/// first.
fn icon_options() -> Result<Vec<(String, String)>, PageError> {
    let mut options = vec![(String::new(), "Its main hull".to_owned())];
    options.extend(
        query(
            "SELECT DISTINCT t.type_id, t.name FROM fits f JOIN types t ON t.type_id = f.hull_type_id \
             ORDER BY t.name, t.type_id LIMIT $1",
            &[SELECT_OPTIONS.into()],
        )?
        .iter()
        .map(|r| (int(r, 0).to_string(), clip(&text(r, 1), 200))),
    );
    Ok(options)
}

fn doctrine_form(values: &Values, submit: &str) -> Result<Form, PageError> {
    Ok(Form::new("doctrine", submit)
        .field(
            Field::text("name", "Name", MAX_NAME)
                .required()
                .value(values.name.clone()),
        )
        .field(
            Field::textarea("description", "Description", MAX_DESCRIPTION)
                .help("What it's for, and how to fly it.")
                .value(values.description.clone()),
        )
        .field(
            Field::select("icon", "Icon", icon_options()?)
                .help("The ship shown for it. Its main hull is the one most of its fits are.")
                .value(values.icon.clone()),
        ))
}

pub(crate) fn add_page(again: Option<(&str, Values)>) -> Result<Page, PageError> {
    let mut page = Page::new("New doctrine").description("A new doctrine; add its fits next.");
    let values = match again {
        Some((note, values)) => {
            page = page.text(note);
            values
        }
        None => Values::default(),
    };
    Ok(page.form(doctrine_form(&values, "Create doctrine")?))
}

pub(crate) fn edit_page(
    access: &Access,
    id: i64,
    again: Option<(&str, Values)>,
) -> Result<Page, PageError> {
    let doctrine = seen_doctrine(access, id)?;
    let fits = fits_of(access, id)?;
    let mut page = Page::new(format!("Edit {}", clip(&doctrine.name, 200)))
        .description("Its name, description, icon and fits");
    let values = match again {
        Some((note, values)) => {
            page = page.text(note);
            values
        }
        None => Values {
            name: doctrine.name.clone(),
            description: doctrine.description.clone(),
            icon: doctrine
                .icon_choice
                .map(|i| i.to_string())
                .unwrap_or_default(),
        },
    };
    page = page.form(doctrine_form(&values, "Save doctrine")?);
    // Fits to add: every fit not in it yet (a manager sees them all).
    let options: Vec<(String, String)> = query(
        "SELECT f.id, t.name, f.name FROM fits f JOIN types t ON t.type_id = f.hull_type_id \
         WHERE NOT EXISTS (SELECT 1 FROM doctrine_fits df WHERE df.fit_id = f.id AND df.doctrine_id = $1) \
         ORDER BY lower(t.name), lower(f.name), f.id LIMIT $2",
        &[id.into(), (SELECT_OPTIONS + 1).into()],
    )?
    .iter()
    .map(|r| {
        (
            int(r, 0).to_string(),
            clip(&format!("{}: {}", text(r, 1), text(r, 2)), 300),
        )
    })
    .collect();
    if !options.is_empty() {
        page = page.form(
            Form::new("add_fit", "Add fit to doctrine")
                .field(Field::select("fit", "Fit", options).required()),
        );
    }
    let mut table = Table::new(vec![
        Column::text("Hull"),
        Column::text("Fit"),
        Column::text("Role"),
        Column::text(""),
    ])
    .title("Fits")
    .empty("No fits in this doctrine yet: add them above.");
    for r in &fits {
        let fit = int(r, 0);
        table = table.row(vec![
            item_type(int(r, 1), text(r, 2)).into(),
            link(clip(&text(r, 3), 200), format!("fit/{fit}")).into(),
            clip(&text(r, 4), 200).into(),
            action("Remove", "remove_fit")
                .field("fit", fit.to_string())
                .confirm(clip(
                    &format!(
                        "\"{}\" is taken out of this doctrine (the fit itself stays).",
                        text(r, 3)
                    ),
                    400,
                ))
                .into(),
        ]);
    }
    Ok(page.table(table).card(
        Card::new("Delete this doctrine")
            .description("Its fits stay, in All Fits and their other doctrines.")
            .field(
                "",
                action("Delete", "delete_doctrine")
                    .tone(Tone::Danger)
                    .confirm(clip(
                        &format!("The {} doctrine is deleted; its fits stay.", doctrine.name),
                        400,
                    )),
            ),
    ))
}

pub(crate) fn save(
    access: &Access,
    id: Option<i64>,
    submission: &Submission,
) -> Result<SubmitResult, PageError> {
    let again = |note: &str| -> Result<SubmitResult, PageError> {
        let values = Some((note, Values::posted(submission)));
        Ok(SubmitResult::Page(match id {
            Some(id) => edit_page(access, id, values)?,
            None => add_page(values)?,
        }))
    };
    if let Some(id) = id {
        seen_doctrine(access, id)?;
    }
    let name = submission.value("name").trim().to_owned();
    if name.is_empty() {
        return again("Give the doctrine a name.");
    }
    // The select only offers fits' hulls; empty is automatic.
    let icon: Option<i64> = submission.value("icon").parse().ok();
    let params: Vec<Db> = vec![
        name.into(),
        submission.value("description").trim().to_owned().into(),
        icon.into(),
    ];
    let saved = match id {
        None => storage::query(
            "INSERT INTO doctrines (name, description, icon_type_id) VALUES ($1, $2, $3) RETURNING id",
            &params,
        )
        .map(|r| r.rows.first().map_or(0, |r| int(r, 0))),
        Some(id) => {
            let mut params = params;
            params.push(id.into());
            storage::execute(
                "UPDATE doctrines SET name = $1, description = $2, icon_type_id = $3, \
                 updated_at = now() WHERE id = $4",
                &params,
            )
            .map(|_| id)
        }
    }
    .map_err(|e| failed("saving the doctrine", e))?;
    Ok(SubmitResult::Redirect(match id {
        // Its fits next.
        None => format!("edit/doctrine/{saved}"),
        Some(_) => format!("doctrine/{saved}"),
    }))
}

/// Changes to a doctrine's fits, with its updated time.
fn change_fits(id: i64, change: Statement) -> Result<SubmitResult, PageError> {
    storage::transaction(&[
        change,
        Statement::new(
            "UPDATE doctrines SET updated_at = now() WHERE id = $1",
            vec![id.into()],
        ),
    ])
    .map_err(|e| failed("changing the doctrine's fits", e))?;
    Ok(SubmitResult::Redirect(format!("edit/doctrine/{id}")))
}

pub(crate) fn add_fit(id: i64, fit: i64) -> Result<SubmitResult, PageError> {
    change_fits(
        id,
        Statement::new(
            "INSERT INTO doctrine_fits (doctrine_id, fit_id) SELECT d.id, f.id FROM doctrines d, fits f \
             WHERE d.id = $1 AND f.id = $2 ON CONFLICT DO NOTHING",
            vec![id.into(), fit.into()],
        ),
    )
}

pub(crate) fn remove_fit(id: i64, fit: i64) -> Result<SubmitResult, PageError> {
    change_fits(
        id,
        Statement::new(
            "DELETE FROM doctrine_fits WHERE doctrine_id = $1 AND fit_id = $2",
            vec![id.into(), fit.into()],
        ),
    )
}

pub(crate) fn delete(viewer: &Viewer, id: i64) -> Result<SubmitResult, PageError> {
    if execute(
        "deleting the doctrine",
        "DELETE FROM doctrines WHERE id = $1",
        &[id.into()],
    )? == 0
    {
        return Err(PageError::NotFound);
    }
    log::info(format!(
        "doctrine {id} deleted by {} ({})",
        viewer.main.name, viewer.main.id
    ));
    Ok(SubmitResult::Redirect(String::new()))
}

// ---- shared with Fleet Pings and FAT ----------------------------------------------

/// Shares the doctrines (aa-fleetpings' and aa-fat's
/// `use_doctrines_from_fittings_module`), seen as here: public when a
/// doctrine is in no category or in one without groups, else by members
/// of its categories' groups; `manage` sees every one. Logged, not failed:
/// nothing a pilot did depends on it.
pub(crate) fn share() {
    let rows = match query(
        "SELECT d.id, d.name, \
         (NOT EXISTS (SELECT 1 FROM category_doctrines sd WHERE sd.doctrine_id = d.id) \
          OR EXISTS (SELECT 1 FROM category_doctrines sd WHERE sd.doctrine_id = d.id \
                     AND NOT EXISTS (SELECT 1 FROM category_groups cg \
                                     WHERE cg.category_id = sd.category_id))) AS public, \
         coalesce((SELECT jsonb_agg(DISTINCT cg.group_id) FROM category_doctrines sd \
                   JOIN category_groups cg ON cg.category_id = sd.category_id \
                   WHERE sd.doctrine_id = d.id), '[]'::jsonb) AS groups \
         FROM doctrines d ORDER BY lower(d.name), d.id LIMIT 500",
        &[],
    ) {
        Ok(rows) => rows,
        Err(err) => {
            log::warn(format!("sharing doctrines: {err:?}"));
            return;
        }
    };
    let shared: Vec<SharedDoctrine> = rows
        .iter()
        .map(|r| {
            let id = int(r, 0);
            let public = r.get(2).and_then(Db::as_bool).unwrap_or(true);
            let groups: Vec<i64> = serde_json::from_str(&text(r, 3)).unwrap_or_default();
            SharedDoctrine {
                key: id.to_string(),
                name: clip(&text(r, 1), 100),
                link: format!("doctrine/{id}"),
                groups: (!public).then_some(groups),
            }
        })
        .collect();
    if let Err(err) = tether_plugin_sdk::doctrines::publish(&shared, Some("manage")) {
        log::warn(format!("sharing doctrines: {err:?}"));
    }
}
