//! aa-structures' tags: labels on structures to sort and filter them.
//!
//! - Generated tags (space type: highsec, lowsec, nullsec, w_space; and
//!   sov, where the owner's alliance holds sovereignty) are kept by the
//!   sync, as aa-structures' `update_generated_tags`.
//! - Managers make the rest (name, description, style, order, default)
//!   and put them on structures from a structure's page. A default tag
//!   goes on every structure first seen after it's made.
//! - The list filters by tags (any of them), and can show only
//!   structures with a default tag until a filter is picked
//!   (STRUCTURES_DEFAULT_TAGS_FILTER_ENABLED).

use tether_plugin_sdk::identity::Viewer;
use tether_plugin_sdk::jobs::JobError;
use tether_plugin_sdk::storage::{self, Statement, Value as Db};
use tether_plugin_sdk::{
    Column, Field, Form, Page, PageError, Submission, SubmitResult, Table, Tone, Value, badge,
    link, log,
};

use crate::{VISIBLE, failed, int, retry, text, with_rows};

/// Tags managers may make (the generated five come on top; a form holds
/// at most 30 fields).
pub const MAX_USER_TAGS: i64 = 25;

pub const STYLES: [(&str, &str); 6] = [
    ("default", "Default"),
    ("primary", "Primary"),
    ("success", "Success (green)"),
    ("info", "Info (blue)"),
    ("warning", "Warning (orange)"),
    ("danger", "Danger (red)"),
];

pub struct Tag {
    pub id: i64,
    pub name: String,
    pub description: String,
    pub style: String,
    pub order: i64,
    pub is_default: bool,
    pub user_managed: bool,
    pub structures: i64,
}

pub fn tone(style: &str) -> Tone {
    match style {
        "success" => Tone::Success,
        "warning" => Tone::Warning,
        "danger" => Tone::Danger,
        _ => Tone::Neutral,
    }
}

impl Tag {
    pub fn badge(&self) -> Value {
        badge(self.name.clone(), tone(&self.style)).into()
    }
}

/// Every tag, with how many structures have it (managers see them all).
pub fn all() -> Result<Vec<Tag>, storage::Error> {
    read(
        "SELECT t.id, t.name, t.description, t.style, t.sort_order, t.is_default, t.is_user_managed, \
             (SELECT count(*) FROM structure_tags s WHERE s.tag_id = t.id) \
         FROM tags t ORDER BY t.sort_order, t.name",
        &[],
    )
}

/// Every tag, counting only the structures the viewer may see
/// (`visibility`'s parameters).
pub fn all_visible(visible: &[Db]) -> Result<Vec<Tag>, storage::Error> {
    read(
        &format!(
            "SELECT t.id, t.name, t.description, t.style, t.sort_order, t.is_default, t.is_user_managed, \
                 (SELECT count(*) FROM structure_tags g JOIN structures s ON s.structure_id = g.structure_id \
                  WHERE g.tag_id = t.id AND {VISIBLE}) \
             FROM tags t ORDER BY t.sort_order, t.name"
        ),
        visible,
    )
}

fn read(sql: &str, params: &[Db]) -> Result<Vec<Tag>, storage::Error> {
    let rows = storage::query(sql, params)?;
    Ok(rows
        .rows
        .iter()
        .map(|r| Tag {
            id: int(r, 0),
            name: text(r, 1),
            description: text(r, 2),
            style: text(r, 3),
            order: int(r, 4),
            is_default: r.get(5).and_then(Db::as_bool).unwrap_or(false),
            user_managed: r.get(6).and_then(Db::as_bool).unwrap_or(true),
            structures: int(r, 7),
        })
        .collect())
}

/// Default tags on structures first seen, and the generated tags kept
/// current.
pub fn apply_generated() -> Result<(), JobError> {
    storage::transaction(&[
        Statement::new(
            "INSERT INTO structure_tags (structure_id, tag_id) \
             SELECT s.structure_id, t.id FROM structures s CROSS JOIN tags t \
             WHERE NOT s.defaults_applied AND t.is_default ON CONFLICT DO NOTHING",
            vec![],
        ),
        Statement::new(
            "UPDATE structures SET defaults_applied = true WHERE NOT defaults_applied",
            vec![],
        ),
        Statement::new(
            "DELETE FROM structure_tags WHERE tag_id IN (SELECT id FROM tags WHERE NOT is_user_managed)",
            vec![],
        ),
        // Space type: wormhole systems are 31xxxxxx; security as EVE
        // rounds it (0.45 and up is high).
        Statement::new(
            "INSERT INTO structure_tags (structure_id, tag_id) \
             SELECT s.structure_id, t.id FROM structures s JOIN systems y ON y.system_id = s.system_id \
             JOIN tags t ON NOT t.is_user_managed AND t.name = CASE \
                 WHEN s.system_id BETWEEN 31000000 AND 31999999 THEN 'w_space' \
                 WHEN y.security_status >= 0.45 THEN 'highsec' \
                 WHEN y.security_status > 0.0 THEN 'lowsec' \
                 ELSE 'nullsec' END \
             ON CONFLICT DO NOTHING",
            vec![],
        ),
        Statement::new(
            "INSERT INTO structure_tags (structure_id, tag_id) \
             SELECT DISTINCT s.structure_id, t.id FROM structures s \
             JOIN sovereignty v ON v.system_id = s.system_id \
             JOIN owners o ON o.corporation_id = s.corporation_id AND o.alliance_id = v.alliance_id \
             JOIN tags t ON NOT t.is_user_managed AND t.name = 'sov' \
             ON CONFLICT DO NOTHING",
            vec![],
        ),
    ])
    .map_err(|e| retry("tagging structures", e))?;
    Ok(())
}

/// Tag ids from a filter path ("3-7"), each once; none if it isn't one.
pub fn parse_filter(path: &str) -> Option<Vec<i64>> {
    let mut ids = Vec::new();
    for part in path.split('-') {
        let id: i64 = part.parse().ok()?;
        if id <= 0 {
            return None;
        }
        ids.push(id);
    }
    ids.sort_unstable();
    ids.dedup();
    (!ids.is_empty() && ids.len() <= 30).then_some(ids)
}

/// The filter form on the list: a checkbox per tag.
pub fn filter_form(tags: &[Tag], selected: &[i64]) -> Form {
    let mut form = Form::new("filter_tags", "Filter")
        .title("Filter by tag")
        .description("Structures with any of the tags ticked. Tick none to see every structure.");
    for tag in tags.iter().take(30) {
        form = form.field(Field::checkbox(
            format!("tag_{}", tag.id),
            tag.name.clone(),
            selected.contains(&tag.id),
        ));
    }
    form
}

/// Where the filter form goes: the list filtered by the ticked tags.
pub fn submit_filter(submission: &Submission) -> SubmitResult {
    let tags = all().unwrap_or_default();
    let ticked: Vec<String> = tags
        .iter()
        .filter(|t| submission.checked(&format!("tag_{}", t.id)))
        .map(|t| t.id.to_string())
        .collect();
    if ticked.is_empty() {
        SubmitResult::Redirect("".into())
    } else {
        SubmitResult::Redirect(format!("tags/{}", ticked.join("-")))
    }
}

/// The Tags tab: each tag with its structures, linking to the filter.
pub fn tag_table(tags: &[Tag]) -> Table {
    with_rows(
        Table::new(vec![
            Column::text("Tag"),
            Column::text("Description"),
            Column::numeric("Structures"),
            Column::text("Show"),
        ])
        .title("Tags")
        .empty("No tags yet."),
        tags.iter().map(|t| {
            vec![
                t.badge(),
                t.description.clone().into(),
                t.structures.into(),
                link("Structures with this tag", format!("tags/{}", t.id)).into(),
            ]
        }),
    )
}

/// Managers' tag page (Structures settings → Tags).
pub fn settings_page(problem: Option<&str>) -> Result<Page, PageError> {
    let tags = all().map_err(|e| failed("reading tags", e))?;
    let mut page = Page::new("Structures tags")
        .description("Tags to sort and filter structures, as aa-structures' tags");
    if let Some(problem) = problem {
        page = page.text(problem);
    }
    let table = with_rows(
        Table::new(vec![
            Column::text("Tag"),
            Column::text("Description"),
            Column::text("Kind"),
            Column::text("Default"),
            Column::numeric("Order"),
            Column::numeric("Structures"),
        ])
        .title("Tags")
        .empty("No tags yet."),
        tags.iter().map(|t| {
            vec![
                t.badge(),
                t.description.clone().into(),
                if t.user_managed {
                    "Made by a manager".into()
                } else {
                    "Generated".into()
                },
                if t.is_default { "Yes" } else { "No" }.into(),
                t.order.into(),
                t.structures.into(),
            ]
        }),
    );
    page = page
        .table(table)
        .text(
            "Generated tags (space type, and sov where the owner's alliance holds sovereignty) are \
             kept by the sync. Put the others on structures from a structure's page (open it from \
             the list).",
        )
        .form(
            Form::new("save_tag", "Save tag")
                .title("Add or change a tag")
                .description("A tag with the same name is changed.")
                .field(Field::text("name", "Name", 40).required())
                .field(Field::text("description", "Description", 200))
                .field(
                    Field::select(
                        "style",
                        "Style",
                        STYLES
                            .iter()
                            .map(|(v, l)| ((*v).to_owned(), (*l).to_owned()))
                            .collect(),
                    )
                    .value("default")
                    .required(),
                )
                .field(
                    Field::number("sort_order", "Order")
                        .range(Some(100.0), Some(100_000.0), true)
                        .value("100")
                        .help("Tags are listed by this, lowest first (100 and up).")
                        .required(),
                )
                .field(
                    Field::checkbox("is_default", "Default", false)
                        .help("Put it on every structure first seen from now on."),
                ),
        );
    let removable: Vec<(String, String)> = tags
        .iter()
        .filter(|t| t.user_managed)
        .map(|t| (t.id.to_string(), t.name.clone()))
        .collect();
    if !removable.is_empty() {
        page = page.form(
            Form::new("delete_tag", "Delete tag")
                .title("Delete a tag")
                .description("It comes off every structure. Generated tags can't be deleted.")
                .field(Field::select("tag", "Tag", removable).required()),
        );
    }
    Ok(page.card(
        tether_plugin_sdk::Card::new("Structures").field("Back", link("Settings", "settings")),
    ))
}

pub fn save_tag(viewer: &Viewer, submission: &Submission) -> Result<SubmitResult, PageError> {
    let name = submission.value("name").trim().to_owned();
    if name.is_empty() || name.chars().any(char::is_control) {
        return Ok(SubmitResult::Page(settings_page(Some(
            "Give the tag a name.",
        ))?));
    }
    let style = submission.value("style");
    if !STYLES.iter().any(|(v, _)| *v == style) {
        return Err(PageError::NotFound);
    }
    let order: i64 = submission
        .value("sort_order")
        .parse()
        .map_err(|_| PageError::NotFound)?;
    let existing = storage::query(
        "SELECT is_user_managed FROM tags WHERE name = $1",
        &[name.as_str().into()],
    )
    .map_err(|e| failed("reading tags", e))?;
    match existing
        .rows
        .first()
        .and_then(|r| r.first())
        .and_then(Db::as_bool)
    {
        Some(false) => {
            return Ok(SubmitResult::Page(settings_page(Some(
                "That's a generated tag: pick another name.",
            ))?));
        }
        Some(true) => {}
        None => {
            let count = storage::query("SELECT count(*) FROM tags WHERE is_user_managed", &[])
                .map_err(|e| failed("counting tags", e))?;
            if count.rows.first().map_or(0, |r| int(r, 0)) >= MAX_USER_TAGS {
                return Ok(SubmitResult::Page(settings_page(Some(&format!(
                    "At most {MAX_USER_TAGS} tags: delete one first."
                )))?));
            }
        }
    }
    storage::execute(
        "INSERT INTO tags (name, description, style, sort_order, is_default) VALUES ($1, $2, $3, $4, $5) \
         ON CONFLICT (name) DO UPDATE SET description = EXCLUDED.description, style = EXCLUDED.style, \
             sort_order = EXCLUDED.sort_order, is_default = EXCLUDED.is_default \
         WHERE tags.is_user_managed",
        &[
            name.as_str().into(),
            submission.value("description").trim().into(),
            style.into(),
            order.into(),
            submission.checked("is_default").into(),
        ],
    )
    .map_err(|e| failed("saving a tag", e))?;
    log::info(format!(
        "tag {name:?} saved by {} ({})",
        viewer.main.name, viewer.main.id
    ));
    Ok(SubmitResult::Redirect("settings/tags".into()))
}

pub fn delete_tag(viewer: &Viewer, submission: &Submission) -> Result<SubmitResult, PageError> {
    let id: i64 = submission
        .value("tag")
        .parse()
        .map_err(|_| PageError::NotFound)?;
    storage::execute(
        "DELETE FROM tags WHERE id = $1 AND is_user_managed",
        &[id.into()],
    )
    .map_err(|e| failed("deleting a tag", e))?;
    log::info(format!(
        "tag {id} deleted by {} ({})",
        viewer.main.name, viewer.main.id
    ));
    Ok(SubmitResult::Redirect("settings/tags".into()))
}

/// The structure page's form: which of the managers' tags it has.
pub fn structure_form(tags: &[Tag], on: &[i64]) -> Option<Form> {
    let managed: Vec<&Tag> = tags.iter().filter(|t| t.user_managed).collect();
    if managed.is_empty() {
        return None;
    }
    let mut form = Form::new("structure_tags", "Save tags")
        .title("Tags")
        .description("Generated tags (space type, sov) are set by the sync.");
    for tag in managed {
        form = form.field(Field::checkbox(
            format!("tag_{}", tag.id),
            tag.name.clone(),
            on.contains(&tag.id),
        ));
    }
    Some(form)
}

pub fn save_structure_tags(
    viewer: &Viewer,
    structure: i64,
    submission: &Submission,
) -> Result<SubmitResult, PageError> {
    let tags = all().map_err(|e| failed("reading tags", e))?;
    let ticked: Vec<i64> = tags
        .iter()
        .filter(|t| t.user_managed && submission.checked(&format!("tag_{}", t.id)))
        .map(|t| t.id)
        .collect();
    storage::transaction(&[
        Statement::new(
            "DELETE FROM structure_tags WHERE structure_id = $1 \
             AND tag_id IN (SELECT id FROM tags WHERE is_user_managed)",
            vec![structure.into()],
        ),
        Statement::new(
            "INSERT INTO structure_tags (structure_id, tag_id) \
             SELECT $1, t.id FROM tags t \
             WHERE t.is_user_managed AND t.id = ANY(string_to_array($2, ',')::integer[]) \
               AND EXISTS (SELECT 1 FROM structures s WHERE s.structure_id = $1) \
             ON CONFLICT DO NOTHING",
            vec![structure.into(), crate::id_list(&ticked).into()],
        ),
    ])
    .map_err(|e| failed("saving tags", e))?;
    log::info(format!(
        "tags of structure {structure} set to {ticked:?} by {} ({})",
        viewer.main.name, viewer.main.id
    ));
    Ok(SubmitResult::Redirect(format!("structure/{structure}")))
}
