//! Categories (AA's): tags on fits and doctrines, each public or limited
//! to groups. A doctrine's fits count as in its categories.

use std::collections::BTreeMap;

use tether_plugin_sdk::identity::{self, Viewer};
use tether_plugin_sdk::storage;
use tether_plugin_sdk::{
    Card, Column, Field, Form, Page, PageError, Submission, SubmitResult, Table, Tone, Value,
    action, badge, item_type, link, log,
};

use crate::{
    Access, CATEGORY_SEEN, app_links, clip, doctrine_seen, execute, failed, fit_seen, int, query,
    text,
};

/// AA's lengths.
const MAX_NAME: u32 = 255;
const MAX_COLOR: u32 = 20;
const DEFAULT_COLOR: &str = "#FFFFFF";
/// Options in a select (the host's limit).
const SELECT_OPTIONS: i64 = 100;

struct Category {
    name: String,
    color: String,
    groups: Vec<i64>,
}

fn seen_category(access: &Access, id: i64) -> Result<Category, PageError> {
    let rows = query(
        &format!(
            "SELECT c.name, c.color, coalesce((SELECT string_agg(cg.group_id::text, ',' \
             ORDER BY cg.group_id) FROM category_groups cg WHERE cg.category_id = c.id), '') \
             FROM categories c WHERE c.id = $3 AND {CATEGORY_SEEN}"
        ),
        &access.with(vec![id.into()]),
    )?;
    let r = rows.first().ok_or(PageError::NotFound)?;
    Ok(Category {
        name: text(r, 0),
        color: text(r, 1),
        groups: text(r, 2)
            .split(',')
            .filter_map(|g| g.parse().ok())
            .collect(),
    })
}

/// Names for group ids, as far as the viewer may know them (their own, and
/// those they may be offered).
fn group_names() -> BTreeMap<i64, String> {
    identity::all_groups()
        .into_iter()
        .chain(identity::groups())
        .map(|g| (g.id, g.name))
        .collect()
}

fn group_name(names: &BTreeMap<i64, String>, id: i64) -> String {
    names
        .get(&id)
        .map_or_else(|| format!("A group you can't see ({id})"), |n| clip(n, 200))
}

/// Who sees it: everyone, or members of its groups.
fn access_badge(groups: i64) -> Value {
    if groups == 0 {
        badge("Public", Tone::Neutral).into()
    } else if groups == 1 {
        badge("1 group", Tone::Warning).into()
    } else {
        badge(format!("{groups} groups"), Tone::Warning).into()
    }
}

// ---- the list -----------------------------------------------------------------

pub(crate) fn list(access: &Access) -> Result<Page, PageError> {
    // AA's counts: its doctrines, and its fits plus its doctrines' fits.
    let rows = query(
        &format!(
            "SELECT c.id, c.name, c.color, \
             (SELECT count(*) FROM category_doctrines cd WHERE cd.category_id = c.id), \
             (SELECT count(*) FROM category_fits cf WHERE cf.category_id = c.id) + \
             (SELECT count(*) FROM category_doctrines cd JOIN doctrine_fits df \
              ON df.doctrine_id = cd.doctrine_id WHERE cd.category_id = c.id), \
             (SELECT count(*) FROM category_groups cg WHERE cg.category_id = c.id) \
             FROM categories c WHERE {CATEGORY_SEEN} ORDER BY lower(c.name), c.id LIMIT 500"
        ),
        &access.params(),
    )?;
    let mut columns = vec![
        Column::text("Category"),
        Column::text("Colour"),
        Column::numeric("Doctrines"),
        Column::numeric("Fits"),
        Column::text("Seen by"),
    ];
    if access.manage {
        columns.push(Column::text("Action"));
        columns.push(Column::text(""));
    }
    let mut table = Table::new(columns)
        .title("Categories")
        .empty("No categories yet.");
    for r in &rows {
        let id = int(r, 0);
        let mut row: Vec<Value> = vec![
            link(clip(&text(r, 1), 200), format!("category/{id}")).into(),
            clip(&text(r, 2), 40).into(),
            int(r, 3).into(),
            int(r, 4).into(),
            access_badge(int(r, 5)),
        ];
        if access.manage {
            row.push(link("Edit", format!("edit/category/{id}")).into());
            row.push(delete_button(id, &text(r, 1)));
        }
        table = table.row(row);
    }
    Ok(app_links(
        Page::new("Categories").description(
            "Tags on doctrines and fits. A category limited to groups is seen, with what's in it, \
             only by their members.",
        ),
        access,
        None,
    )
    .table(table))
}

fn delete_button(id: i64, name: &str) -> Value {
    action("Delete", "delete_category")
        .field("category", id.to_string())
        .tone(Tone::Danger)
        .confirm(clip(
            &format!(
                "The category \"{name}\" is deleted; its doctrines and fits stay, and may be seen \
                 by more pilots."
            ),
            400,
        ))
        .into()
}

// ---- a category's page ------------------------------------------------------------

pub(crate) fn page(access: &Access, id: i64) -> Result<Page, PageError> {
    let category = seen_category(access, id)?;
    let doctrines = query(
        &format!(
            "SELECT d.id, d.name, d.description, \
             (SELECT count(*) FROM doctrine_fits df WHERE df.doctrine_id = d.id) \
             FROM category_doctrines cd JOIN doctrines d ON d.id = cd.doctrine_id \
             WHERE cd.category_id = $3 AND {} ORDER BY lower(d.name), d.id",
            doctrine_seen()
        ),
        &access.with(vec![id.into()]),
    )?;
    let fits = query(
        &format!(
            "SELECT f.id, f.hull_type_id, t.name, f.name, f.role FROM fits f \
             JOIN types t ON t.type_id = f.hull_type_id \
             WHERE f.id IN (SELECT fit_id FROM fit_categories WHERE category_id = $3) AND {} \
             ORDER BY lower(t.name), lower(f.name), f.id LIMIT 500",
            fit_seen()
        ),
        &access.with(vec![id.into()]),
    )?;
    let edit = format!("edit/category/{id}");
    let mut about = Card::new("Category")
        .field("Colour", clip(&category.color, 40))
        .field(
            "Seen by",
            access_badge(i64::try_from(category.groups.len()).unwrap_or(i64::MAX)),
        );
    if access.manage && !category.groups.is_empty() {
        let names = group_names();
        let listed: Vec<String> = category
            .groups
            .iter()
            .map(|g| group_name(&names, *g))
            .collect();
        about = about.field("Groups", clip(&listed.join(", "), 1000));
    }
    let mut doctrine_table = Table::new(vec![
        Column::text("Doctrine"),
        Column::text("Description"),
        Column::numeric("Fits"),
    ])
    .title("Doctrines")
    .empty("No doctrines in this category.");
    for r in &doctrines {
        doctrine_table = doctrine_table.row(vec![
            link(clip(&text(r, 1), 200), format!("doctrine/{}", int(r, 0))).into(),
            clip(&text(r, 2), 300).into(),
            int(r, 3).into(),
        ]);
    }
    let mut fit_table = Table::new(vec![
        Column::text("Hull"),
        Column::text("Fit"),
        Column::text("Role"),
    ])
    .title("Fits")
    .empty("No fits in this category.");
    for r in &fits {
        fit_table = fit_table.row(vec![
            item_type(int(r, 1), text(r, 2)).into(),
            link(clip(&text(r, 3), 200), format!("fit/{}", int(r, 0))).into(),
            clip(&text(r, 4), 200).into(),
        ]);
    }
    Ok(app_links(
        Page::new(clip(&category.name, 200))
            .description("Category: its doctrines, and its fits with its doctrines' fits"),
        access,
        Some(("Edit Category", &edit)),
    )
    .card(about)
    .table(doctrine_table)
    .table(fit_table))
}

// ---- adding and editing ---------------------------------------------------------------

pub(crate) struct Values {
    name: String,
    color: String,
}

impl Values {
    fn posted(submission: &Submission) -> Self {
        Self {
            name: submission.value("name").to_owned(),
            color: submission.value("color").to_owned(),
        }
    }
}

fn category_form(values: &Values, submit: &str) -> Form {
    Form::new("category", submit)
        .field(
            Field::text("name", "Name", MAX_NAME)
                .required()
                .value(values.name.clone()),
        )
        .field(
            Field::text("color", "Colour", MAX_COLOR)
                .help("As #RRGGBB, e.g. #FFFFFF.")
                .value(values.color.clone()),
        )
}

pub(crate) fn add_page(access: &Access, again: Option<(&str, Values)>) -> Result<Page, PageError> {
    let mut page = app_links(
        Page::new("Add Category")
            .description("A new category; add its groups, doctrines and fits next."),
        access,
        None,
    );
    let values = match again {
        Some((note, values)) => {
            page = page.text(note);
            values
        }
        None => Values {
            name: String::new(),
            color: DEFAULT_COLOR.to_owned(),
        },
    };
    Ok(page.form(category_form(&values, "Add Category")))
}

/// A select of `rows` (id, label) for a form with one field, if any.
fn add_form(
    form: &str,
    field: &str,
    label: &str,
    submit: &str,
    rows: Vec<(String, String)>,
) -> Option<Form> {
    (!rows.is_empty())
        .then(|| Form::new(form, submit).field(Field::select(field, label, rows).required()))
}

fn remove_button(form: &str, field: &str, id: i64, sentence: String) -> Value {
    action("Remove", form)
        .field(field, id.to_string())
        .confirm(clip(&sentence, 400))
        .into()
}

pub(crate) fn edit_page(
    access: &Access,
    id: i64,
    again: Option<(&str, Values)>,
) -> Result<Page, PageError> {
    let category = seen_category(access, id)?;
    let mut page = app_links(
        Page::new(format!("Edit {}", clip(&category.name, 200)))
            .description("Its name, colour, groups, doctrines and fits")
            .link("Category", format!("category/{id}")),
        access,
        None,
    );
    let values = match again {
        Some((note, values)) => {
            page = page.text(note);
            values
        }
        None => Values {
            name: category.name.clone(),
            color: category.color.clone(),
        },
    };
    page = page.form(category_form(&values, "Save Category"));

    // Groups: those the editor may be offered, not on it yet.
    let names = group_names();
    let offered: Vec<(String, String)> = identity::all_groups()
        .into_iter()
        .filter(|g| !category.groups.contains(&g.id))
        .take(usize::try_from(SELECT_OPTIONS).unwrap_or(100))
        .map(|g| (g.id.to_string(), clip(&g.name, 200)))
        .collect();
    if let Some(form) = add_form("add_group", "group", "Group", "Limit to Group", offered) {
        page = page.form(form);
    }
    let mut groups = Table::new(vec![Column::text("Group"), Column::text("")])
        .title("Groups")
        .empty("No groups: every pilot with access to Fittings sees this category.");
    for group in &category.groups {
        let name = group_name(&names, *group);
        groups = groups.row(vec![
            name.clone().into(),
            remove_button(
                "remove_group",
                "group",
                *group,
                format!(
                    "{name} no longer limits this category; with no groups left, everyone sees it."
                ),
            ),
        ]);
    }
    page = page.table(groups);

    // Doctrines.
    let options: Vec<(String, String)> = query(
        "SELECT d.id, d.name FROM doctrines d WHERE NOT EXISTS (SELECT 1 FROM category_doctrines cd \
         WHERE cd.doctrine_id = d.id AND cd.category_id = $1) ORDER BY lower(d.name), d.id LIMIT $2",
        &[id.into(), SELECT_OPTIONS.into()],
    )?
    .iter()
    .map(|r| (int(r, 0).to_string(), clip(&text(r, 1), 200)))
    .collect();
    if let Some(form) = add_form(
        "add_doctrine",
        "doctrine",
        "Doctrine",
        "Add Doctrine",
        options,
    ) {
        page = page.form(form);
    }
    let mut doctrines = Table::new(vec![Column::text("Doctrine"), Column::text("")])
        .title("Doctrines")
        .empty("No doctrines in this category.");
    for r in query(
        "SELECT d.id, d.name FROM category_doctrines cd JOIN doctrines d ON d.id = cd.doctrine_id \
         WHERE cd.category_id = $1 ORDER BY lower(d.name), d.id",
        &[id.into()],
    )? {
        let doctrine = int(&r, 0);
        doctrines = doctrines.row(vec![
            link(clip(&text(&r, 1), 200), format!("doctrine/{doctrine}")).into(),
            remove_button(
                "remove_doctrine",
                "doctrine",
                doctrine,
                format!("The {} doctrine leaves this category.", text(&r, 1)),
            ),
        ]);
    }
    page = page.table(doctrines);

    // Fits of its own.
    let options: Vec<(String, String)> = query(
        "SELECT f.id, t.name, f.name FROM fits f JOIN types t ON t.type_id = f.hull_type_id \
         WHERE NOT EXISTS (SELECT 1 FROM category_fits cf WHERE cf.fit_id = f.id AND cf.category_id = $1) \
         ORDER BY lower(t.name), lower(f.name), f.id LIMIT $2",
        &[id.into(), SELECT_OPTIONS.into()],
    )?
    .iter()
    .map(|r| {
        (
            int(r, 0).to_string(),
            clip(&format!("{}: {}", text(r, 1), text(r, 2)), 300),
        )
    })
    .collect();
    if let Some(form) = add_form("add_fit", "fit", "Fit", "Add Fit", options) {
        page = page.form(form);
    }
    let mut fits = Table::new(vec![
        Column::text("Hull"),
        Column::text("Fit"),
        Column::text(""),
    ])
    .title("Fits")
    .empty("No fits of its own (its doctrines' fits are in it too).");
    for r in query(
        "SELECT f.id, f.hull_type_id, t.name, f.name FROM category_fits cf \
         JOIN fits f ON f.id = cf.fit_id JOIN types t ON t.type_id = f.hull_type_id \
         WHERE cf.category_id = $1 ORDER BY lower(t.name), lower(f.name), f.id",
        &[id.into()],
    )? {
        let fit = int(&r, 0);
        fits = fits.row(vec![
            item_type(int(&r, 1), text(&r, 2)).into(),
            link(clip(&text(&r, 3), 200), format!("fit/{fit}")).into(),
            remove_button(
                "remove_fit",
                "fit",
                fit,
                format!("\"{}\" leaves this category.", text(&r, 3)),
            ),
        ]);
    }
    Ok(page.table(fits).card(
        Card::new("Delete this category")
            .description("Its doctrines and fits stay.")
            .field("", delete_button(id, &category.name)),
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
            None => add_page(access, values)?,
        }))
    };
    if let Some(id) = id {
        seen_category(access, id)?;
    }
    let name = submission.value("name").trim().to_owned();
    if name.is_empty() {
        return again("Give the category a name.");
    }
    let mut color = submission.value("color").trim().to_owned();
    if color.is_empty() {
        DEFAULT_COLOR.clone_into(&mut color);
    }
    let saved = match id {
        None => storage::query(
            "INSERT INTO categories (name, color) VALUES ($1, $2) RETURNING id",
            &[name.into(), color.into()],
        )
        .map(|r| r.rows.first().map_or(0, |r| int(r, 0))),
        Some(id) => storage::execute(
            "UPDATE categories SET name = $1, color = $2 WHERE id = $3",
            &[name.into(), color.into(), id.into()],
        )
        .map(|_| id),
    }
    .map_err(|e| failed("saving the category", e))?;
    Ok(SubmitResult::Redirect(match id {
        None => format!("edit/category/{saved}"),
        Some(_) => format!("category/{saved}"),
    }))
}

/// Limits the category to a group the editor may be offered.
pub(crate) fn add_group(access: &Access, id: i64, group: i64) -> Result<SubmitResult, PageError> {
    seen_category(access, id)?;
    if !identity::all_groups().iter().any(|g| g.id == group) {
        return Err(PageError::NotFound);
    }
    add(id, "group", group)
}

/// Puts a doctrine, fit or group in the category.
pub(crate) fn add(id: i64, kind: &str, other: i64) -> Result<SubmitResult, PageError> {
    let sql = match kind {
        "doctrine" => {
            "INSERT INTO category_doctrines (category_id, doctrine_id) SELECT c.id, d.id \
             FROM categories c, doctrines d WHERE c.id = $1 AND d.id = $2 ON CONFLICT DO NOTHING"
        }
        "fit" => {
            "INSERT INTO category_fits (category_id, fit_id) SELECT c.id, f.id \
             FROM categories c, fits f WHERE c.id = $1 AND f.id = $2 ON CONFLICT DO NOTHING"
        }
        "group" => {
            "INSERT INTO category_groups (category_id, group_id) SELECT c.id, $2 \
             FROM categories c WHERE c.id = $1 ON CONFLICT DO NOTHING"
        }
        _ => return Err(PageError::NotFound),
    };
    execute("changing the category", sql, &[id.into(), other.into()])?;
    Ok(SubmitResult::Redirect(format!("edit/category/{id}")))
}

/// Takes a doctrine, fit or group out of the category.
pub(crate) fn remove(id: i64, kind: &str, other: i64) -> Result<SubmitResult, PageError> {
    let sql = match kind {
        "doctrine" => "DELETE FROM category_doctrines WHERE category_id = $1 AND doctrine_id = $2",
        "fit" => "DELETE FROM category_fits WHERE category_id = $1 AND fit_id = $2",
        "group" => "DELETE FROM category_groups WHERE category_id = $1 AND group_id = $2",
        _ => return Err(PageError::NotFound),
    };
    execute("changing the category", sql, &[id.into(), other.into()])?;
    Ok(SubmitResult::Redirect(format!("edit/category/{id}")))
}

pub(crate) fn delete(viewer: &Viewer, id: i64) -> Result<SubmitResult, PageError> {
    if execute(
        "deleting the category",
        "DELETE FROM categories WHERE id = $1",
        &[id.into()],
    )? == 0
    {
        return Err(PageError::NotFound);
    }
    log::info(format!(
        "category {id} deleted by {} ({})",
        viewer.main.name, viewer.main.id
    ));
    Ok(SubmitResult::Redirect("categories".to_owned()))
}
