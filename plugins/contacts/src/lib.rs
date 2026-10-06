//! Contacts (aa-contacts).
//!
//! - **Tracked alliances and corporations**: each owner's (a character
//!   added by a holder of `manage_alliance_contacts` or
//!   `manage_corporation_contacts`, as aa-contacts' tokens) corporation, and
//!   its alliance, read hourly; a manager of that kind may update one now.
//! - **Who sees them**: anyone with a character in that alliance or
//!   corporation; superusers every one (as aa-contacts).
//! - **Contacts**: each with its standing and labels; notes for
//!   `view_*_notes` (edited with `manage_*_contacts` too), and server links
//!   (a name, an address of any kind, a password) for `view_*_server_links`
//!   (managed with `manage_*_contacts` too).
//!
//! Not taken: aa-contacts' Secure Groups standings filter (apps don't learn
//! every character of an account, so can't judge one).

use chrono::{DateTime, Utc};
use tether_plugin_sdk::esi::{self, Subject};
use tether_plugin_sdk::identity::{self, Viewer};
use tether_plugin_sdk::jobs::{self, Job, JobError, NewJob};
use tether_plugin_sdk::storage::{self, Statement, Value as Db};
use tether_plugin_sdk::{
    Card, Column, Field, Form, Page, PageError, Plugin, Request, Submission, SubmitResult, Table,
    Tone, Value, action, alliance, badge, character, corporation, faction, link, log, time,
};

const UPDATE: &str = "update";
const MAX_NOTES: u32 = 2_000;
const MAX_LINKS: i64 = 20;
const COLORS: [(&str, &str); 8] = [
    ("primary", "Blue"),
    ("secondary", "Gray"),
    ("success", "Green"),
    ("danger", "Red"),
    ("warning", "Yellow"),
    ("info", "Cyan"),
    ("light", "Light"),
    ("dark", "Dark"),
];

struct Contacts;

impl Plugin for Contacts {
    fn render(request: Request) -> Result<Page, PageError> {
        let viewer = identity::viewer().ok_or(PageError::Forbidden)?;
        let parts: Vec<&str> = request.path.split('/').collect();
        match parts.as_slice() {
            [""] => index_page(&viewer),
            [kind, id] => list_page(&viewer, kind_of(kind)?, number(id)?),
            [kind, id, "contact", contact] => {
                contact_page(&viewer, kind_of(kind)?, number(id)?, number(contact)?, None)
            }
            _ => Err(PageError::NotFound),
        }
    }

    fn submit(submission: Submission) -> Result<SubmitResult, PageError> {
        let viewer = identity::viewer().ok_or(PageError::Forbidden)?;
        let parts: Vec<&str> = submission.request.path.split('/').collect();
        match (parts.as_slice(), submission.form.as_str()) {
            ([kind, id], "update") => {
                let kind = kind_of(kind)?;
                seen(&viewer, kind, number(id)?)?;
                if !viewer.can(&format!("manage_{kind}_contacts")) {
                    return Err(PageError::Forbidden);
                }
                let id = number(id)?;
                // ESI caches contacts for 5 minutes: reading again sooner
                // only spends the app's ESI budget.
                let fresh = storage::query(
                    "SELECT 1 FROM tracked WHERE kind = $1 AND entity_id = $2 \
                     AND updated_at > now() - interval '5 minutes'",
                    &[kind.into(), id.into()],
                )
                .map_err(|e| failed("reading tracked", e))?;
                if fresh.rows.is_empty() {
                    jobs::enqueue(NewJob::new(UPDATE).key("update-now"))
                        .map_err(|e| failed("queuing an update", e))?;
                    log::info(format!(
                        "every alliance and corporation updated on request of {} ({}), from {kind} {id}",
                        viewer.main.name, viewer.main.id
                    ));
                }
                Ok(SubmitResult::Redirect(format!("{kind}/{id}")))
            }
            ([kind, id, "contact", contact], form) => {
                let kind = kind_of(kind)?;
                let (id, contact) = (number(id)?, number(contact)?);
                seen(&viewer, kind, id)?;
                match form {
                    "notes" => save_notes(&viewer, kind, id, contact, &submission),
                    "add_link" => add_link(&viewer, kind, id, contact, &submission),
                    "delete_link" => delete_link(&viewer, kind, id, contact, &submission),
                    _ => Err(PageError::NotFound),
                }
            }
            _ => Err(PageError::NotFound),
        }
    }

    fn run_job(job: Job) -> Result<(), JobError> {
        match job.name.as_str() {
            UPDATE => update(),
            other => Err(JobError::Permanent(format!("no job {other}"))),
        }
    }
}

tether_plugin_sdk::export!(Contacts);

// ---- helpers ---------------------------------------------------------------

fn failed(what: &str, err: impl std::fmt::Debug) -> PageError {
    PageError::Failed(format!("{what}: {err:?}"))
}

fn int(row: &[Db], i: usize) -> i64 {
    row.get(i).and_then(Db::as_integer).unwrap_or_default()
}

fn float(row: &[Db], i: usize) -> f64 {
    row.get(i).and_then(Db::as_float).unwrap_or_default()
}

fn text(row: &[Db], i: usize) -> String {
    row.get(i)
        .and_then(Db::as_text)
        .unwrap_or_default()
        .to_owned()
}

fn when(row: &[Db], i: usize) -> Option<DateTime<Utc>> {
    row.get(i)
        .and_then(Db::as_text)
        .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
        .map(|t| t.with_timezone(&Utc))
}

fn rfc3339(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn number(text: &str) -> Result<i64, PageError> {
    text.parse::<i64>()
        .ok()
        .filter(|n| *n > 0)
        .ok_or(PageError::NotFound)
}

fn kind_of(text: &str) -> Result<&'static str, PageError> {
    match text {
        "alliance" => Ok("alliance"),
        "corporation" => Ok("corporation"),
        _ => Err(PageError::NotFound),
    }
}

/// Whether the viewer sees an alliance's or corporation's contacts: with a
/// character in it, or as a superuser (aa-contacts' `visible_for`).
fn sees(viewer: &Viewer, kind: &str, id: i64) -> bool {
    identity::superuser()
        || viewer.characters.iter().any(|c| match kind {
            "alliance" => c.alliance_id == Some(id),
            _ => c.corporation_id == id,
        })
}

/// The tracked alliance or corporation, if the viewer sees it; else as if
/// it weren't there.
fn seen(viewer: &Viewer, kind: &str, id: i64) -> Result<(), PageError> {
    let rows = storage::query(
        "SELECT 1 FROM tracked WHERE kind = $1 AND entity_id = $2",
        &[kind.into(), id.into()],
    )
    .map_err(|e| failed("reading tracked", e))?;
    if rows.rows.is_empty() || !sees(viewer, kind, id) {
        return Err(PageError::NotFound);
    }
    Ok(())
}

fn name(id: i64) -> Result<String, PageError> {
    let rows = storage::query("SELECT name FROM names WHERE id = $1", &[id.into()])
        .map_err(|e| failed("reading a name", e))?;
    Ok(rows
        .rows
        .first()
        .map_or_else(|| id.to_string(), |r| text(r, 0)))
}

fn entity(kind: &str, id: i64, name: String) -> Value {
    match kind {
        "character" => character(id, name).into(),
        "corporation" => corporation(id, name).into(),
        "alliance" => alliance(id, name).into(),
        "faction" => faction(id, name).into(),
        _ => name.into(),
    }
}

/// A standing as EVE colours it: blue above zero, red below.
fn standing(value: f64) -> Value {
    let tone = if value > 0.0 {
        Tone::Success
    } else if value < 0.0 {
        Tone::Danger
    } else {
        Tone::Neutral
    };
    badge(format!("{value:+.1}"), tone).into()
}

fn word(kind: &str) -> &'static str {
    match kind {
        "alliance" => "Alliance",
        _ => "Corporation",
    }
}

// ---- the update ----------------------------------------------------------------

/// Every owner's corporation's and alliance's contacts and labels.
fn update() -> Result<(), JobError> {
    let mut ids: Vec<i64> = Vec::new();
    for source in esi::data_sources() {
        let subject = Subject::DataSource(source.id);
        let mut targets = vec![("corporation", source.corporation_id)];
        if let Some(alliance) = source.alliance_id {
            targets.push(("alliance", alliance));
        }
        for (kind, entity_id) in targets {
            ids.push(entity_id);
            storage::execute(
                "INSERT INTO tracked (kind, entity_id) VALUES ($1, $2) ON CONFLICT DO NOTHING",
                &[kind.into(), entity_id.into()],
            )
            .map_err(|e| JobError::Retry(format!("tracking {kind} {entity_id}: {e:?}")))?;
            match read(kind, subject) {
                // The host reads the source's corporation or alliance as it
                // is now: store only if that's still the one asked for, so
                // a corporation changing alliance mid-run can't file one
                // alliance's standings as another's.
                Ok(_) if !still_reads(source.id, kind, entity_id) => {
                    log::info(format!(
                        "{kind} {entity_id}: owner {} moved on while reading; skipped",
                        source.id
                    ));
                }
                Ok((contacts, labels)) => {
                    ids.extend(contacts.iter().filter_map(|c| c["contact_id"].as_i64()));
                    store(kind, entity_id, &contacts, &labels)?;
                }
                Err(why) => {
                    log::warn(format!("{kind} {entity_id}: {why}"));
                    storage::execute(
                        "UPDATE tracked SET last_error = $3 WHERE kind = $1 AND entity_id = $2",
                        &[kind.into(), entity_id.into(), why.into()],
                    )
                    .map_err(|e| JobError::Retry(format!("noting an error: {e:?}")))?;
                }
            }
        }
    }
    learn_names(&ids)
}

/// Whether data source `source` still reads `kind` `entity_id`.
fn still_reads(source: i64, kind: &str, entity_id: i64) -> bool {
    esi::data_sources().into_iter().any(|s| {
        s.id == source
            && match kind {
                "alliance" => s.alliance_id == Some(entity_id),
                _ => s.corporation_id == entity_id,
            }
    })
}

/// A kind's contacts (every page) and labels.
fn read(
    kind: &str,
    subject: Subject,
) -> Result<(Vec<serde_json::Value>, Vec<serde_json::Value>), String> {
    let bodies = esi::get_all(&format!("{kind}-contacts"), subject, &[])
        .map_err(|e| format!("contacts not read: {e:?}"))?;
    let mut contacts = Vec::new();
    for body in bodies {
        let page: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
        contacts.extend(page.as_array().cloned().unwrap_or_default());
    }
    let labels = esi::get(&format!("{kind}-contact-labels"), subject, &[], None)
        .ok()
        .and_then(|r| serde_json::from_str::<serde_json::Value>(&r.body).ok())
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default();
    Ok((contacts, labels))
}

fn store(
    kind: &str,
    entity_id: i64,
    contacts: &[serde_json::Value],
    labels: &[serde_json::Value],
) -> Result<(), JobError> {
    let rows: Vec<serde_json::Value> = contacts
        .iter()
        .filter_map(|c| {
            let ids: Vec<String> = c["label_ids"]
                .as_array()
                .map(|l| {
                    l.iter()
                        .filter_map(|i| i.as_i64())
                        .map(|i| i.to_string())
                        .collect()
                })
                .unwrap_or_default();
            Some(serde_json::json!({
                "contact_id": c["contact_id"].as_i64()?,
                "contact_type": c["contact_type"].as_str()?,
                "standing": c["standing"].as_f64()?,
                "label_ids": ids.join(","),
            }))
        })
        .collect();
    let labels: Vec<serde_json::Value> = labels
        .iter()
        .filter_map(|l| {
            Some(serde_json::json!({
                "label_id": l["label_id"].as_i64()?,
                "name": l["label_name"].as_str()?,
            }))
        })
        .collect();
    let rows = Db::json(serde_json::Value::Array(rows).to_string());
    storage::transaction(&[
        // Gone from EVE: gone here (with their notes and links).
        Statement::new(
            "DELETE FROM contacts WHERE kind = $1 AND entity_id = $2 AND contact_id NOT IN \
             (SELECT contact_id FROM json_to_recordset($3::json) AS x(contact_id bigint))",
            vec![kind.into(), entity_id.into(), rows.clone()],
        ),
        Statement::new(
            "INSERT INTO contacts (kind, entity_id, contact_id, contact_type, standing, label_ids) \
             SELECT DISTINCT ON (contact_id) $1, $2, contact_id, contact_type, standing, label_ids \
             FROM json_to_recordset($3::json) AS x(contact_id bigint, contact_type text, \
                 standing double precision, label_ids text) \
             ON CONFLICT (kind, entity_id, contact_id) DO UPDATE SET \
                 contact_type = EXCLUDED.contact_type, standing = EXCLUDED.standing, \
                 label_ids = EXCLUDED.label_ids",
            vec![kind.into(), entity_id.into(), rows],
        ),
        Statement::new(
            "DELETE FROM labels WHERE kind = $1 AND entity_id = $2",
            vec![kind.into(), entity_id.into()],
        ),
        Statement::new(
            "INSERT INTO labels (kind, entity_id, label_id, name) \
             SELECT DISTINCT ON (label_id) $1, $2, label_id, name \
             FROM json_to_recordset($3::json) AS x(label_id bigint, name text)",
            vec![
                kind.into(),
                entity_id.into(),
                Db::json(serde_json::Value::Array(labels).to_string()),
            ],
        ),
        Statement::new(
            "UPDATE tracked SET updated_at = now(), last_error = NULL \
             WHERE kind = $1 AND entity_id = $2",
            vec![kind.into(), entity_id.into()],
        ),
    ])
    .map_err(|e| JobError::Retry(format!("storing {kind} {entity_id}: {e:?}")))?;
    Ok(())
}

/// Names for ids not named yet, a thousand at a time.
fn learn_names(ids: &[i64]) -> Result<(), JobError> {
    let mut ids: Vec<i64> = ids.iter().copied().filter(|id| *id > 0).collect();
    ids.sort_unstable();
    ids.dedup();
    let list = ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",");
    let known = storage::query(
        "SELECT id FROM names WHERE id = ANY(string_to_array($1, ',')::bigint[])",
        &[list.into()],
    )
    .map_err(|e| JobError::Retry(format!("reading names: {e:?}")))?;
    let known: Vec<i64> = known.rows.iter().map(|r| int(r, 0)).collect();
    let missing: Vec<i64> = ids.into_iter().filter(|id| !known.contains(id)).collect();
    for chunk in missing.chunks(1000) {
        match esi::names(chunk) {
            Ok(named) => {
                let rows: Vec<serde_json::Value> = named
                    .into_iter()
                    .map(|n| serde_json::json!({ "id": n.id, "name": n.name }))
                    .collect();
                storage::execute(
                    "INSERT INTO names (id, name) \
                     SELECT id, name FROM json_to_recordset($1::json) AS x(id bigint, name text) \
                     ON CONFLICT (id) DO UPDATE SET name = EXCLUDED.name",
                    &[Db::json(serde_json::Value::Array(rows).to_string())],
                )
                .map_err(|e| JobError::Retry(format!("storing names: {e:?}")))?;
            }
            Err(err) => log::warn(format!("names: {err:?}")),
        }
    }
    Ok(())
}

// ---- pages -----------------------------------------------------------------------

fn index_page(viewer: &Viewer) -> Result<Page, PageError> {
    let rows = storage::query(
        "SELECT t.kind, t.entity_id, coalesce(n.name, ''), t.updated_at, t.last_error, \
             (SELECT count(*) FROM contacts c WHERE c.kind = t.kind AND c.entity_id = t.entity_id) \
         FROM tracked t LEFT JOIN names n ON n.id = t.entity_id \
         ORDER BY t.kind, lower(coalesce(n.name, ''))",
        &[],
    )
    .map_err(|e| failed("reading tracked", e))?;
    let mut table = Table::new(vec![
        Column::text("Alliance or corporation"),
        Column::text("Kind"),
        Column::numeric("Contacts"),
        Column::numeric("Updated"),
        Column::text(""),
    ])
    .title("Contacts")
    .empty("None you're in: a data source (a character added with Add data source) brings its corporation and alliance.");
    for r in &rows.rows {
        let (kind, id) = (text(r, 0), int(r, 1));
        if !sees(viewer, &kind, id) {
            continue;
        }
        let shown = if text(r, 2).is_empty() {
            id.to_string()
        } else {
            text(r, 2)
        };
        table = table.row(vec![
            link(shown, format!("{kind}/{id}")).into(),
            word(&kind).into(),
            int(r, 5).into(),
            when(r, 3).map_or_else(|| "".into(), |t| time(rfc3339(t))),
            if r.get(4).and_then(Db::as_text).is_some() {
                badge("Last update failed", Tone::Warning).into()
            } else {
                "".into()
            },
        ]);
    }
    Ok(Page::new("Contacts")
        .description("The contacts and standings of your alliance and corporation")
        .table(table))
}

fn list_page(viewer: &Viewer, kind: &str, id: i64) -> Result<Page, PageError> {
    seen(viewer, kind, id)?;
    let notes = viewer.can(&format!("view_{kind}_notes"));
    let links = viewer.can(&format!("view_{kind}_server_links"));
    let rows = storage::query(
        "SELECT c.contact_id, c.contact_type, c.standing, coalesce(n.name, ''), c.notes, \
             coalesce((SELECT string_agg(l.name, ', ' ORDER BY l.name) FROM labels l \
                 WHERE l.kind = c.kind AND l.entity_id = c.entity_id \
                   AND l.label_id::text = ANY(string_to_array(c.label_ids, ','))), ''), \
             (SELECT count(*) FROM server_links s WHERE s.kind = c.kind \
                 AND s.entity_id = c.entity_id AND s.contact_id = c.contact_id) \
         FROM contacts c LEFT JOIN names n ON n.id = c.contact_id \
         WHERE c.kind = $1 AND c.entity_id = $2 \
         ORDER BY c.standing DESC, lower(coalesce(n.name, '')) LIMIT 500",
        &[kind.into(), id.into()],
    )
    .map_err(|e| failed("reading contacts", e))?;
    let mut columns = vec![
        Column::text("Contact"),
        Column::text("Type"),
        Column::numeric("Standing"),
        Column::text("Labels"),
    ];
    if notes {
        columns.push(Column::text("Notes"));
    }
    if links {
        columns.push(Column::numeric("Server links"));
    }
    if notes || links {
        columns.push(Column::text(""));
    }
    let mut table = Table::new(columns)
        .title("Contacts")
        .empty("No contacts, or not read yet.");
    for r in &rows.rows {
        let contact = int(r, 0);
        let kind_of_contact = text(r, 1);
        let shown = if text(r, 3).is_empty() {
            contact.to_string()
        } else {
            text(r, 3)
        };
        let mut row: Vec<Value> = vec![
            entity(&kind_of_contact, contact, shown),
            word_of_contact(&kind_of_contact).into(),
            standing(float(r, 2)),
            text(r, 5).into(),
        ];
        if notes {
            let mut note: String = text(r, 4).chars().take(200).collect();
            if text(r, 4).chars().count() > 200 {
                note.push('…');
            }
            row.push(note.into());
        }
        if links {
            row.push(int(r, 6).into());
        }
        if notes || links {
            row.push(link("Open", format!("{kind}/{id}/contact/{contact}")).into());
        }
        table = table.row(row);
    }
    let mut page = Page::new(format!("{} contacts: {}", word(kind), name(id)?))
        .description("Standings and labels as set in EVE, read hourly")
        .link("Contacts", "")
        .table(table);
    if viewer.can(&format!("manage_{kind}_contacts")) {
        page =
            page.card(Card::new("Update").field("Read them again", action("Update now", "update")));
    }
    Ok(page)
}

fn word_of_contact(kind: &str) -> String {
    let mut chars = kind.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().collect::<String>() + chars.as_str()
    })
}

fn contact_page(
    viewer: &Viewer,
    kind: &str,
    id: i64,
    contact: i64,
    problem: Option<&str>,
) -> Result<Page, PageError> {
    seen(viewer, kind, id)?;
    let (view_notes, view_links, manage) = (
        viewer.can(&format!("view_{kind}_notes")),
        viewer.can(&format!("view_{kind}_server_links")),
        viewer.can(&format!("manage_{kind}_contacts")),
    );
    if !view_notes && !view_links {
        return Err(PageError::NotFound);
    }
    let rows = storage::query(
        "SELECT c.contact_type, c.standing, coalesce(n.name, ''), c.notes FROM contacts c \
         LEFT JOIN names n ON n.id = c.contact_id \
         WHERE c.kind = $1 AND c.entity_id = $2 AND c.contact_id = $3",
        &[kind.into(), id.into(), contact.into()],
    )
    .map_err(|e| failed("reading the contact", e))?;
    let row = rows.rows.first().ok_or(PageError::NotFound)?;
    let shown = if text(row, 2).is_empty() {
        contact.to_string()
    } else {
        text(row, 2)
    };
    let mut page = Page::new(shown.clone())
        .description(format!("A contact of {}", name(id)?))
        .link("Contacts", "")
        .link("All of them", format!("{kind}/{id}"))
        .card(
            Card::new("Contact")
                .field("Contact", entity(&text(row, 0), contact, shown))
                .field("Standing", standing(float(row, 1))),
        );
    if let Some(problem) = problem {
        page = page.text(problem);
    }
    if view_notes {
        if manage {
            page = page.form(
                Form::new("notes", "Save notes")
                    .title("Notes")
                    .field(Field::textarea("notes", "Notes", MAX_NOTES).value(text(row, 3))),
            );
        } else {
            page = page.card(Card::new("Notes").field(
                "Notes",
                if text(row, 3).is_empty() {
                    "None.".to_owned()
                } else {
                    text(row, 3)
                },
            ));
        }
    }
    if view_links {
        let links = storage::query(
            "SELECT id, name, url, password, color FROM server_links \
             WHERE kind = $1 AND entity_id = $2 AND contact_id = $3 ORDER BY lower(name), id",
            &[kind.into(), id.into(), contact.into()],
        )
        .map_err(|e| failed("reading server links", e))?;
        let mut columns = vec![
            Column::text("Name"),
            Column::text("Address"),
            Column::text("Password"),
        ];
        if manage {
            columns.push(Column::text(""));
        }
        let mut table = Table::new(columns)
            .title("Server links")
            .empty("No server links.");
        for l in &links.rows {
            let tone = match text(l, 4).as_str() {
                "primary" | "info" => Tone::Accent,
                "success" => Tone::Success,
                "danger" => Tone::Danger,
                "warning" => Tone::Warning,
                _ => Tone::Neutral,
            };
            let mut cells: Vec<Value> = vec![
                badge(text(l, 1), tone).into(),
                text(l, 2).into(),
                text(l, 3).into(),
            ];
            if manage {
                cells.push(
                    action("Delete", "delete_link")
                        .field("link", int(l, 0).to_string())
                        .tone(Tone::Danger)
                        .confirm(format!("The server link {} is deleted.", text(l, 1)))
                        .into(),
                );
            }
            table = table.row(cells);
        }
        page = page.table(table);
        if manage {
            page = page.form(
                Form::new("add_link", "Add server link")
                    .title("Add a server link")
                    .field(Field::text("name", "Name", 100).required())
                    .field(
                        Field::text("url", "Address", 500)
                            .help("A Discord invite, a TeamSpeak address, ...")
                            .required(),
                    )
                    .field(Field::text("password", "Password", 255))
                    .field(
                        Field::select(
                            "color",
                            "Colour",
                            COLORS
                                .iter()
                                .map(|(v, l)| ((*v).to_owned(), (*l).to_owned()))
                                .collect(),
                        )
                        .value("secondary")
                        .required(),
                    ),
            );
        }
    }
    Ok(page)
}

// ---- forms -------------------------------------------------------------------------

fn save_notes(
    viewer: &Viewer,
    kind: &str,
    id: i64,
    contact: i64,
    submission: &Submission,
) -> Result<SubmitResult, PageError> {
    if !(viewer.can(&format!("manage_{kind}_contacts"))
        && viewer.can(&format!("view_{kind}_notes")))
    {
        return Err(PageError::Forbidden);
    }
    let notes = submission.value("notes").trim();
    if notes.chars().count() > MAX_NOTES as usize {
        return Ok(SubmitResult::Page(contact_page(
            viewer,
            kind,
            id,
            contact,
            Some("Notes are at most 2,000 characters."),
        )?));
    }
    let changed = storage::execute(
        "UPDATE contacts SET notes = $4 WHERE kind = $1 AND entity_id = $2 AND contact_id = $3",
        &[kind.into(), id.into(), contact.into(), notes.into()],
    )
    .map_err(|e| failed("saving notes", e))?;
    if changed == 0 {
        return Err(PageError::NotFound);
    }
    log::info(format!(
        "notes on {kind} {id}'s contact {contact} saved by {} ({})",
        viewer.main.name, viewer.main.id
    ));
    Ok(SubmitResult::Redirect(format!(
        "{kind}/{id}/contact/{contact}"
    )))
}

fn may_manage_links(viewer: &Viewer, kind: &str) -> bool {
    viewer.can(&format!("manage_{kind}_contacts"))
        && viewer.can(&format!("view_{kind}_server_links"))
}

fn add_link(
    viewer: &Viewer,
    kind: &str,
    id: i64,
    contact: i64,
    submission: &Submission,
) -> Result<SubmitResult, PageError> {
    if !may_manage_links(viewer, kind) {
        return Err(PageError::Forbidden);
    }
    let (name, url, password) = (
        submission.value("name").trim(),
        submission.value("url").trim(),
        submission.value("password").trim(),
    );
    let color = submission.value("color");
    let problem = if name.is_empty() || name.chars().count() > 100 {
        Some("A name is 1 to 100 characters.")
    } else if url.is_empty() || url.chars().count() > 500 || url.chars().any(char::is_whitespace) {
        Some("An address is 1 to 500 characters, with no spaces.")
    } else if password.chars().count() > 255 {
        Some("A password is at most 255 characters.")
    } else if !COLORS.iter().any(|(v, _)| *v == color) {
        Some("Pick a colour.")
    } else {
        None
    };
    if let Some(problem) = problem {
        return Ok(SubmitResult::Page(contact_page(
            viewer,
            kind,
            id,
            contact,
            Some(problem),
        )?));
    }
    let added = storage::execute(
        &format!(
            "INSERT INTO server_links (kind, entity_id, contact_id, name, url, password, color) \
             SELECT $1, $2, $3, $4, $5, $6, $7 \
             WHERE EXISTS (SELECT 1 FROM contacts WHERE kind = $1 AND entity_id = $2 AND contact_id = $3) \
               AND (SELECT count(*) FROM server_links WHERE kind = $1 AND entity_id = $2 \
                   AND contact_id = $3) < {MAX_LINKS}"
        ),
        &[
            kind.into(),
            id.into(),
            contact.into(),
            name.into(),
            url.into(),
            password.into(),
            color.into(),
        ],
    )
    .map_err(|e| failed("adding the server link", e))?;
    if added == 0 {
        return Ok(SubmitResult::Page(contact_page(
            viewer,
            kind,
            id,
            contact,
            Some("At most 20 server links a contact."),
        )?));
    }
    log::info(format!(
        "server link {name} added to {kind} {id}'s contact {contact} by {} ({})",
        viewer.main.name, viewer.main.id
    ));
    Ok(SubmitResult::Redirect(format!(
        "{kind}/{id}/contact/{contact}"
    )))
}

fn delete_link(
    viewer: &Viewer,
    kind: &str,
    id: i64,
    contact: i64,
    submission: &Submission,
) -> Result<SubmitResult, PageError> {
    if !may_manage_links(viewer, kind) {
        return Err(PageError::Forbidden);
    }
    let link: i64 = submission
        .value("link")
        .parse()
        .map_err(|_| PageError::NotFound)?;
    storage::execute(
        "DELETE FROM server_links WHERE id = $1 AND kind = $2 AND entity_id = $3 AND contact_id = $4",
        &[link.into(), kind.into(), id.into(), contact.into()],
    )
    .map_err(|e| failed("deleting the server link", e))?;
    log::info(format!(
        "server link {link} deleted from {kind} {id}'s contact {contact} by {} ({})",
        viewer.main.name, viewer.main.id
    ));
    Ok(SubmitResult::Redirect(format!(
        "{kind}/{id}/contact/{contact}"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standings_are_coloured_as_eve() {
        assert_eq!(word_of_contact("alliance"), "Alliance");
        assert!(kind_of("alliance").is_ok());
        assert!(kind_of("character").is_err());
        assert!(number("0").is_err());
        assert_eq!(number("99005338").ok(), Some(99005338));
    }
}
