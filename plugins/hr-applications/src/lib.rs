//! HR Applications (Alliance Auth's hrapplications; PRD F23).
//!
//! - **Application forms**, one per corporation, with questions: a written
//!   answer, one choice, or any of several choices (`manage`, AA's admin).
//! - **Applications**: pilots (`basic`) apply to a corporation once, follow
//!   its status on **My Applications**, and may delete it while pending.
//! - **HR Application Management** (`human_resources`): the applications
//!   to the corporation of the reviewer's main (every corporation with
//!   `all_corporations`), with the applicant's characters and answers.
//!   Reviewers **Mark in Progress** to become an application's reviewer,
//!   comment, and (as its reviewer) approve or reject it with
//!   `approve_application` / `reject_application`; `delete_application`
//!   deletes one. Nobody reviews their own application.
//!
//! The applicant's characters are kept as they were when they applied:
//! plugins only learn an account's characters while its owner is looking.

mod text;

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use tether_plugin_sdk::esi;
use tether_plugin_sdk::identity::{self, Viewer};
use tether_plugin_sdk::storage::{self, Statement, Value as Db};
use tether_plugin_sdk::{
    Badge, Card, Column, Field, Form, Page, PageError, Plugin, Request, Section, Stat, Submission,
    SubmitResult, Table, Tone, Value, badge, link, log, time,
};

/// The host's fields per form; one is the applicant's consent.
const MAX_ANSWER_FIELDS: usize = 29;
const MAX_ANSWER: u32 = 2000;
/// At most 4 bytes a character: one page value (2 KiB) always holds it.
const MAX_COMMENT: u32 = 500;
/// AA's question and help text lengths.
const MAX_TITLE: u32 = 254;
const MAX_HELP: u32 = 254;
/// Bytes per value on a page (the host allows 2 KiB).
const PIECE: usize = 2000;
/// Fields per card (the host allows 40).
const CARD_FIELDS: usize = 40;
const QUEUE_ROWS: i64 = 500;
const REVIEWED_ROWS: i64 = 200;
/// Comments per application, and characters listed: with the answers,
/// an application's page stays within the host's rows and bytes. A
/// comment of `MAX_COMMENT` characters always fits one value.
const MAX_COMMENTS: i64 = 200;
const MAX_CHARACTERS_SHOWN: usize = 250;
const MY_ROWS: i64 = 200;

struct HrApplications;

impl Plugin for HrApplications {
    fn render(request: Request) -> Result<Page, PageError> {
        let viewer = identity::viewer().ok_or(PageError::Forbidden)?;
        let parts: Vec<&str> = request.path.split('/').collect();
        match parts.as_slice() {
            [""] => my_applications(&viewer),
            ["apply", form] => apply_page(&viewer, id(form)?, None),
            ["view", app] => personal_view(&viewer, id(app)?),
            ["review"] => review_page(&viewer, None),
            ["review", app] => review_view(&viewer, id(app)?, None),
            ["forms"] => forms_page(&viewer, None),
            ["forms", form] => form_page(id(form)?, None),
            ["forms", form, "question", question] => question_page(id(form)?, id(question)?, None),
            _ => Err(PageError::NotFound),
        }
    }

    fn submit(submission: Submission) -> Result<SubmitResult, PageError> {
        let viewer = identity::viewer().ok_or(PageError::Forbidden)?;
        let path = submission.request.path.clone();
        let parts: Vec<&str> = path.split('/').collect();
        let needs = match parts.first() {
            Some(&"review") => "human_resources",
            Some(&"forms") => "manage",
            _ => "basic",
        };
        if !viewer.can(needs) {
            return Err(PageError::Forbidden);
        }
        let form = submission.form.as_str();
        match (parts.as_slice(), form) {
            (["apply", f], "apply") => apply(&viewer, id(f)?, &submission),
            (["view", app], "delete") => delete_own(&viewer, id(app)?),
            (["review"], "search") => Ok(SubmitResult::Page(review_page(
                &viewer,
                Some(submission.value("q").trim()),
            )?)),
            (["review", app], _) => review_action(&viewer, id(app)?, &submission),
            (["forms"], "add_form") => add_form(&viewer, &submission),
            (["forms", f], _) => form_action(&viewer, id(f)?, &submission),
            (["forms", f, "question", q], "edit_question") => {
                edit_question(&viewer, id(f)?, id(q)?, &submission)
            }
            _ => Err(PageError::NotFound),
        }
    }
}

tether_plugin_sdk::export!(HrApplications);

// ---- helpers ---------------------------------------------------------------

fn failed(what: &str, err: impl std::fmt::Debug) -> PageError {
    PageError::Failed(format!("{what}: {err:?}"))
}

/// A positive id from a path segment.
fn id(segment: &str) -> Result<i64, PageError> {
    segment
        .parse::<i64>()
        .ok()
        .filter(|n| *n > 0)
        .ok_or(PageError::NotFound)
}

fn rfc3339(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn int(row: &[Db], i: usize) -> i64 {
    row.get(i).and_then(Db::as_integer).unwrap_or_default()
}

fn opt_int(row: &[Db], i: usize) -> Option<i64> {
    row.get(i).and_then(Db::as_integer)
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

fn time_or(t: Option<DateTime<Utc>>, otherwise: &str) -> Value {
    t.map_or_else(|| otherwise.into(), |t| time(rfc3339(t)))
}

fn query(sql: &str, params: &[Db]) -> Result<Vec<Vec<Db>>, PageError> {
    storage::query(sql, params)
        .map(|r| r.rows)
        .map_err(|e| failed("reading", e))
}

fn count(n: usize) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

/// The first piece of `text` that fits a page value, marked when cut.
fn clip(text: &str) -> String {
    let pieces = text::pieces(text, PIECE - 4);
    match pieces.as_slice() {
        [one] => (*one).to_owned(),
        [first, ..] => format!("{first}…"),
        [] => String::new(),
    }
}

/// Names for corporations and alliances (public ESI); ids stand in for
/// any it can't give.
fn names(ids: impl IntoIterator<Item = i64>) -> HashMap<i64, String> {
    let mut ids: Vec<i64> = ids.into_iter().filter(|id| *id > 0).collect();
    ids.sort_unstable();
    ids.dedup();
    let mut out = HashMap::new();
    for chunk in ids.chunks(1000) {
        match esi::names(chunk) {
            Ok(named) => out.extend(named.into_iter().map(|n| (n.id, n.name))),
            Err(err) => {
                log::warn(format!("names: {err:?}"));
                break;
            }
        }
    }
    out
}

fn name_of(names: &HashMap<i64, String>, id: i64) -> String {
    match names.get(&id) {
        Some(name) => name.clone(),
        None if id == 0 => "Unknown".to_owned(),
        None => id.to_string(),
    }
}

// ---- applications ------------------------------------------------------------

struct Character {
    name: String,
    corporation_id: i64,
    alliance_id: Option<i64>,
}

struct Application {
    id: i64,
    corporation_name: String,
    main_name: String,
    main_corporation_id: i64,
    characters: Vec<Character>,
    approved: Option<bool>,
    reviewer_account_id: Option<i64>,
    reviewer_name: String,
    created_at: Option<DateTime<Utc>>,
    decided_at: Option<DateTime<Utc>>,
}

const APP_SELECT: &str = "SELECT a.id, f.corporation_name, a.main_name, a.main_corporation_id, \
     a.characters::text, a.approved, a.reviewer_account_id, coalesce(a.reviewer_name, ''), \
     a.created_at, a.decided_at \
     FROM applications a JOIN forms f ON f.id = a.form_id";

fn application(row: &[Db]) -> Application {
    let characters = serde_json::from_str::<Vec<serde_json::Value>>(&text(row, 4))
        .unwrap_or_default()
        .iter()
        .map(|c| Character {
            name: c["name"].as_str().unwrap_or_default().to_owned(),
            corporation_id: c["corporation_id"].as_i64().unwrap_or_default(),
            alliance_id: c["alliance_id"].as_i64(),
        })
        .collect();
    Application {
        id: int(row, 0),
        corporation_name: text(row, 1),
        main_name: text(row, 2),
        main_corporation_id: int(row, 3),
        characters,
        approved: row.get(5).and_then(Db::as_bool),
        reviewer_account_id: opt_int(row, 6),
        reviewer_name: text(row, 7),
        created_at: when(row, 8),
        decided_at: when(row, 9),
    }
}

impl Application {
    fn pending(&self) -> bool {
        self.approved.is_none()
    }

    fn status(&self) -> Badge {
        match self.approved {
            Some(true) => badge("Approved", Tone::Success),
            Some(false) => badge("Rejected", Tone::Danger),
            None if self.reviewer_account_id.is_some() => badge("In Progress", Tone::Warning),
            None => badge("Pending", Tone::Neutral),
        }
    }
}

/// The answers, as cards of label and value (long answers over several).
fn answer_cards(application: i64) -> Result<Vec<Section>, PageError> {
    let rows = query(
        "SELECT question, answer FROM responses WHERE application_id = $1 ORDER BY position",
        &[application.into()],
    )?;
    let mut fields: Vec<(String, String)> = Vec::new();
    for r in &rows {
        let answer = text(r, 1);
        let answer = if answer.trim().is_empty() {
            "—".to_owned()
        } else {
            answer
        };
        for (i, piece) in text::pieces(&answer, PIECE).into_iter().enumerate() {
            let label = if i == 0 {
                text(r, 0)
            } else {
                "(continued)".to_owned()
            };
            fields.push((label, piece.to_owned()));
        }
    }
    if fields.is_empty() {
        return Ok(vec![Section::Text("The form had no questions.".to_owned())]);
    }
    Ok(fields
        .chunks(CARD_FIELDS)
        .enumerate()
        .map(|(i, chunk)| {
            let title = if i == 0 {
                "Answers"
            } else {
                "Answers (continued)"
            };
            let mut card = Card::new(title);
            for (label, value) in chunk {
                card = card.field(label.clone(), value.clone());
            }
            Section::Card(card)
        })
        .collect())
}

fn my_applications(viewer: &Viewer) -> Result<Page, PageError> {
    let apps: Vec<Application> = query(
        &format!("{APP_SELECT} WHERE a.account_id = $1 ORDER BY a.created_at DESC LIMIT $2"),
        &[viewer.account_id.into(), MY_ROWS.into()],
    )?
    .iter()
    .map(|r| application(r))
    .collect();
    let open = query(
        "SELECT f.id, f.corporation_name FROM forms f WHERE NOT EXISTS ( \
           SELECT 1 FROM applications a WHERE a.form_id = f.id AND a.account_id = $1) \
         ORDER BY f.corporation_name LIMIT 500",
        &[viewer.account_id.into()],
    )?;
    let mut mine = Table::new(vec![
        Column::text("Corporation"),
        Column::numeric("Applied"),
        Column::text("Status"),
    ])
    .title("My Applications")
    .empty("You haven't applied anywhere yet.");
    for a in &apps {
        mine = mine.row(vec![
            link(a.corporation_name.clone(), format!("view/{}", a.id)).into(),
            time_or(a.created_at, ""),
            a.status().into(),
        ]);
    }
    let mut create = Table::new(vec![Column::text("Corporation"), Column::text("")])
        .title("Create Application")
        .empty("No other corporation is taking applications right now.");
    for r in &open {
        let name = text(r, 1);
        create = create.row(vec![
            name.clone().into(),
            link(format!("Apply to {name}"), format!("apply/{}", int(r, 0))).into(),
        ]);
    }
    let mut page = Page::new("My Applications")
        .description("Apply to a corporation and follow your applications")
        .table(mine)
        .table(create);
    let mut more = Card::new("Recruiters");
    if viewer.can("human_resources") {
        more = more.field("Review", link("HR Application Management", "review"));
    }
    if viewer.can("manage") {
        more = more.field("Forms", link("Application Forms", "forms"));
    }
    if viewer.can("human_resources") || viewer.can("manage") {
        page = page.card(more);
    }
    Ok(page)
}

// ---- applying ----------------------------------------------------------------

struct Question {
    id: i64,
    title: String,
    help: String,
    choices: Vec<String>,
    multi_select: bool,
}

impl Question {
    /// Fields it takes on the application form.
    fn fields(&self) -> usize {
        if self.multi_select && !self.choices.is_empty() {
            self.choices.len()
        } else {
            1
        }
    }

    fn kind(&self) -> &'static str {
        match (self.choices.is_empty(), self.multi_select) {
            (true, _) => "Written",
            (false, false) => "Pick one",
            (false, true) => "Tick any",
        }
    }
}

fn questions(form: i64) -> Result<Vec<Question>, PageError> {
    Ok(query(
        "SELECT id, title, help_text, choices::text, multi_select FROM questions \
         WHERE form_id = $1 ORDER BY position, id",
        &[form.into()],
    )?
    .iter()
    .map(|r| Question {
        id: int(r, 0),
        title: text(r, 1),
        help: text(r, 2),
        choices: serde_json::from_str(&text(r, 3)).unwrap_or_default(),
        multi_select: r.get(4).and_then(Db::as_bool).unwrap_or_default(),
    })
    .collect())
}

/// A form's corporation name, or not found.
fn form_name(form: i64) -> Result<String, PageError> {
    query(
        "SELECT corporation_name FROM forms WHERE id = $1",
        &[form.into()],
    )?
    .first()
    .map(|r| text(r, 0))
    .ok_or(PageError::NotFound)
}

/// The viewer's application to a form, if any.
fn applied(viewer: &Viewer, form: i64) -> Result<Option<i64>, PageError> {
    Ok(query(
        "SELECT id FROM applications WHERE form_id = $1 AND account_id = $2",
        &[form.into(), viewer.account_id.into()],
    )?
    .first()
    .map(|r| int(r, 0)))
}

fn apply_page(viewer: &Viewer, form: i64, note: Option<&str>) -> Result<Page, PageError> {
    let corporation = form_name(form)?;
    let mut page = Page::new(format!("Apply to {corporation}"));
    if let Some(note) = note {
        page = page.text(note);
    }
    if let Some(existing) = applied(viewer, form)? {
        return Ok(page
            .text(format!("You've already applied to {corporation}."))
            .card(Card::new("Your application").field(
                "Application",
                link("View Application", format!("view/{existing}")),
            )));
    }
    let mut apply = Form::new("apply", "Submit Application").description(format!(
        "{corporation}'s recruiters, and HR staff who review every corporation, see your answers and the characters on your account."
    ));
    for q in questions(form)? {
        let help = (!q.help.is_empty()).then(|| q.help.clone());
        if q.choices.is_empty() {
            let mut field =
                Field::textarea(format!("q{}", q.id), q.title.clone(), MAX_ANSWER).required();
            if let Some(help) = help {
                field = field.help(help);
            }
            apply = apply.field(field);
        } else if !q.multi_select {
            let options = q
                .choices
                .iter()
                .enumerate()
                .map(|(i, c)| (i.to_string(), c.clone()))
                .collect();
            let mut field =
                Field::select(format!("q{}", q.id), q.title.clone(), options).required();
            if let Some(help) = help {
                field = field.help(help);
            }
            apply = apply.field(field);
        } else {
            for (i, choice) in q.choices.iter().enumerate() {
                let mut field = Field::checkbox(
                    format!("q{}_{i}", q.id),
                    format!("{}: {choice}", q.title),
                    false,
                );
                if let (0, Some(help)) = (i, &help) {
                    field = field.help(help.clone());
                }
                apply = apply.field(field);
            }
        }
    }
    apply = apply.field(
        Field::checkbox(
            "consent",
            format!("Share my answers and characters with {corporation}'s recruiters and HR staff"),
            false,
        )
        .required(),
    );
    Ok(page.description("Answer the questions below").form(apply))
}

fn apply(viewer: &Viewer, form: i64, submission: &Submission) -> Result<SubmitResult, PageError> {
    let corporation = form_name(form)?;
    let mut answers = Vec::new();
    for (position, q) in questions(form)?.iter().enumerate() {
        let answer = if q.choices.is_empty() {
            submission.value(&format!("q{}", q.id)).trim().to_owned()
        } else if !q.multi_select {
            let pick: usize = submission
                .value(&format!("q{}", q.id))
                .parse()
                .map_err(|_| PageError::Failed("a choice wasn't a number".to_owned()))?;
            q.choices.get(pick).cloned().unwrap_or_default()
        } else {
            q.choices
                .iter()
                .enumerate()
                .filter(|(i, _)| submission.checked(&format!("q{}_{i}", q.id)))
                .map(|(_, c)| c.clone())
                .collect::<Vec<_>>()
                .join(", ")
        };
        answers.push(serde_json::json!({
            "position": position,
            "question": q.title,
            "answer": answer,
        }));
    }
    let characters: Vec<serde_json::Value> = viewer
        .characters
        .iter()
        .map(|c| {
            serde_json::json!({
                "id": c.id, "name": c.name,
                "corporation_id": c.corporation_id, "alliance_id": c.alliance_id,
            })
        })
        .collect();
    // The application and its answers together; nothing if they've
    // already applied.
    let added = storage::query(
        "WITH a AS ( \
           INSERT INTO applications (form_id, account_id, main_character_id, main_name, \
                                     main_corporation_id, characters) \
           VALUES ($1, $2, $3, $4, $5, $6::jsonb) \
           ON CONFLICT (form_id, account_id) DO NOTHING RETURNING id), \
         r AS ( \
           INSERT INTO responses (application_id, position, question, answer) \
           SELECT a.id, x.position, x.question, x.answer FROM a, \
                  json_to_recordset($7::json) AS x(position int, question text, answer text) \
           RETURNING 1) \
         SELECT id FROM a",
        &[
            form.into(),
            viewer.account_id.into(),
            viewer.main.id.into(),
            viewer.main.name.clone().into(),
            viewer.main.corporation_id.into(),
            Db::json(serde_json::Value::Array(characters).to_string()),
            Db::json(serde_json::Value::Array(answers).to_string()),
        ],
    )
    .map_err(|e| failed("saving the application", e))?;
    let Some(new) = added.rows.first().map(|r| int(r, 0)) else {
        return Ok(SubmitResult::Page(apply_page(viewer, form, None)?));
    };
    log::info(format!(
        "application {new} to {corporation} from {} ({})",
        viewer.main.name, viewer.main.id
    ));
    Ok(SubmitResult::Redirect(format!("view/{new}")))
}

fn personal_view(viewer: &Viewer, app: i64) -> Result<Page, PageError> {
    let a = query(
        &format!("{APP_SELECT} WHERE a.id = $1 AND a.account_id = $2"),
        &[app.into(), viewer.account_id.into()],
    )?
    .first()
    .map(|r| application(r))
    .ok_or(PageError::NotFound)?;
    let mut about = Card::new("Application")
        .field("Corporation", a.corporation_name.clone())
        .field("Applied", time_or(a.created_at, ""))
        .field("Status", a.status());
    if let Some(decided) = a.decided_at {
        about = about.field("Decided", time(rfc3339(decided)));
    }
    let mut page = Page::new("View Application")
        .description(format!("Your application to {}", a.corporation_name))
        .card(about);
    for section in answer_cards(a.id)? {
        page = page.section(section);
    }
    // Only until a recruiter takes it up: withdrawing then would take
    // their notes with it.
    if a.pending() && a.reviewer_account_id.is_none() {
        page = page.form(
            Form::new("delete", "Delete Application")
                .description("You can apply again afterwards.")
                .field(Field::checkbox("confirm", "Yes, delete my application", false).required()),
        );
    }
    Ok(page)
}

fn delete_own(viewer: &Viewer, app: i64) -> Result<SubmitResult, PageError> {
    let deleted = storage::execute(
        "DELETE FROM applications WHERE id = $1 AND account_id = $2 AND approved IS NULL \
         AND reviewer_account_id IS NULL",
        &[app.into(), viewer.account_id.into()],
    )
    .map_err(|e| failed("deleting the application", e))?;
    if deleted == 0 {
        return Err(PageError::NotFound);
    }
    log::info(format!(
        "application {app} deleted by its applicant {} ({})",
        viewer.main.name, viewer.main.id
    ));
    Ok(SubmitResult::Redirect(String::new()))
}

// ---- reviewing ---------------------------------------------------------------

/// Applications this reviewer may see: to their main's corporation (every
/// corporation with `all_corporations`), and never their own. Takes $1
/// (corporation), $2 (all) and $3 (account).
const IN_SCOPE: &str = "((f.corporation_id = $1 AND $1 <> 0) OR $2) AND a.account_id <> $3";

fn scope(viewer: &Viewer) -> Vec<Db> {
    vec![
        viewer.main.corporation_id.into(),
        viewer.can("all_corporations").into(),
        viewer.account_id.into(),
    ]
}

/// An application the reviewer may see, or not found.
fn reviewable(viewer: &Viewer, app: i64) -> Result<Application, PageError> {
    let mut params = scope(viewer);
    params.push(app.into());
    query(
        &format!("{APP_SELECT} WHERE {IN_SCOPE} AND a.id = $4"),
        &params,
    )?
    .first()
    .map(|r| application(r))
    .ok_or(PageError::NotFound)
}

fn queue_table(apps: &[Application], names: &HashMap<i64, String>, empty: &str) -> Table {
    let mut table = Table::new(vec![
        Column::numeric("Applied"),
        Column::text("Main Character"),
        Column::text("Main's corporation"),
        Column::text("Applying to"),
        Column::numeric("Characters"),
        Column::text("Status"),
        Column::text("Reviewer"),
    ])
    .empty(empty);
    for a in apps {
        table = table.row(vec![
            time_or(a.created_at, ""),
            link(a.main_name.clone(), format!("review/{}", a.id)).into(),
            name_of(names, a.main_corporation_id).into(),
            a.corporation_name.clone().into(),
            count(a.characters.len()).into(),
            a.status().into(),
            a.reviewer_name.clone().into(),
        ]);
    }
    table
}

fn review_page(viewer: &Viewer, search: Option<&str>) -> Result<Page, PageError> {
    let search = search.filter(|q| !q.is_empty()).map(str::to_lowercase);
    let mut params = scope(viewer);
    let mut filter = String::new();
    if let Some(q) = &search {
        params.push(q.clone().into());
        filter = " AND EXISTS (SELECT 1 FROM jsonb_array_elements(a.characters) c \
                   WHERE strpos(lower(c->>'name'), $4) > 0)"
            .to_owned();
    }
    let list = |status: &str, order: &str, limit: i64| -> Result<Vec<Application>, PageError> {
        Ok(query(
            &format!(
                "{APP_SELECT} WHERE {IN_SCOPE} AND {status}{filter} ORDER BY {order} LIMIT {limit}"
            ),
            &params,
        )?
        .iter()
        .map(|r| application(r))
        .collect())
    };
    let pending = list("a.approved IS NULL", "a.created_at, a.id", QUEUE_ROWS)?;
    let reviewed = list(
        "a.approved IS NOT NULL",
        "a.decided_at DESC NULLS LAST, a.id DESC",
        REVIEWED_ROWS,
    )?;
    let names = names(
        pending
            .iter()
            .chain(&reviewed)
            .map(|a| a.main_corporation_id),
    );
    let unclaimed = pending
        .iter()
        .filter(|a| a.reviewer_account_id.is_none())
        .count();
    let yours = pending
        .iter()
        .filter(|a| a.reviewer_account_id == Some(viewer.account_id))
        .count();
    let mut page = Page::new("HR Application Management")
        .description(if viewer.can("all_corporations") {
            "Applications to every corporation".to_owned()
        } else {
            "Applications to your main's corporation".to_owned()
        })
        .stats(vec![
            Stat::new("Pending", count(unclaimed)).caption("nobody reviewing yet"),
            Stat::new("In Progress", count(pending.len() - unclaimed)),
            Stat::new("Yours", count(yours)).caption("you're reviewing"),
            Stat::new("Reviewed", count(reviewed.len())),
        ]);
    let mut search_field = Field::text("q", "Character name", 100)
        .required()
        .help("Any of the applicant's characters, or part of a name.");
    if let Some(q) = &search {
        page = page.text(format!("Applications with a character named like \"{q}\"."));
        search_field = search_field.value(q.clone());
    }
    page = page.form(Form::new("search", "Search Applications").field(search_field));
    if viewer.can("manage") {
        page = page.card(Card::new("Forms").field(
            "Questions and corporations",
            link("Application Forms", "forms"),
        ));
    }
    Ok(page
        .tab(
            "Pending",
            vec![Section::Table(queue_table(
                &pending,
                &names,
                "No applications waiting.",
            ))],
        )
        .tab(
            "Reviewed",
            vec![Section::Table(
                queue_table(&reviewed, &names, "None reviewed yet.")
                    .title(format!("The latest {REVIEWED_ROWS}")),
            )],
        ))
}

fn review_view(viewer: &Viewer, app: i64, note: Option<&str>) -> Result<Page, PageError> {
    let a = reviewable(viewer, app)?;
    let names = names(
        a.characters
            .iter()
            .flat_map(|c| [Some(c.corporation_id), c.alliance_id])
            .flatten()
            .chain([a.main_corporation_id]),
    );
    let mut about = Card::new("Application")
        .field("Main Character", a.main_name.clone())
        .field("Main's corporation", name_of(&names, a.main_corporation_id))
        .field("Applying to", a.corporation_name.clone())
        .field("Applied", time_or(a.created_at, ""))
        .field("Status", a.status())
        .field(
            "Reviewer",
            if a.reviewer_name.is_empty() {
                "Nobody yet".to_owned()
            } else {
                a.reviewer_name.clone()
            },
        );
    if let Some(decided) = a.decided_at {
        about = about.field("Decided", time(rfc3339(decided)));
    }
    let mut characters = Table::new(vec![
        Column::text("Character"),
        Column::text("Corporation"),
        Column::text("Alliance"),
    ])
    .title("Characters")
    .empty("No characters were on the account.");
    if a.characters.len() > MAX_CHARACTERS_SHOWN {
        characters = characters.title(format!(
            "Characters: the first {MAX_CHARACTERS_SHOWN} of {}",
            a.characters.len()
        ));
    }
    for c in a.characters.iter().take(MAX_CHARACTERS_SHOWN) {
        characters = characters.row(vec![
            c.name.clone().into(),
            name_of(&names, c.corporation_id).into(),
            c.alliance_id
                .map_or_else(String::new, |id| name_of(&names, id))
                .into(),
        ]);
    }
    let mut page = Page::new("View Application").description(format!(
        "{} to {}. Characters as they were when they applied.",
        a.main_name, a.corporation_name
    ));
    if let Some(note) = note {
        page = page.text(note);
    }
    page = page.card(about).table(characters);
    for section in answer_cards(a.id)? {
        page = page.section(section);
    }
    let comments = query(
        "SELECT created_at, author_name, body FROM comments WHERE application_id = $1 \
         ORDER BY created_at, id LIMIT $2",
        &[a.id.into(), MAX_COMMENTS.into()],
    )?;
    let mut table = Table::new(vec![
        Column::numeric("When"),
        Column::text("By"),
        Column::text("Comment"),
    ])
    .title("Comments")
    .empty("No comments yet.");
    for c in &comments {
        table = table.row(vec![
            time_or(when(c, 0), ""),
            text(c, 1).into(),
            clip(&text(c, 2)).into(),
        ]);
    }
    page = page.table(table);

    let mine = a.reviewer_account_id == Some(viewer.account_id);
    let all = viewer.can("all_corporations");
    if a.pending() && a.reviewer_account_id.is_none() {
        page = page.form(
            Form::new("claim", "Mark in Progress")
                .description("You become its reviewer: only you can then approve or reject it.")
                .field(
                    Field::checkbox("confirm", "I'm reviewing this application", true).required(),
                ),
        );
    }
    let mut decisions = Vec::new();
    if viewer.can("approve_application") {
        decisions.push(("approve".to_owned(), "Approve".to_owned()));
    }
    if viewer.can("reject_application") {
        decisions.push(("reject".to_owned(), "Reject".to_owned()));
    }
    if a.pending() && (mine || all) && !decisions.is_empty() {
        page = page.form(
            Form::new("decide", "Save Decision")
                .description(format!(
                    "{} sees the decision on their applications page.",
                    a.main_name
                ))
                .field(Field::select("decision", "Decision", decisions).required()),
        );
    }
    page = page.form(
        Form::new("comment", "Add Comment")
            .description("Only reviewers see comments.")
            .field(Field::textarea("comment", "Comment", MAX_COMMENT).required()),
    );
    if viewer.can("delete_application") {
        page = page.form(
            Form::new("delete", "Delete Application")
                .description(format!(
                    "Its answers and comments go with it, and {} can apply again.",
                    a.main_name
                ))
                .field(
                    Field::checkbox("confirm", "Yes, delete this application", false).required(),
                ),
        );
    }
    Ok(page)
}

fn review_action(
    viewer: &Viewer,
    app: i64,
    submission: &Submission,
) -> Result<SubmitResult, PageError> {
    let a = reviewable(viewer, app)?;
    let me: [Db; 3] = [
        viewer.account_id.into(),
        viewer.main.id.into(),
        viewer.main.name.clone().into(),
    ];
    let back = || Ok(SubmitResult::Redirect(format!("review/{app}")));
    let note = |text: &str| Ok(SubmitResult::Page(review_view(viewer, app, Some(text))?));
    match submission.form.as_str() {
        "claim" => {
            let [account, character, name] = me;
            let claimed = storage::execute(
                "UPDATE applications SET reviewer_account_id = $2, reviewer_character_id = $3, \
                 reviewer_name = $4 \
                 WHERE id = $1 AND approved IS NULL AND reviewer_account_id IS NULL",
                &[app.into(), account, character, name],
            )
            .map_err(|e| failed("marking in progress", e))?;
            if claimed == 0 {
                return note("Someone is reviewing it already.");
            }
            log::info(format!(
                "application {app} marked in progress by {} ({})",
                viewer.main.name, viewer.main.id
            ));
            back()
        }
        "decide" => {
            let approve = match submission.value("decision") {
                "approve" if viewer.can("approve_application") => true,
                "reject" if viewer.can("reject_application") => false,
                _ => return Err(PageError::Forbidden),
            };
            let [account, character, name] = me;
            // Only its reviewer decides (anyone with all_corporations, as
            // AA's superusers), and only once.
            let decided = storage::execute(
                "UPDATE applications SET approved = $2, decided_at = now(), \
                 reviewer_account_id = $3, reviewer_character_id = $4, reviewer_name = $5 \
                 WHERE id = $1 AND approved IS NULL AND (reviewer_account_id = $3 OR $6)",
                &[
                    app.into(),
                    approve.into(),
                    account,
                    character,
                    name,
                    viewer.can("all_corporations").into(),
                ],
            )
            .map_err(|e| failed("saving the decision", e))?;
            if decided == 0 {
                return note("Only its reviewer can decide it, once: mark it in progress first.");
            }
            log::info(format!(
                "application {app} to {} {} by {} ({})",
                a.corporation_name,
                if approve { "approved" } else { "rejected" },
                viewer.main.name,
                viewer.main.id
            ));
            back()
        }
        "comment" => {
            let body = submission.value("comment").trim().to_owned();
            if body.is_empty() {
                return note("Write a comment first.");
            }
            let [account, character, name] = me;
            // At most MAX_COMMENTS, checked in the statement itself.
            let added = storage::execute(
                "INSERT INTO comments (application_id, author_account_id, author_character_id, \
                 author_name, body) \
                 SELECT $1, $2, $3, $4, $5 \
                 WHERE (SELECT count(*) FROM comments WHERE application_id = $1) < $6",
                &[
                    app.into(),
                    account,
                    character,
                    name,
                    body.into(),
                    MAX_COMMENTS.into(),
                ],
            )
            .map_err(|e| failed("adding the comment", e))?;
            if added == 0 {
                return note(&format!(
                    "An application holds at most {MAX_COMMENTS} comments."
                ));
            }
            log::info(format!(
                "comment on application {app} by {} ({})",
                viewer.main.name, viewer.main.id
            ));
            back()
        }
        "delete" => {
            if !viewer.can("delete_application") {
                return Err(PageError::Forbidden);
            }
            storage::execute("DELETE FROM applications WHERE id = $1", &[app.into()])
                .map_err(|e| failed("deleting the application", e))?;
            log::info(format!(
                "application {app} from {} to {} deleted by {} ({})",
                a.main_name, a.corporation_name, viewer.main.name, viewer.main.id
            ));
            Ok(SubmitResult::Redirect("review".to_owned()))
        }
        _ => Err(PageError::NotFound),
    }
}

// ---- forms -------------------------------------------------------------------

fn forms_page(viewer: &Viewer, note: Option<&str>) -> Result<Page, PageError> {
    let forms = query(
        "SELECT f.id, f.corporation_name, f.corporation_id, \
                (SELECT count(*) FROM questions q WHERE q.form_id = f.id), \
                (SELECT count(*) FROM applications a WHERE a.form_id = f.id AND a.approved IS NULL), \
                (SELECT count(*) FROM applications a WHERE a.form_id = f.id) \
         FROM forms f ORDER BY f.corporation_name LIMIT 500",
        &[],
    )?;
    let mut table = Table::new(vec![
        Column::text("Corporation"),
        Column::numeric("Questions"),
        Column::numeric("Pending"),
        Column::numeric("Applications"),
    ])
    .empty("No forms yet: create one below.");
    for f in &forms {
        table = table.row(vec![
            link(text(f, 1), format!("forms/{}", int(f, 0))).into(),
            int(f, 3).into(),
            int(f, 4).into(),
            int(f, 5).into(),
        ]);
    }
    let taken: Vec<i64> = forms.iter().map(|f| int(f, 2)).collect();
    let mut yours: Vec<i64> = viewer
        .characters
        .iter()
        .map(|c| c.corporation_id)
        .filter(|id| *id > 0 && !taken.contains(id))
        .collect();
    yours.sort_unstable();
    yours.dedup();
    let names = names(yours.iter().copied());
    let mut add = Form::new("add_form", "Create Form").title("New form").description(
        "One form per corporation. Pick one of your characters' corporations, or give a corporation's ID.",
    );
    if !yours.is_empty() {
        let options = yours
            .iter()
            .take(100)
            .map(|id| (id.to_string(), name_of(&names, *id)))
            .collect();
        add = add.field(Field::select("corporation", "Corporation", options));
    }
    add = add.field(
        Field::number("corporation_id", "Corporation ID")
            .range(Some(1.0), None, true)
            .help("Any corporation, by its EVE ID."),
    );
    let mut page = Page::new("Application Forms")
        .description("Which corporations take applications, and what they ask");
    if let Some(note) = note {
        page = page.text(note);
    }
    Ok(page.table(table).form(add))
}

fn add_form(viewer: &Viewer, submission: &Submission) -> Result<SubmitResult, PageError> {
    let again = |text: &str| Ok(SubmitResult::Page(forms_page(viewer, Some(text))?));
    let picked = submission.value("corporation");
    let typed = submission.value("corporation_id");
    let corporation: i64 = match (picked.is_empty(), typed.is_empty()) {
        (false, true) => picked,
        (true, false) => typed,
        _ => return again("Pick a corporation or give its ID, not both."),
    }
    .parse()
    .map_err(|_| PageError::Failed("corporation wasn't a number".to_owned()))?;
    let named = match esi::names(&[corporation]) {
        Ok(named) => named,
        Err(err) => {
            log::warn(format!("names: {err:?}"));
            return again("EVE couldn't be asked about that corporation just now: try again.");
        }
    };
    let Some(name) = named
        .into_iter()
        .find(|n| n.id == corporation && n.category == "corporation")
        .map(|n| n.name)
    else {
        return again("That isn't a corporation EVE knows.");
    };
    let added = storage::query(
        "INSERT INTO forms (corporation_id, corporation_name) VALUES ($1, $2) \
         ON CONFLICT (corporation_id) DO NOTHING RETURNING id",
        &[corporation.into(), name.clone().into()],
    )
    .map_err(|e| failed("adding the form", e))?;
    let Some(form) = added.rows.first().map(|r| int(r, 0)) else {
        return again(&format!("{name} has a form already."));
    };
    log::info(format!(
        "form {form} for {name} created by {} ({})",
        viewer.main.name, viewer.main.id
    ));
    Ok(SubmitResult::Redirect(format!("forms/{form}")))
}

fn question_options(list: &[Question]) -> Vec<(String, String)> {
    list.iter()
        .enumerate()
        .map(|(i, q)| {
            let title: String = q.title.chars().take(80).collect();
            (q.id.to_string(), format!("{}. {title}", i + 1))
        })
        .collect()
}

fn question_fields(form: Form, current: Option<&Question>) -> Form {
    let title = Field::text("title", "Question", MAX_TITLE).required();
    let help = Field::text("help_text", "Help text", MAX_HELP).help("Shown under the question.");
    let choices = Field::textarea("choices", "Choices", 5000)
        .help("One per line. Leave empty for a written answer.");
    let (title, help, choices) = match current {
        Some(q) => (
            title.value(q.title.clone()),
            help.value(q.help.clone()),
            choices.value(q.choices.join("\n")),
        ),
        None => (title, help, choices),
    };
    form.field(title)
        .field(help)
        .field(choices)
        .field(Field::checkbox(
            "multi_select",
            "Pilots may tick more than one choice",
            current.is_some_and(|q| q.multi_select),
        ))
}

fn form_page(form: i64, note: Option<&str>) -> Result<Page, PageError> {
    let corporation = form_name(form)?;
    let list = questions(form)?;
    let applications = query(
        "SELECT count(*) FROM applications WHERE form_id = $1",
        &[form.into()],
    )?
    .first()
    .map_or(0, |r| int(r, 0));
    let used: usize = list.iter().map(Question::fields).sum();
    let mut table = Table::new(vec![
        Column::numeric("#"),
        Column::text("Question"),
        Column::text("Answer"),
        Column::text("Choices"),
        Column::text("Help text"),
    ])
    .title("Questions")
    .empty("No questions yet: add the first below.");
    for (i, q) in list.iter().enumerate() {
        table = table.row(vec![
            count(i + 1).into(),
            link(q.title.clone(), format!("forms/{form}/question/{}", q.id)).into(),
            q.kind().into(),
            clip(&q.choices.join(", ")).into(),
            q.help.clone().into(),
        ]);
    }
    let mut page = Page::new(corporation.clone()).description(format!(
        "Application form. Answers take {used} of the {MAX_ANSWER_FIELDS} fields a form has (a tick-any question takes one per choice)."
    ));
    if let Some(note) = note {
        page = page.text(note);
    }
    page = page.table(table).form(question_fields(
        Form::new("add_question", "Add Question").title("New question"),
        None,
    ));
    if list.len() > 1 {
        page = page.form(
            Form::new("move_question", "Move")
                .title("Reorder")
                .field(Field::select("question", "Question", question_options(&list)).required())
                .field(
                    Field::select(
                        "direction",
                        "Direction",
                        vec![
                            ("up".to_owned(), "Up".to_owned()),
                            ("down".to_owned(), "Down".to_owned()),
                        ],
                    )
                    .required(),
                ),
        );
    }
    if !list.is_empty() {
        page = page.form(
            Form::new("delete_question", "Delete Question")
                .description("Applications already made keep their answers.")
                .field(Field::select("question", "Question", question_options(&list)).required())
                .field(Field::checkbox("confirm", "Yes, delete this question", false).required()),
        );
    }
    Ok(page.form(
        Form::new("delete_form", "Delete Form")
            .description(format!(
                "Its questions and its {applications} applications, answers and comments are deleted too."
            ))
            .field(
                Field::checkbox(
                    "confirm",
                    format!("Yes, delete {corporation}'s form"),
                    false,
                )
                .required(),
            ),
    ))
}

fn question_page(form: i64, question: i64, note: Option<&str>) -> Result<Page, PageError> {
    let corporation = form_name(form)?;
    let list = questions(form)?;
    let q = list
        .iter()
        .find(|q| q.id == question)
        .ok_or(PageError::NotFound)?;
    let mut page = Page::new("Edit Question").description(format!(
        "{corporation}'s application form. Applications already made keep their answers."
    ));
    if let Some(note) = note {
        page = page.text(note);
    }
    Ok(page
        .form(question_fields(
            Form::new("edit_question", "Save Question"),
            Some(q),
        ))
        .card(Card::new("Form").field("Back to", link(corporation, format!("forms/{form}")))))
}

/// A question as posted, checked: its title, help, choices and whether
/// any may be ticked; or what to fix. `others` are the form's other
/// questions, for the field budget.
fn posted_question(
    submission: &Submission,
    others: &[&Question],
) -> Result<(String, String, Vec<String>, bool), String> {
    let title = submission.value("title").trim().to_owned();
    if title.is_empty() {
        return Err("Write the question.".to_owned());
    }
    let choices = text::parse_choices(submission.value("choices"))?;
    let multi = submission.checked("multi_select");
    if multi && choices.is_empty() {
        return Err("Give the choices to tick, one per line.".to_owned());
    }
    let this = Question {
        id: 0,
        title: title.clone(),
        help: String::new(),
        choices: choices.clone(),
        multi_select: multi,
    };
    let used: usize = others.iter().map(|q| q.fields()).sum::<usize>() + this.fields();
    if used > MAX_ANSWER_FIELDS {
        return Err(format!(
            "That makes {used} answer fields; a form has {MAX_ANSWER_FIELDS}. Use fewer questions or choices."
        ));
    }
    let help = submission.value("help_text").trim().to_owned();
    Ok((title, help, choices, multi))
}

fn form_action(
    viewer: &Viewer,
    form: i64,
    submission: &Submission,
) -> Result<SubmitResult, PageError> {
    let corporation = form_name(form)?;
    let list = questions(form)?;
    let back = || Ok(SubmitResult::Redirect(format!("forms/{form}")));
    let note = |text: &str| Ok(SubmitResult::Page(form_page(form, Some(text))?));
    let logged = |what: &str| {
        log::info(format!(
            "{what} on form {form} ({corporation}) by {} ({})",
            viewer.main.name, viewer.main.id
        ));
    };
    let chosen = || -> Result<i64, PageError> {
        let q: i64 = submission
            .value("question")
            .parse()
            .map_err(|_| PageError::NotFound)?;
        list.iter()
            .find(|x| x.id == q)
            .map(|x| x.id)
            .ok_or(PageError::NotFound)
    };
    match submission.form.as_str() {
        "add_question" => {
            let others: Vec<&Question> = list.iter().collect();
            let (title, help, choices, multi) = match posted_question(submission, &others) {
                Ok(q) => q,
                Err(why) => return note(&why),
            };
            storage::execute(
                "INSERT INTO questions (form_id, position, title, help_text, choices, multi_select) \
                 SELECT $1, coalesce(max(position), 0) + 1, $2, $3, $4::jsonb, $5 \
                 FROM questions WHERE form_id = $1",
                &[
                    form.into(),
                    title.into(),
                    help.into(),
                    Db::json(serde_json::json!(choices).to_string()),
                    multi.into(),
                ],
            )
            .map_err(|e| failed("adding the question", e))?;
            logged("question added");
            back()
        }
        "move_question" => {
            let q = chosen()?;
            let mut order: Vec<i64> = list.iter().map(|x| x.id).collect();
            let Some(at) = order.iter().position(|x| *x == q) else {
                return Err(PageError::NotFound);
            };
            match submission.value("direction") {
                "up" if at > 0 => order.swap(at, at - 1),
                "down" if at + 1 < order.len() => order.swap(at, at + 1),
                _ => return back(),
            }
            let statements: Vec<Statement> = order
                .iter()
                .enumerate()
                .map(|(i, id)| {
                    Statement::new(
                        "UPDATE questions SET position = $1 WHERE id = $2 AND form_id = $3",
                        vec![count(i + 1).into(), (*id).into(), form.into()],
                    )
                })
                .collect();
            storage::transaction(&statements).map_err(|e| failed("reordering", e))?;
            logged(&format!("question {q} moved"));
            back()
        }
        "delete_question" => {
            let q = chosen()?;
            storage::execute(
                "DELETE FROM questions WHERE id = $1 AND form_id = $2",
                &[q.into(), form.into()],
            )
            .map_err(|e| failed("deleting the question", e))?;
            logged(&format!("question {q} deleted"));
            back()
        }
        "delete_form" => {
            storage::execute("DELETE FROM forms WHERE id = $1", &[form.into()])
                .map_err(|e| failed("deleting the form", e))?;
            log::info(format!(
                "form {form} for {corporation} deleted by {} ({})",
                viewer.main.name, viewer.main.id
            ));
            Ok(SubmitResult::Redirect("forms".to_owned()))
        }
        _ => Err(PageError::NotFound),
    }
}

fn edit_question(
    viewer: &Viewer,
    form: i64,
    question: i64,
    submission: &Submission,
) -> Result<SubmitResult, PageError> {
    let list = questions(form)?;
    if !list.iter().any(|q| q.id == question) {
        return Err(PageError::NotFound);
    }
    let others: Vec<&Question> = list.iter().filter(|q| q.id != question).collect();
    let (title, help, choices, multi) = match posted_question(submission, &others) {
        Ok(q) => q,
        Err(why) => {
            return Ok(SubmitResult::Page(question_page(
                form,
                question,
                Some(&why),
            )?));
        }
    };
    storage::execute(
        "UPDATE questions SET title = $3, help_text = $4, choices = $5::jsonb, multi_select = $6 \
         WHERE id = $1 AND form_id = $2",
        &[
            question.into(),
            form.into(),
            title.into(),
            help.into(),
            Db::json(serde_json::json!(choices).to_string()),
            multi.into(),
        ],
    )
    .map_err(|e| failed("saving the question", e))?;
    log::info(format!(
        "question {question} of form {form} edited by {} ({})",
        viewer.main.name, viewer.main.id
    ));
    Ok(SubmitResult::Redirect(format!("forms/{form}")))
}
