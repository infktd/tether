//! aa-memberaudit's data exports (`exports_access`): contracts, contract
//! items and the wallet journal of every character, as CSV files Tether
//! writes from the rows handed over here and serves to holders of
//! `exports_access`. Built by a job, a chunk of rows at a time, daily and
//! when someone asks (at most once an hour per topic, AA's
//! `MEMBERAUDIT_DATA_EXPORT_MIN_UPDATE_AGE`).

use serde_json::json;
use tether_plugin_sdk::downloads;
use tether_plugin_sdk::jobs::{self, JobError, NewJob};
use tether_plugin_sdk::storage::{self, Value as Db};
use tether_plugin_sdk::{
    Column, Page, PageError, SubmitResult, Table, Tone, Value, action, badge, link, time,
};

use crate::{name_of, query};

/// The job building one topic's file.
pub(crate) const JOB: &str = "export";
/// The daily schedule starting every topic.
pub(crate) const SCHEDULE: &str = "exports";
/// Rows read and handed over at once, and chunks in one run (each run is
/// one plugin call, with its time limit).
const CHUNK: i64 = 2_000;
const CHUNKS_PER_RUN: usize = 10;
/// AA's minimum age before a topic may be exported again, in minutes.
const MIN_UPDATE_AGE: i64 = 60;

/// AA's topics: name, title, what's in it.
const TOPICS: &[(&str, &str, &str)] = &[
    (
        "contract",
        "Contract",
        "Every character's contracts, with their parties, dates and values.",
    ),
    (
        "contract-item",
        "Contract item",
        "The items of every character's contracts, by the contract's pk.",
    ),
    (
        "wallet-journal",
        "Wallet journal",
        "Every character's wallet journal entries.",
    ),
];

fn failed(what: &str, err: impl std::fmt::Debug) -> JobError {
    JobError::Retry(format!("{what}: {err:?}"))
}

/// Why a build stopped: a newer one began, or it failed.
enum Stop {
    Superseded,
    Failed(JobError),
}

impl From<JobError> for Stop {
    fn from(err: JobError) -> Self {
        Self::Failed(err)
    }
}

/// A downloads call's result: only the host being unavailable is worth
/// retrying.
fn stored<T>(what: &str, result: Result<T, downloads::Error>) -> Result<T, Stop> {
    result.map_err(|err| match err {
        downloads::Error::Superseded => Stop::Superseded,
        downloads::Error::Unavailable => Stop::Failed(failed(what, err)),
        err => Stop::Failed(JobError::Permanent(format!("{what}: {err:?}"))),
    })
}

/// `snake_case` as words: `item_exchange` as "Item Exchange".
fn words(text: &str) -> String {
    text.split('_')
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut chars = w.chars();
            chars.next().map_or_else(String::new, |first| {
                first.to_uppercase().collect::<String>() + chars.as_str()
            })
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn cell(row: &[Db], i: usize) -> String {
    match row.get(i) {
        Some(Db::Text(t) | Db::Timestamp(t) | Db::Json(t)) => t.clone(),
        Some(Db::Integer(n)) => n.to_string(),
        Some(Db::Float(f)) => f.to_string(),
        Some(Db::Boolean(b)) => b.to_string(),
        _ => String::new(),
    }
}

fn yes_no(row: &[Db], i: usize) -> String {
    match row.get(i).and_then(Db::as_bool) {
        Some(true) => "yes".to_owned(),
        Some(false) => "no".to_owned(),
        None => String::new(),
    }
}

/// AA's date format: `2026-09-27 18:05:00`, or empty.
const DATE: &str = "to_char({} AT TIME ZONE 'UTC', 'YYYY-MM-DD HH24:MI:SS')";

fn date(column: &str) -> String {
    format!("coalesce({}, '')", DATE.replace("{}", column))
}

/// SQL naming the id in `column`, or empty when there is none.
fn name_or_blank(column: &str) -> String {
    format!(
        "CASE WHEN {column} IS NULL OR {column} <= 0 THEN '' ELSE {} END",
        name_of(column)
    )
}

/// The last row's keys: where the next chunk starts.
type Cursor = (i64, i64, i64);
/// A chunk's rows, and where the next one starts (none after the last).
type Chunk = (Vec<Vec<String>>, Option<Cursor>);

fn header(topic: &str) -> Vec<String> {
    let names: &[&str] = match topic {
        "contract" => &[
            "owner character",
            "owner corporation",
            "contract pk",
            "contract id",
            "contract_type",
            "status",
            "date issued",
            "date expired",
            "date accepted",
            "date completed",
            "availability",
            "issuer",
            "issuer corporation",
            "acceptor",
            "assignee",
            "reward",
            "collateral",
            "volume",
            "days to complete",
            "start location",
            "end location",
            "price",
            "buyout",
            "title",
        ],
        "contract-item" => &[
            "contract pk",
            "record id",
            "type",
            "quantity",
            "is included",
            "is singleton",
            "is blueprint",
            "is blueprint_original",
            "is blueprint_copy",
            "raw quantity",
        ],
        _ => &[
            "date",
            "owner character",
            "owner corporation",
            "entry id",
            "ref type",
            "first party",
            "second party",
            "amount",
            "balance",
            "context_id",
            "context_id_type",
            "tax",
            "tax_receiver",
            "description",
            "reason",
        ],
    };
    names.iter().map(|n| (*n).to_owned()).collect()
}

/// One chunk of a topic after the cursor (the last row's keys), as rows
/// of cells and the next cursor.
fn chunk(topic: &str, after: Cursor) -> Result<Chunk, JobError> {
    let (a, b, c) = after;
    let (acceptor, assignee) = (
        name_or_blank("k.acceptor_id"),
        name_or_blank("k.assignee_id"),
    );
    let (first, second, receiver) = (
        name_or_blank("j.first_party_id"),
        name_or_blank("j.second_party_id"),
        name_or_blank("j.tax_receiver_id"),
    );
    let params: Vec<Db> = vec![a.into(), b.into(), c.into(), CHUNK.into()];
    let (sql, keys): (String, usize) = match topic {
        "contract" => (
            format!(
                "SELECT ch.name, {owner_corp}, k.character_id || '-' || k.contract_id, k.contract_id, \
                 k.kind, k.status, {issued}, {expires}, {accepted}, {completed}, k.availability, \
                 {issuer}, {issuer_corp}, {acceptor}, {assignee}, k.reward, k.collateral, k.volume, \
                 k.days_to_complete, {start}, {end}, k.price, k.buyout, k.title, \
                 k.character_id, k.contract_id \
                 FROM contracts k JOIN characters ch ON ch.character_id = k.character_id \
                 WHERE (k.character_id, k.contract_id) > ($1, $2) AND $3 = $3 \
                 ORDER BY k.character_id, k.contract_id LIMIT $4",
                owner_corp = name_of("ch.corporation_id"),
                issued = date("k.issued"),
                expires = date("k.expires"),
                accepted = date("k.accepted"),
                completed = date("k.completed"),
                issuer = name_of("k.issuer_id"),
                issuer_corp = name_of("k.issuer_corporation_id"),
                start = name_of("k.start_location_id"),
                end = name_of("k.end_location_id"),
            ),
            24,
        ),
        "contract-item" => (
            format!(
                "SELECT i.character_id || '-' || i.contract_id, i.record_id, {kind}, i.quantity, \
                 i.is_included, i.is_singleton, coalesce(i.raw_quantity IN (-1, -2), false), \
                 coalesce(i.raw_quantity = -1, false), \
                 coalesce(i.raw_quantity = -2, false), i.raw_quantity, i.character_id, i.contract_id, i.record_id \
                 FROM contract_items i \
                 WHERE (i.character_id, i.contract_id, i.record_id) > ($1, $2, $3) \
                 ORDER BY i.character_id, i.contract_id, i.record_id LIMIT $4",
                kind = name_of("i.type_id"),
            ),
            10,
        ),
        _ => (
            format!(
                "SELECT {at}, ch.name, {owner_corp}, j.id, j.ref_type, {first}, {second}, j.amount, \
                 j.balance, j.context_id, j.context_id_type, j.tax, {receiver}, j.description, \
                 j.reason, j.character_id, j.id \
                 FROM journal j JOIN characters ch ON ch.character_id = j.character_id \
                 WHERE (j.character_id, j.id) > ($1, $2) AND $3 = $3 \
                 ORDER BY j.character_id, j.id LIMIT $4",
                at = date("j.at"),
                owner_corp = name_of("ch.corporation_id"),
            ),
            15,
        ),
    };
    let rows = query(&sql, &params).map_err(|e| failed("reading an export", e))?;
    let next = rows.last().map(|r| {
        let key = |i: usize| r.get(keys + i).and_then(Db::as_integer).unwrap_or(0);
        (
            key(0),
            key(1),
            if topic == "contract-item" { key(2) } else { 0 },
        )
    });
    let cells = rows
        .iter()
        .map(|r| match topic {
            "contract" => (0..24)
                .map(|i| match i {
                    4 | 5 | 10 => words(&cell(r, i)),
                    _ => cell(r, i),
                })
                .collect(),
            "contract-item" => (0..10)
                .map(|i| match i {
                    4..=8 => yes_no(r, i),
                    _ => cell(r, i),
                })
                .collect(),
            _ => (0..15)
                .map(|i| match i {
                    4 | 10 => words(&cell(r, i)),
                    _ => cell(r, i),
                })
                .collect(),
        })
        .collect::<Vec<Vec<String>>>();
    let more = i64::try_from(rows.len()).unwrap_or(0) == CHUNK;
    Ok((cells, next.filter(|_| more)))
}

/// Builds (part of) a topic's file: from the start when `after` is none,
/// else from there in its `build`; queues the rest when a run's chunks are
/// used. A newer build (the daily run, or someone's update) supersedes this
/// one, which then stops.
pub(crate) fn run(payload: &str) -> Result<(), JobError> {
    let payload: serde_json::Value = serde_json::from_str(payload).unwrap_or_default();
    let topic = payload["topic"].as_str().unwrap_or_default().to_owned();
    let Some((_, title, _)) = TOPICS.iter().find(|(t, _, _)| *t == topic) else {
        return Err(JobError::Permanent(format!("no export topic {topic:?}")));
    };
    let resumed = payload["build"]
        .as_u64()
        .and_then(|b| u32::try_from(b).ok())
        .zip(payload["after"].as_array());
    let (build, mut after) = match resumed {
        Some((build, keys)) => {
            let key = |i: usize| keys.get(i).and_then(serde_json::Value::as_i64).unwrap_or(0);
            (build, (key(0), key(1), key(2)))
        }
        None => {
            let begun = downloads::begin(&topic, title, "exports_access", &header(&topic));
            let build = match stored("starting an export", begun) {
                Ok(build) => build,
                Err(Stop::Failed(err)) => return Err(err),
                Err(Stop::Superseded) => return Ok(()),
            };
            (build, (i64::MIN, i64::MIN, i64::MIN))
        }
    };
    let mut built = || -> Result<Option<Cursor>, Stop> {
        for _ in 0..CHUNKS_PER_RUN {
            let (rows, next) = chunk(&topic, after)?;
            stored("adding rows", downloads::append(&topic, build, &rows))?;
            match next {
                Some(next) => after = next,
                None => {
                    stored("finishing an export", downloads::finish(&topic, build))?;
                    return Ok(None);
                }
            }
        }
        Ok(Some(after))
    };
    match built() {
        Ok(Some(after)) => jobs::enqueue(
            NewJob::new(JOB)
                // Its own key: the next start mustn't replace it, nor it the next start.
                .key(format!("export-{topic}-{build}"))
                .payload(
                    json!({ "topic": topic, "build": build, "after": [after.0, after.1, after.2] })
                        .to_string(),
                ),
        )
        .map_err(|e| failed("queueing the rest of an export", e)),
        Ok(None) | Err(Stop::Superseded) => Ok(()),
        Err(Stop::Failed(err)) => Err(err),
    }
}

/// The daily run: every topic, from the start.
pub(crate) fn all() -> Result<(), JobError> {
    for (topic, _, _) in TOPICS {
        start(topic).map_err(|e| failed("queueing an export", e))?;
    }
    Ok(())
}

fn start(topic: &str) -> Result<(), PageError> {
    storage::execute(
        "INSERT INTO export_runs (topic, asked_at) VALUES ($1, now()) \
         ON CONFLICT (topic) DO UPDATE SET asked_at = now()",
        &[topic.into()],
    )
    .map_err(|e| crate::failed("recording an export", e))?;
    jobs::enqueue(
        NewJob::new(JOB)
            .key(format!("export-{topic}"))
            .payload(json!({ "topic": topic }).to_string()),
    )
    .map_err(|e| crate::failed("queueing an export", e))
}

/// AA's Data Export page, for `exports_access`.
pub(crate) fn page(notice: Option<&str>) -> Result<Page, PageError> {
    let files = downloads::files();
    let recent: Vec<String> = query(
        "SELECT topic FROM export_runs WHERE asked_at > now() - $1 * interval '1 minute'",
        &[MIN_UPDATE_AGE.into()],
    )?
    .iter()
    .map(|r| crate::text(r, 0))
    .collect();
    let characters = query("SELECT count(*) FROM characters", &[])?
        .first()
        .and_then(|r| r.first())
        .and_then(Db::as_integer)
        .unwrap_or(0);
    let mut table = Table::new(vec![
        Column::text("Topic"),
        Column::text("Contents"),
        Column::numeric("Rows"),
        Column::text("Last updated"),
        Column::text(""),
        Column::text(""),
    ])
    .title("Data export");
    for (topic, title, about) in TOPICS {
        let file = files.iter().find(|f| f.name == *topic);
        let download: Value = match file {
            Some(_) => link("Download", format!("downloads/{topic}")).into(),
            None => "".into(),
        };
        let update: Value = if recent.iter().any(|t| t == topic) {
            badge("Updating, or updated in the last hour", Tone::Neutral).into()
        } else {
            action("Update", "update_export")
                .field("topic", *topic)
                .into()
        };
        table = table.row(vec![
            (*title).into(),
            (*about).into(),
            file.map_or_else(|| "".into(), |f| f.rows.to_string().into()),
            file.map_or_else(|| "Not yet".into(), |f| time(f.built_at.clone())),
            download,
            update,
        ]);
    }
    let page = Page::new("Data export").description(format!(
        "CSV files of all {characters} characters' data, as aa-memberaudit's data export. Each is \
         updated daily, and on request at most once an hour."
    ));
    let mut page = page;
    if let Some(notice) = notice {
        page = page.text(notice);
    }
    Ok(page.table(table))
}

/// "Update": starts a topic's export, unless it ran in the last hour.
pub(crate) fn update(topic: &str) -> Result<SubmitResult, PageError> {
    if !TOPICS.iter().any(|(t, _, _)| *t == topic) {
        return Err(PageError::NotFound);
    }
    let recent = !query(
        "SELECT 1 FROM export_runs WHERE topic = $1 AND asked_at > now() - $2 * interval '1 minute'",
        &[topic.into(), MIN_UPDATE_AGE.into()],
    )?
    .is_empty();
    let notice = if recent {
        "That export was updated in the last hour.".to_owned()
    } else {
        start(topic)?;
        format!("Data export for {topic} has been started. This can take a couple of minutes.")
    };
    Ok(SubmitResult::Page(page(Some(&notice))?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_read_as_words() {
        assert_eq!(words("item_exchange"), "Item Exchange");
        assert_eq!(words("player_donation"), "Player Donation");
        assert_eq!(words(""), "");
    }

    #[test]
    fn headers_match_the_cells() {
        assert_eq!(header("contract").len(), 24);
        assert_eq!(header("contract-item").len(), 10);
        assert_eq!(header("wallet-journal").len(), 15);
        for (topic, _, _) in TOPICS {
            assert!(!header(topic).is_empty());
        }
    }
}
