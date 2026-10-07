//! Contacts (aa-contacts).
//!
//! - **Tracked alliances and corporations**: each owner's (a character
//!   added by a holder of `manage_alliance_contacts` or
//!   `manage_corporation_contacts`, as aa-contacts' tokens) corporation, and
//!   its alliance, read hourly; a manager of that kind may update one now.
//!   Each is read once a run (owners share them), as the first owner in it
//!   whose login and roles let it, the longest untried first, and stored
//!   every few reads; what one run's ESI calls or time don't reach, a
//!   follow-up run reads. Labels that can't be read for a moment stay as
//!   they were.
//! - **Who sees them**: anyone with a character in that alliance or
//!   corporation; superusers every one (as aa-contacts).
//! - **Contacts**: each with its standing and labels; notes for
//!   `view_*_notes` (edited with `manage_*_contacts` too), and server links
//!   (a name, an address of any kind, a password) for `view_*_server_links`
//!   (managed with `manage_*_contacts` too).
//!
//! Not taken: aa-contacts' Secure Groups standings filter (apps don't learn
//! every character of an account, so can't judge one).

use chrono::{DateTime, Duration, Utc};
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
/// The colours offered, named as Tether draws them (Bootstrap's others,
/// stored before, draw as the nearest).
const COLORS: [(&str, &str); 4] = [
    ("secondary", "Grey"),
    ("success", "Blue"),
    ("warning", "Signal"),
    ("danger", "Red"),
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
                    // That one now, as aa-contacts' manual update.
                    jobs::enqueue(
                        NewJob::new(UPDATE)
                            .key(format!("update:{kind}:{id}"))
                            .payload(serde_json::json!({ "kind": kind, "id": id }).to_string()),
                    )
                    .map_err(|e| failed("queuing an update", e))?;
                    log::info(format!(
                        "{kind} {id} updated on request of {} ({})",
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
            UPDATE => update(&job),
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
    linked(kind, id, name, None)
}

/// An entity whose name opens one of the app's pages about it (a row's
/// name opening its record).
fn linked(kind: &str, id: i64, name: String, path: Option<String>) -> Value {
    let picture = match kind {
        "character" => character(id, name),
        "corporation" => corporation(id, name),
        "alliance" => alliance(id, name),
        "faction" => faction(id, name),
        _ => {
            return match path {
                Some(path) => link(name, path).into(),
                None => name.into(),
            };
        }
    };
    match path {
        Some(path) => picture.link(path).into(),
        None => picture.into(),
    }
}

/// A standing as EVE colours it: blue above zero, red below, neutral
/// plain.
fn standing(value: f64) -> Value {
    if value > 0.0 {
        badge(format!("{value:+.1}"), Tone::Success).into()
    } else if value < 0.0 {
        badge(format!("{value:+.1}"), Tone::Danger).into()
    } else {
        "0.0".into()
    }
}

fn word(kind: &str) -> &'static str {
    match kind {
        "alliance" => "Alliance",
        _ => "Corporation",
    }
}

// ---- the update ----------------------------------------------------------------

/// ESI calls one job run may make (the host's limit).
const ESI_CALLS: usize = 100;
/// Name lookups (a thousand ids each) a run keeps room for.
const NAME_CALLS: usize = 4;
/// Kept for a run's end: the last look at the data sources, and names.
const RESERVE: usize = 1 + NAME_CALLS;
/// Alliances and corporations read between looks at the data sources
/// (an ESI call each), after which what was read is stored.
const CHECK_EVERY: usize = 4;
/// A job run has a minute, ESI's waits included: no alliance or
/// corporation is begun after this long into a run, and a follow-up run
/// reads the rest.
const READ_FOR: std::time::Duration = std::time::Duration::from_secs(30);
/// A follow-up run, when one run didn't reach every alliance and
/// corporation, waits this long.
const FOLLOW_UP_SECONDS: i64 = 60;

/// What a run has spent of its ESI calls.
struct Budget {
    used: usize,
}

impl Budget {
    /// Calls left for reading contacts.
    fn left(&self) -> usize {
        ESI_CALLS.saturating_sub(RESERVE + self.used)
    }

    fn spend(&mut self, calls: usize) {
        self.used += calls;
    }
}

/// Whether a run may begin its `n`th alliance or corporation, `elapsed`
/// into it. The first is always begun, so none waits forever.
fn may_begin(n: usize, elapsed: std::time::Duration) -> bool {
    n == 0 || elapsed < READ_FOR
}

/// An alliance or corporation to read, and the owners in it (data
/// sources, by name): it's read as the first whose read goes through.
#[derive(Debug, Clone, PartialEq)]
struct Target {
    kind: &'static str,
    id: i64,
    sources: Vec<i64>,
}

/// Each owner's corporation and alliance, once each (owners share an
/// alliance, or a corporation), with every owner in it.
fn targets(sources: &[esi::Character]) -> Vec<Target> {
    let mut out: Vec<Target> = Vec::new();
    for s in sources {
        let mut theirs = vec![("corporation", s.corporation_id)];
        if let Some(alliance) = s.alliance_id {
            theirs.push(("alliance", alliance));
        }
        for (kind, id) in theirs {
            match out.iter_mut().find(|t| t.kind == kind && t.id == id) {
                Some(t) => t.sources.push(s.id),
                None => out.push(Target {
                    kind,
                    id,
                    sources: vec![s.id],
                }),
            }
        }
    }
    out
}

/// Whether `sources` still read `target` as data source `source`.
fn still_reads(sources: &[esi::Character], target: &Target, source: i64) -> bool {
    sources.iter().any(|s| {
        s.id == source
            && match target.kind {
                "alliance" => s.alliance_id == Some(target.id),
                _ => s.corporation_id == target.id,
            }
    })
}

fn targets_json(targets: &[Target]) -> Db {
    let rows: Vec<serde_json::Value> = targets
        .iter()
        .map(|t| serde_json::json!({ "kind": t.kind, "entity_id": t.id }))
        .collect();
    Db::json(serde_json::Value::Array(rows).to_string())
}

/// The hourly update and its follow-ups: every owner's corporation and
/// alliance not tried since the hourly run began (`since`), the longest
/// untried first, as far as the run's ESI calls and time go; a follow-up
/// run takes the rest. What's read is stored every few, so a run cut
/// short keeps what it did. Update now asks for one (`kind`, `id`), as
/// aa-contacts' manual update.
fn update(job: &Job) -> Result<(), JobError> {
    let started = std::time::Instant::now();
    let asked: serde_json::Value = serde_json::from_str(&job.payload).unwrap_or_default();
    let sources = esi::data_sources();
    let mut budget = Budget { used: 1 };
    let all = targets(&sources);
    storage::execute(
        "INSERT INTO tracked (kind, entity_id) \
         SELECT kind, entity_id FROM json_to_recordset($1::json) AS x(kind text, entity_id bigint) \
         ON CONFLICT DO NOTHING",
        &[targets_json(&all)],
    )
    .map_err(|e| JobError::Retry(format!("tracking: {e:?}")))?;
    let (due, since) = match (asked["kind"].as_str(), asked["id"].as_i64()) {
        (Some(kind), Some(id)) => (
            all.iter()
                .filter(|t| t.kind == kind && t.id == id)
                .cloned()
                .collect(),
            None,
        ),
        _ => {
            let since = match asked["since"].as_str() {
                Some(since) => Db::timestamp(since),
                None => storage::query("SELECT now()", &[])
                    .ok()
                    .and_then(|r| r.rows.first().and_then(|r| r.first().cloned()))
                    .ok_or_else(|| JobError::Retry("reading the time".to_owned()))?,
            };
            (due(&all, &since)?, Some(since))
        }
    };
    let mut read_now = Vec::new();
    let mut unreached = false;
    for (n, target) in due.iter().enumerate() {
        if !may_begin(n, started.elapsed()) {
            unreached = true;
            break;
        }
        match read(target, &mut budget, n == 0) {
            Read::NoRoom => {
                unreached = true;
                break;
            }
            Read::Failed(why) => {
                log::warn(format!("{} {}: {why}", target.kind, target.id));
                tried(target, Some(why))?;
            }
            Read::Done(done) => {
                read_now.push((target, done));
                if read_now.len() >= CHECK_EVERY {
                    keep(&mut read_now, &mut budget)?;
                }
            }
        }
    }
    keep(&mut read_now, &mut budget)?;
    learn_names()?;
    if unreached && let Some(since) = since {
        let since = since.as_text().unwrap_or_default().to_owned();
        jobs::enqueue(
            NewJob::new(UPDATE)
                .key("update-more")
                .payload(serde_json::json!({ "since": since }).to_string())
                .at(rfc3339(Utc::now() + Duration::seconds(FOLLOW_UP_SECONDS))),
        )
        .map_err(|e| JobError::Retry(format!("queuing the rest: {e:?}")))?;
    }
    Ok(())
}

/// Stores what's been read, after a look at the data sources. The host
/// reads a source's corporation or alliance as it is at the time: only
/// what's still the one asked for is kept, so a corporation that changed
/// alliance mid-run can't file one alliance's standings as another's.
fn keep(read_now: &mut Vec<(&Target, Done)>, budget: &mut Budget) -> Result<(), JobError> {
    if read_now.is_empty() {
        return Ok(());
    }
    let sources = esi::data_sources();
    budget.spend(1);
    for (target, done) in read_now.drain(..) {
        if still_reads(&sources, target, done.source) {
            store(target, &done.contacts, done.labels)?;
        } else {
            log::info(format!(
                "{} {}: owner {} moved on while reading; skipped",
                target.kind, target.id, done.source
            ));
            tried(target, None)?;
        }
    }
    Ok(())
}

/// Of `all`, those not tried since `since`, the longest untried first.
fn due(all: &[Target], since: &Db) -> Result<Vec<Target>, JobError> {
    let rows = storage::query(
        "SELECT kind, entity_id FROM tracked \
         WHERE attempted_at IS NULL OR attempted_at < $1 \
         ORDER BY attempted_at NULLS FIRST, kind, entity_id",
        std::slice::from_ref(since),
    )
    .map_err(|e| JobError::Retry(format!("reading tracked: {e:?}")))?;
    Ok(rows
        .rows
        .iter()
        .filter_map(|r| {
            let (kind, id) = (text(r, 0), int(r, 1));
            all.iter().find(|t| t.kind == kind && t.id == id).cloned()
        })
        .collect())
}

/// Notes that `target` was tried, and why it wasn't read, if it wasn't.
fn tried(target: &Target, why: Option<String>) -> Result<(), JobError> {
    storage::execute(
        "UPDATE tracked SET attempted_at = now(), last_error = coalesce($3, last_error) \
         WHERE kind = $1 AND entity_id = $2",
        &[target.kind.into(), target.id.into(), why.into()],
    )
    .map_err(|e| JobError::Retry(format!("noting {} {}: {e:?}", target.kind, target.id)))?;
    Ok(())
}

/// One alliance's or corporation's reading.
enum Read {
    Done(Done),
    Failed(String),
    /// The run's ESI calls don't reach it: for a follow-up run.
    NoRoom,
}

/// What was read, and as which owner.
struct Done {
    source: i64,
    /// Every page.
    contacts: Vec<serde_json::Value>,
    /// Or why they weren't read.
    labels: Result<Vec<serde_json::Value>, String>,
}

/// A JSON array, or nothing if it isn't one.
fn array(body: &str) -> Option<Vec<serde_json::Value>> {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.as_array().cloned())
}

/// Whether ESI refused an owner for something of that owner's own (its
/// login, its roles, its standing as a data source), so another owner in
/// the same alliance or corporation may read it.
fn owners_own(err: &esi::Error) -> bool {
    matches!(
        err,
        esi::Error::Token | esi::Error::NotADataSource | esi::Error::Status(401 | 403)
    )
}

/// A target's contacts and labels within the run's ESI calls, as the
/// first of its owners whose read goes through. The run's first target is
/// read whatever its size, so none waits forever.
fn read(target: &Target, budget: &mut Budget, first: bool) -> Read {
    let endpoint = format!("{}-contacts", target.kind);
    let unread = |e: &esi::Error| format!("contacts not read: {}", esi::describe(e));
    let mut refused: Option<esi::Error> = None;
    let mut asked = 0;
    for &source in &target.sources {
        // Its first page and its labels, at least.
        if budget.left() < 2 {
            return match refused {
                Some(e) if first => Read::Failed(unread(&e)),
                _ => Read::NoRoom,
            };
        }
        budget.spend(1);
        asked += 1;
        match esi::get(&endpoint, Subject::DataSource(source), &[], Some(1)) {
            Ok(page) => return read_rest(target, source, page, budget, first),
            // That owner's own problem: the next one in it may read it.
            Err(e) if owners_own(&e) => {
                log::info(format!(
                    "{} {}: owner {source}: {}",
                    target.kind,
                    target.id,
                    esi::describe(&e)
                ));
                refused = Some(e);
            }
            Err(e) => return Read::Failed(unread(&e)),
        }
    }
    Read::Failed(match refused {
        Some(e) if asked > 1 => format!(
            "contacts not read by any of its {asked} owners: {}",
            esi::describe(&e)
        ),
        Some(e) => unread(&e),
        None => "contacts not read: no owner is in it".to_owned(),
    })
}

/// The rest of a target's contacts after their first page, and its labels.
fn read_rest(
    target: &Target,
    source: i64,
    page: esi::Response,
    budget: &mut Budget,
    first: bool,
) -> Read {
    let endpoint = format!("{}-contacts", target.kind);
    let subject = Subject::DataSource(source);
    let unread = |e: &esi::Error| format!("contacts not read: {}", esi::describe(e));
    let unreadable = || "contacts not read: ESI's answer couldn't be read".to_owned();
    let more = page.pages.saturating_sub(1) as usize;
    if more + 1 > budget.left() {
        return if first {
            Read::Failed(format!(
                "its {} pages of contacts are more than one update may read",
                page.pages
            ))
        } else {
            Read::NoRoom
        };
    }
    let Some(mut contacts) = array(&page.body) else {
        return Read::Failed(unreadable());
    };
    for n in 2..=page.pages {
        budget.spend(1);
        match esi::get(&endpoint, subject, &[], Some(n)).map(|p| array(&p.body)) {
            Ok(Some(more)) => contacts.extend(more),
            Ok(None) => return Read::Failed(unreadable()),
            Err(e) => return Read::Failed(unread(&e)),
        }
    }
    budget.spend(1);
    let labels = esi::get(
        &format!("{}-contact-labels", target.kind),
        subject,
        &[],
        None,
    )
    .map_err(|e| format!("labels not read: {}", esi::describe(&e)))
    .and_then(|r| {
        array(&r.body).ok_or_else(|| "labels not read: ESI's answer couldn't be read".to_owned())
    });
    Read::Done(Done {
        source,
        contacts,
        labels,
    })
}

/// Stores a target's contacts, and its labels if they were read (else the
/// ones it had stay, and why is noted).
fn store(
    target: &Target,
    contacts: &[serde_json::Value],
    labels: Result<Vec<serde_json::Value>, String>,
) -> Result<(), JobError> {
    let (kind, entity_id) = (target.kind, target.id);
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
    let rows = Db::json(serde_json::Value::Array(rows).to_string());
    let mut statements = vec![
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
    ];
    let problem = match labels {
        Ok(labels) => {
            let labels: Vec<serde_json::Value> = labels
                .iter()
                .filter_map(|l| {
                    Some(serde_json::json!({
                        "label_id": l["label_id"].as_i64()?,
                        "name": l["label_name"].as_str()?,
                    }))
                })
                .collect();
            statements.push(Statement::new(
                "DELETE FROM labels WHERE kind = $1 AND entity_id = $2",
                vec![kind.into(), entity_id.into()],
            ));
            statements.push(Statement::new(
                "INSERT INTO labels (kind, entity_id, label_id, name) \
                 SELECT DISTINCT ON (label_id) $1, $2, label_id, name \
                 FROM json_to_recordset($3::json) AS x(label_id bigint, name text)",
                vec![
                    kind.into(),
                    entity_id.into(),
                    Db::json(serde_json::Value::Array(labels).to_string()),
                ],
            ));
            None
        }
        // A labels read that failed for a moment mustn't take every
        // label's name away: they stay as they were.
        Err(why) => {
            log::warn(format!("{kind} {entity_id}: {why}"));
            Some(why)
        }
    };
    statements.push(Statement::new(
        "UPDATE tracked SET updated_at = now(), attempted_at = now(), last_error = $3 \
         WHERE kind = $1 AND entity_id = $2",
        vec![kind.into(), entity_id.into(), problem.into()],
    ));
    storage::transaction(&statements)
        .map_err(|e| JobError::Retry(format!("storing {kind} {entity_id}: {e:?}")))?;
    Ok(())
}

/// Names for the contacts and the alliances and corporations not named
/// yet, a thousand at a time, as far as the run's calls kept for them go.
fn learn_names() -> Result<(), JobError> {
    let missing = storage::query(
        "SELECT id FROM (SELECT contact_id AS id FROM contacts UNION SELECT entity_id FROM tracked) x \
         WHERE id > 0 AND NOT EXISTS (SELECT 1 FROM names n WHERE n.id = x.id) \
         ORDER BY id LIMIT $1",
        &[((NAME_CALLS * 1000) as i64).into()],
    )
    .map_err(|e| JobError::Retry(format!("reading names: {e:?}")))?;
    let missing: Vec<i64> = missing.rows.iter().map(|r| int(r, 0)).collect();
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
            Err(err) => log::warn(format!("names: {}", esi::describe(&err))),
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
            linked(&kind, id, shown, Some(format!("{kind}/{id}"))),
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
        // The contact's name opens its notes and server links.
        let mut row: Vec<Value> = vec![
            linked(
                &kind_of_contact,
                contact,
                shown,
                (notes || links).then(|| format!("{kind}/{id}/contact/{contact}")),
            ),
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
        table = table.row(row);
    }
    let mut page = Page::new(format!("{} contacts: {}", word(kind), name(id)?))
        .description("Standings and labels as set in EVE, read hourly")
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
        .link("All contacts", format!("{kind}/{id}"))
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
                "success" | "primary" | "info" => Tone::Success,
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

    fn owner(id: i64, corporation_id: i64, alliance_id: Option<i64>) -> esi::Character {
        esi::Character {
            id,
            name: format!("Owner {id}"),
            corporation_id,
            alliance_id,
        }
    }

    #[test]
    fn each_corporation_and_alliance_is_read_once() {
        // Twenty corporations' owners in one alliance, two in the first.
        let mut owners: Vec<esi::Character> =
            (0..20).map(|n| owner(n, 1000 + n, Some(99))).collect();
        owners.push(owner(50, 1000, Some(99)));
        owners.push(owner(51, 2000, None));
        let read = targets(&owners);
        assert_eq!(read.len(), 22);
        let alliances: Vec<&Target> = read.iter().filter(|t| t.kind == "alliance").collect();
        assert_eq!(alliances.len(), 1);
        // Every owner in it, in order: if the first can't read it, the
        // next one may.
        let mut in_it: Vec<i64> = (0..20).collect();
        in_it.push(50);
        assert_eq!(alliances[0].sources, in_it);
        let first = read
            .iter()
            .find(|t| t.kind == "corporation" && t.id == 1000)
            .unwrap();
        assert_eq!(first.sources, vec![0, 50]);
        assert_eq!(
            read.iter()
                .filter(|t| t.kind == "corporation" && t.id == 1000)
                .count(),
            1
        );
        // A source that moved on no longer reads what it was asked for.
        let target = alliances[0];
        assert!(still_reads(&owners, target, 0));
        assert!(still_reads(&owners, target, 50));
        assert!(!still_reads(&owners, target, 51));
        assert!(!still_reads(&[owner(0, 1000, Some(98))], target, 0));
    }

    #[test]
    fn an_owner_s_own_refusal_passes_to_the_next() {
        assert!(owners_own(&esi::Error::Token));
        assert!(owners_own(&esi::Error::Status(403)));
        assert!(owners_own(&esi::Error::NotADataSource));
        assert!(!owners_own(&esi::Error::Status(502)));
        assert!(!owners_own(&esi::Error::Unavailable));
    }

    #[test]
    fn a_run_begins_nothing_late() {
        use std::time::Duration;
        assert!(may_begin(0, Duration::from_secs(55)));
        assert!(may_begin(5, Duration::from_secs(10)));
        assert!(!may_begin(5, READ_FOR));
    }

    #[test]
    fn a_run_keeps_calls_for_its_end() {
        let mut budget = Budget { used: 1 };
        assert_eq!(budget.left(), ESI_CALLS - RESERVE - 1);
        budget.spend(ESI_CALLS);
        assert_eq!(budget.left(), 0);
    }
}
