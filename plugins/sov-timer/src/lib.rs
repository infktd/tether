//! Sovereignty Timer (aa-sov-timer).
//!
//! Sovereignty campaigns across New Eden, as ESI lists them: the system,
//! its constellation and region, the defending alliance, the system's
//! activity defense multiplier (ADM), when the campaign starts and how
//! long until then, and, once active, the defender's progress: the score
//! the last sync saw, the trend, and the score now. Filtered as
//! aa-sov-timer's: every campaign, those starting within four hours, and
//! the active ones.
//!
//! A sync every 30 seconds, as aa-sov-timer's task, stores the campaigns
//! and their scores (each chaining the next; the five-minute schedule
//! restarts the chain if it breaks) and learns names, regions and ADM. The
//! page reads only what's stored, reloading every 30 seconds: however many
//! have it open, ESI is asked by the sync alone.

use chrono::{DateTime, Duration, Utc};
use tether_plugin_sdk::esi::{self, Subject};
use tether_plugin_sdk::identity::{self, Viewer};
use tether_plugin_sdk::jobs::{self, Job, JobError, NewJob};
use tether_plugin_sdk::storage::{self, Statement, Value as Db};
use tether_plugin_sdk::{
    Column, Page, PageError, Plugin, Request, Section, Stat, Table, Tone, Value, alliance, badge,
    countdown, log, time,
};

/// Public endpoints take any subject.
const PUBLIC: Subject = Subject::Character(0);
/// The job's (and schedule's) name.
const SYNC: &str = "sync";
/// aa-sov-timer's "upcoming": starting within four hours.
const UPCOMING_HOURS: i64 = 4;
/// Seconds between reloads while the page is open.
const REFRESH: u32 = 30;
/// Seconds between syncs.
const EVERY: i64 = 30;
/// Constellations looked up a run at most (each is a call).
const CONSTELLATIONS_PER_RUN: usize = 40;

struct SovTimer;

impl Plugin for SovTimer {
    fn render(request: Request) -> Result<Page, PageError> {
        let viewer = identity::viewer().ok_or(PageError::Forbidden)?;
        match request.path.as_str() {
            "" if viewer.can("basic_access") => campaigns_page(&viewer),
            _ => Err(PageError::NotFound),
        }
    }

    fn run_job(job: Job) -> Result<(), JobError> {
        match job.name.as_str() {
            SYNC => sync(),
            other => Err(JobError::Permanent(format!("no job {other}"))),
        }
    }
}

tether_plugin_sdk::export!(SovTimer);

// ---- helpers ---------------------------------------------------------------

fn failed(what: &str, err: impl std::fmt::Debug) -> PageError {
    PageError::Failed(format!("{what}: {err:?}"))
}

fn retry(what: &str, err: impl std::fmt::Debug) -> JobError {
    JobError::Retry(format!("{what}: {err:?}"))
}

fn int(row: &[Db], i: usize) -> i64 {
    row.get(i).and_then(Db::as_integer).unwrap_or_default()
}

fn opt_int(row: &[Db], i: usize) -> Option<i64> {
    row.get(i).and_then(Db::as_integer)
}

fn float(row: &[Db], i: usize) -> Option<f64> {
    row.get(i).and_then(Db::as_float)
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

/// A campaign as ESI lists it.
#[derive(Debug, Clone, PartialEq)]
struct Campaign {
    id: i64,
    event_type: String,
    system: i64,
    constellation: i64,
    defender: Option<i64>,
    start: DateTime<Utc>,
    score: Option<f64>,
}

/// `sovereignty-campaigns`' body.
fn parse_campaigns(body: &str) -> Vec<Campaign> {
    let value: serde_json::Value = serde_json::from_str(body).unwrap_or_default();
    value
        .as_array()
        .map(|all| {
            all.iter()
                .filter_map(|c| {
                    Some(Campaign {
                        id: c["campaign_id"].as_i64()?,
                        event_type: c["event_type"].as_str().unwrap_or_default().to_owned(),
                        system: c["solar_system_id"].as_i64()?,
                        constellation: c["constellation_id"].as_i64()?,
                        defender: c["defender_id"].as_i64(),
                        start: DateTime::parse_from_rfc3339(c["start_time"].as_str()?)
                            .ok()?
                            .with_timezone(&Utc),
                        score: c["defender_score"].as_f64(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// What a campaign is about, in words.
fn event_name(event_type: &str) -> String {
    match event_type {
        "tcu_defense" => "TCU defense".to_owned(),
        "ihub_defense" => "IHub defense".to_owned(),
        "station_defense" => "Station defense".to_owned(),
        "station_freeport" => "Station freeport".to_owned(),
        "sovhub_defense" | "sovhub" => "Sov Hub defense".to_owned(),
        other => other.replace('_', " "),
    }
}

/// A score as aa-sov-timer shows it: a whole percent.
fn percent(score: f64) -> String {
    format!("{:.0}%", score * 100.0)
}

/// Whether a campaign has started, starts within four hours, or later.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    Active,
    Upcoming,
    Later,
}

fn status(start: DateTime<Utc>, now: DateTime<Utc>) -> Status {
    if start <= now {
        Status::Active
    } else if start - now <= Duration::hours(UPCOMING_HOURS) {
        Status::Upcoming
    } else {
        Status::Later
    }
}

// ---- the sync ----------------------------------------------------------------

/// Queues the next sync, then reads and stores this one's.
fn sync() -> Result<(), JobError> {
    // The next one first, so the chain goes on whatever happens below.
    jobs::enqueue(
        NewJob::new(SYNC)
            .key("sync-next")
            .at(rfc3339(Utc::now() + Duration::seconds(EVERY))),
    )
    .map_err(|e| retry("queuing the next sync", e))?;
    // A failure (ESI down at downtime, say) waits for that next sync, as
    // aa-sov-timer's task logs it and tries again at its next beat. A
    // retry couldn't happen anyway: with the next sync queued under the
    // same key, the queue ends a failed one at once ("replaced by a newer
    // job") and drops its error.
    if let Err(JobError::Retry(why) | JobError::Permanent(why)) = read_and_store() {
        log::warn(format!("{why}; the next sync is in {EVERY} seconds"));
    }
    Ok(())
}

/// Stores the campaigns (keeping each one's last score as the previous),
/// and learns their constellations' regions, their systems' ADM and the
/// names.
fn read_and_store() -> Result<(), JobError> {
    let body = esi::get("sovereignty-campaigns", PUBLIC, &[], None)
        .map_err(|e| JobError::Retry(format!("reading campaigns: {}", esi::describe(&e))))?
        .body;
    let campaigns = parse_campaigns(&body);
    let rows: Vec<serde_json::Value> = campaigns
        .iter()
        .map(|c| {
            serde_json::json!({
                "campaign_id": c.id,
                "event_type": c.event_type,
                "system_id": c.system,
                "constellation_id": c.constellation,
                "defender_id": c.defender,
                "start_time": rfc3339(c.start),
                "defender_score": c.score,
            })
        })
        .collect();
    storage::transaction(&[
        Statement::new(
            "DELETE FROM campaigns WHERE campaign_id NOT IN \
             (SELECT campaign_id FROM json_to_recordset($1::json) AS x(campaign_id bigint))",
            vec![Db::json(serde_json::Value::Array(rows.clone()).to_string())],
        ),
        Statement::new(
            "INSERT INTO campaigns (campaign_id, event_type, system_id, constellation_id, \
                 defender_id, start_time, defender_score, previous_score) \
             SELECT campaign_id, event_type, system_id, constellation_id, defender_id, start_time, \
                 defender_score, defender_score \
             FROM json_to_recordset($1::json) AS x(campaign_id bigint, event_type text, \
                 system_id bigint, constellation_id bigint, defender_id bigint, \
                 start_time timestamptz, defender_score double precision) \
             ON CONFLICT (campaign_id) DO UPDATE SET event_type = EXCLUDED.event_type, \
                 defender_id = EXCLUDED.defender_id, start_time = EXCLUDED.start_time, \
                 previous_score = campaigns.defender_score, \
                 defender_score = EXCLUDED.defender_score",
            vec![Db::json(serde_json::Value::Array(rows).to_string())],
        ),
    ])
    .map_err(|e| retry("storing campaigns", e))?;

    // Regions of constellations not looked up yet.
    let unknown = storage::query(
        &format!(
            "SELECT DISTINCT c.constellation_id FROM campaigns c \
             WHERE NOT EXISTS (SELECT 1 FROM constellations k \
                 WHERE k.constellation_id = c.constellation_id) \
             LIMIT {CONSTELLATIONS_PER_RUN}"
        ),
        &[],
    )
    .map_err(|e| retry("reading constellations", e))?;
    for row in &unknown.rows {
        let id = int(row, 0);
        let answer = esi::get(
            "universe-constellation",
            PUBLIC,
            &[("constellation_id".to_owned(), id.to_string())],
            None,
        );
        let Ok(answer) = answer else {
            log::warn(format!("constellation {id}: not read"));
            continue;
        };
        let value: serde_json::Value = serde_json::from_str(&answer.body).unwrap_or_default();
        let (Some(region), Some(name)) = (value["region_id"].as_i64(), value["name"].as_str())
        else {
            continue;
        };
        storage::transaction(&[
            Statement::new(
                "INSERT INTO constellations (constellation_id, region_id) VALUES ($1, $2) \
                 ON CONFLICT (constellation_id) DO UPDATE SET region_id = EXCLUDED.region_id",
                vec![id.into(), region.into()],
            ),
            Statement::new(
                "INSERT INTO names (id, name) VALUES ($1, $2) \
                 ON CONFLICT (id) DO UPDATE SET name = EXCLUDED.name",
                vec![id.into(), name.into()],
            ),
        ])
        .map_err(|e| retry("storing a constellation", e))?;
    }

    // ADM, for the campaigns' systems.
    match esi::get("sovereignty-systems", PUBLIC, &[], None) {
        Ok(answer) => {
            storage::transaction(&[
                Statement::new("DELETE FROM adm", vec![]),
                Statement::new(
                    "INSERT INTO adm (system_id, adm) \
                     SELECT DISTINCT ON (system_id) system_id, adm \
                     FROM json_to_recordset($1::json) AS x(system_id bigint, adm double precision) \
                     WHERE adm IS NOT NULL \
                       AND system_id IN (SELECT system_id FROM campaigns)",
                    vec![Db::json(answer.body)],
                ),
            ])
            .map_err(|e| retry("storing ADM", e))?;
        }
        Err(err) => log::warn(format!("sovereignty: {err:?}")),
    }

    // Names: systems, regions, alliances.
    let missing = storage::query(
        "SELECT id FROM (SELECT system_id AS id FROM campaigns \
             UNION SELECT defender_id FROM campaigns WHERE defender_id IS NOT NULL \
             UNION SELECT region_id FROM constellations \
             UNION SELECT constellation_id FROM campaigns) i \
         WHERE NOT EXISTS (SELECT 1 FROM names n WHERE n.id = i.id) LIMIT 1000",
        &[],
    )
    .map_err(|e| retry("finding names", e))?;
    let ids: Vec<i64> = missing
        .rows
        .iter()
        .map(|r| int(r, 0))
        .filter(|id| *id > 0)
        .collect();
    if !ids.is_empty() {
        match esi::names(&ids) {
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
                .map_err(|e| retry("storing names", e))?;
            }
            Err(err) => log::warn(format!("names: {err:?}")),
        }
    }
    Ok(())
}

// ---- the page -----------------------------------------------------------------

/// A campaign as the page shows it.
struct Row {
    campaign: Campaign,
    previous: Option<f64>,
    system: String,
    constellation: String,
    region: String,
    defender: String,
    adm: Option<f64>,
}

fn campaigns_page(_viewer: &Viewer) -> Result<Page, PageError> {
    let stored = storage::query(
        "SELECT c.campaign_id, c.event_type, c.system_id, c.constellation_id, c.defender_id, \
             c.start_time, c.defender_score, c.previous_score, \
             coalesce(s.name, ''), coalesce(k.name, ''), coalesce(r.name, ''), \
             coalesce(d.name, ''), a.adm \
         FROM campaigns c \
         LEFT JOIN names s ON s.id = c.system_id \
         LEFT JOIN names k ON k.id = c.constellation_id \
         LEFT JOIN constellations g ON g.constellation_id = c.constellation_id \
         LEFT JOIN names r ON r.id = g.region_id \
         LEFT JOIN names d ON d.id = c.defender_id \
         LEFT JOIN adm a ON a.system_id = c.system_id \
         ORDER BY c.start_time, c.campaign_id LIMIT 500",
        &[],
    )
    .map_err(|e| failed("reading campaigns", e))?;
    let rows: Vec<Row> = stored
        .rows
        .iter()
        .filter_map(|r| {
            let id = int(r, 0);
            let campaign = Campaign {
                id,
                event_type: text(r, 1),
                system: int(r, 2),
                constellation: int(r, 3),
                defender: opt_int(r, 4),
                start: when(r, 5)?,
                score: float(r, 6),
            };
            Some(Row {
                campaign,
                previous: float(r, 7),
                system: text(r, 8),
                constellation: text(r, 9),
                region: text(r, 10),
                defender: text(r, 11),
                adm: float(r, 12),
            })
        })
        .collect();
    let now = Utc::now();
    let of = |wanted: Option<Status>| -> Vec<&Row> {
        rows.iter()
            .filter(|r| wanted.is_none_or(|w| status(r.campaign.start, now) == w))
            .collect()
    };
    let (all, upcoming, active) = (
        of(None),
        of(Some(Status::Upcoming)),
        of(Some(Status::Active)),
    );
    let stats = vec![
        Stat::new("Total", i64::try_from(all.len()).unwrap_or(0)),
        Stat::new(
            "Upcoming (< 4 hrs)",
            i64::try_from(upcoming.len()).unwrap_or(0),
        ),
        Stat::new("Active", i64::try_from(active.len()).unwrap_or(0)),
    ];
    let empty = if stored.rows.is_empty() {
        "No campaigns: none are running, or the first sync hasn't finished."
    } else {
        "None of these right now."
    };
    Ok(Page::new("Sovereignty Timer")
        .description("Sovereignty campaigns, updated every 30 seconds")
        .stats(stats)
        .tab("All", vec![Section::Table(table(&all, now, empty))])
        .tab(
            "Upcoming (< 4 hrs)",
            vec![Section::Table(table(&upcoming, now, empty))],
        )
        .tab("Active", vec![Section::Table(table(&active, now, empty))])
        .text(
            "Progress is the defenders' score once a campaign is active, and which side gained \
             since 30 seconds before.",
        )
        .refresh(REFRESH))
}

fn table(rows: &[&Row], now: DateTime<Utc>, empty: &str) -> Table {
    let mut table = Table::new(vec![
        Column::text("System"),
        Column::text("Constellation"),
        Column::text("Region"),
        Column::text("Owner / Defender"),
        Column::text("Type"),
        Column::numeric("ADM"),
        Column::numeric("Start"),
        Column::numeric("Remaining"),
        Column::text("Progress"),
    ])
    .title("Sovereignty campaigns")
    .empty(empty);
    for row in rows {
        let c = &row.campaign;
        // A name not read yet: in words, never an id (the host names an
        // alliance it knows).
        let place = |name: &str, unknown: &str| {
            if name.is_empty() {
                unknown.to_owned()
            } else {
                name.to_owned()
            }
        };
        let defender: Value = match c.defender {
            Some(id) => alliance(id, row.defender.clone()).into(),
            None => "".into(),
        };
        let active = status(c.start, now) == Status::Active;
        // The defenders' score and which way it's going, once the campaign
        // is on; before, there's no score to show.
        let progress: Value = match (active, c.score) {
            (true, Some(score)) => match row.previous {
                Some(before) if score > before => badge(
                    format!("{} · defenders gaining", percent(score)),
                    Tone::Success,
                )
                .into(),
                Some(before) if score < before => badge(
                    format!("{} · attackers gaining", percent(score)),
                    Tone::Danger,
                )
                .into(),
                _ => format!("{} · no change", percent(score)).into(),
            },
            _ => "".into(),
        };
        table = table.row(vec![
            place(&row.system, "Unknown system").into(),
            place(&row.constellation, "Unknown constellation").into(),
            row.region.clone().into(),
            defender,
            event_name(&c.event_type).into(),
            row.adm
                .map_or_else(|| "".into(), |adm| format!("{adm:.1}").into()),
            time(rfc3339(c.start)),
            // Started: under way, not a countdown that reads "done".
            if active {
                badge("Active", Tone::Warning).into()
            } else {
                countdown(rfc3339(c.start))
            },
            progress,
        ]);
    }
    table
}

#[cfg(test)]
mod tests {
    use super::*;

    const CAMPAIGNS: &str = r#"[
        {"campaign_id": 7, "event_type": "ihub_defense", "solar_system_id": 30000474,
         "constellation_id": 20000069, "structure_id": 1, "defender_id": 99000001,
         "defender_score": 0.6, "attackers_score": 0.4, "start_time": "2026-09-27T12:00:00Z"},
        {"campaign_id": 8, "event_type": "something_new", "solar_system_id": 30000475,
         "constellation_id": 20000069, "structure_id": 2, "start_time": "2026-09-28T12:00:00Z"},
        {"campaign_id": "bad"}
    ]"#;

    #[test]
    fn campaigns_are_read_loosely() {
        let campaigns = parse_campaigns(CAMPAIGNS);
        assert_eq!(campaigns.len(), 2);
        assert_eq!(campaigns[0].defender, Some(99000001));
        assert_eq!(campaigns[0].score, Some(0.6));
        assert_eq!(campaigns[1].defender, None);
        assert_eq!(event_name(&campaigns[0].event_type), "IHub defense");
        assert_eq!(event_name(&campaigns[1].event_type), "something new");
        assert!(parse_campaigns("not json").is_empty());
    }

    #[test]
    fn campaigns_are_active_upcoming_or_later() {
        let now = DateTime::parse_from_rfc3339("2026-09-27T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(status(now - Duration::minutes(1), now), Status::Active);
        assert_eq!(status(now + Duration::hours(3), now), Status::Upcoming);
        assert_eq!(status(now + Duration::hours(5), now), Status::Later);
        assert_eq!(percent(0.604), "60%");
    }
}
