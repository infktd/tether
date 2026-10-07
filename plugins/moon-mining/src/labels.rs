//! aa-moonmining's labels (`models/moons.py:28-71`): a name, description
//! and style, one on a moon at most, shown on Moons and filtering it.
//! aa-moonmining makes them and puts them on moons in Django's admin;
//! here holders of `manage` do, on Settings → Labels and a moon's page.

use tether_plugin_sdk::identity::Viewer;
use tether_plugin_sdk::storage;
use tether_plugin_sdk::{
    Column, Field, Form, Page, PageError, Submission, SubmitResult, Table, Tone, Value, action,
    badge, log,
};

use crate::{failed, int, text, with_rows};

/// Labels a manager may make (a moon's form offers them in one select).
pub const MAX_LABELS: i64 = 50;

/// The styles offered, named as Tether draws them (aa-moonmining's six
/// Bootstrap styles drawn with the tones there are).
pub const STYLES: [(&str, &str); 4] = [
    ("default", "Grey"),
    ("success", "Blue"),
    ("warning", "Signal"),
    ("danger", "Red"),
];

pub struct Label {
    pub id: i64,
    pub name: String,
    pub description: String,
    pub style: String,
    pub moons: i64,
}

pub fn tone(style: &str) -> Tone {
    match style {
        "success" => Tone::Success,
        "warning" => Tone::Warning,
        "danger" => Tone::Danger,
        _ => Tone::Neutral,
    }
}

/// A label as a badge (nothing for none).
pub fn badge_of(name: &str, style: &str) -> Value {
    if name.is_empty() {
        "".into()
    } else {
        badge(name.to_owned(), tone(style)).into()
    }
}

/// Every label, by name, with how many moons have it.
pub fn all() -> Result<Vec<Label>, PageError> {
    let rows = storage::query(
        "SELECT l.id, l.name, l.description, l.style, \
                (SELECT count(*) FROM moons m WHERE m.label_id = l.id) \
         FROM labels l ORDER BY l.name",
        &[],
    )
    .map_err(|e| failed("reading labels", e))?;
    Ok(rows
        .rows
        .iter()
        .map(|r| Label {
            id: int(r, 0),
            name: text(r, 1),
            description: text(r, 2),
            style: text(r, 3),
            moons: int(r, 4),
        })
        .collect())
}

/// Managers' Labels page (Moon Mining settings → Labels).
pub fn settings_page(problem: Option<&str>) -> Result<Page, PageError> {
    let labels = all()?;
    let mut page = Page::new("Moon labels")
        .description("Labels to sort and filter moons, as aa-moonmining's labels");
    if let Some(problem) = problem {
        page = page.text(problem);
    }
    let table = with_rows(
        Table::new(vec![
            Column::text("Label"),
            Column::text("Description"),
            Column::numeric("Moons"),
            Column::text(""),
        ])
        .title("Labels")
        .empty("No labels yet."),
        labels.iter().map(|l| {
            vec![
                badge_of(&l.name, &l.style),
                l.description.clone().into(),
                l.moons.into(),
                action("Delete", "delete_label")
                    .field("label", l.id.to_string())
                    .tone(Tone::Danger)
                    .confirm(format!(
                        "The label {} is deleted and comes off its {} moons.",
                        l.name, l.moons
                    ))
                    .into(),
            ]
        }),
    );
    Ok(page
        .table(table)
        .text(
            "Put a label on a moon from the moon's page (open it from Moons). Everyone who may \
             see a moon sees its label, those who only uploaded its survey too, so keep who owns \
             it out of the name.",
        )
        .form(
            Form::new("save_label", "Save label")
                .title("Add or change a label")
                .description("A label with the same name is changed.")
                .field(Field::text("name", "Name", 100).required())
                .field(Field::text("description", "Description", 500))
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
                ),
        ))
}

pub fn save_label(viewer: &Viewer, submission: &Submission) -> Result<SubmitResult, PageError> {
    let name = submission.value("name").trim().to_owned();
    if name.is_empty() || name.chars().any(char::is_control) {
        return Ok(SubmitResult::Page(settings_page(Some(
            "Give the label a name.",
        ))?));
    }
    let style = submission.value("style");
    if !STYLES.iter().any(|(v, _)| *v == style) {
        return Err(PageError::NotFound);
    }
    // A new name only while there's room; one already there is changed.
    let saved = storage::execute(
        &format!(
            "INSERT INTO labels (name, description, style) SELECT $1, $2, $3 \
             WHERE EXISTS (SELECT 1 FROM labels WHERE name = $1) \
                OR (SELECT count(*) FROM labels) < {MAX_LABELS} \
             ON CONFLICT (name) DO UPDATE SET description = EXCLUDED.description, style = EXCLUDED.style"
        ),
        &[
            name.as_str().into(),
            submission.value("description").trim().into(),
            style.into(),
        ],
    )
    .map_err(|e| failed("saving a label", e))?;
    if saved == 0 {
        return Ok(SubmitResult::Page(settings_page(Some(&format!(
            "At most {MAX_LABELS} labels: delete one first."
        )))?));
    }
    log::info(format!(
        "label {name:?} saved by {} ({})",
        viewer.main.name, viewer.main.id
    ));
    Ok(SubmitResult::Redirect("settings/labels".into()))
}

pub fn delete_label(viewer: &Viewer, submission: &Submission) -> Result<SubmitResult, PageError> {
    let id: i64 = submission
        .value("label")
        .parse()
        .map_err(|_| PageError::NotFound)?;
    storage::execute("DELETE FROM labels WHERE id = $1", &[id.into()])
        .map_err(|e| failed("deleting a label", e))?;
    log::info(format!(
        "label {id} deleted by {} ({})",
        viewer.main.name, viewer.main.id
    ));
    Ok(SubmitResult::Redirect("settings/labels".into()))
}

/// A moon's page's form for managers: its label, or none.
pub fn moon_form(labels: &[Label], on: Option<i64>) -> Option<Form> {
    if labels.is_empty() {
        return None;
    }
    let mut choices = vec![(String::new(), "No label".to_owned())];
    choices.extend(labels.iter().map(|l| (l.id.to_string(), l.name.clone())));
    Some(
        Form::new("moon_label", "Save label").title("Label").field(
            Field::select("label", "Label", choices)
                .value(on.map(|id| id.to_string()).unwrap_or_default()),
        ),
    )
}

pub fn save_moon_label(
    viewer: &Viewer,
    moon: i64,
    submission: &Submission,
) -> Result<SubmitResult, PageError> {
    let label: Option<i64> = match submission.value("label") {
        "" => None,
        id => Some(id.parse().map_err(|_| PageError::NotFound)?),
    };
    let set = storage::execute(
        "UPDATE moons SET label_id = $2::bigint WHERE moon_id = $1 \
         AND ($2::bigint IS NULL OR EXISTS (SELECT 1 FROM labels WHERE id = $2::bigint))",
        &[moon.into(), label.into()],
    )
    .map_err(|e| failed("saving the moon's label", e))?;
    if set == 0 {
        return Err(PageError::NotFound);
    }
    log::info(format!(
        "moon {moon} labelled {label:?} by {} ({})",
        viewer.main.name, viewer.main.id
    ));
    Ok(SubmitResult::Redirect(format!("moon/{moon}")))
}
