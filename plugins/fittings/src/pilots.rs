//! The pilot's own side of a fit (allianceauth-fittings' skill check and
//! Save to EVE): whether each of their characters can fly it, from the
//! skills this app reads with its own `esi-skills.read_skills.v1`, and the
//! fit saved to one of their characters in EVE.
//!
//! Only a pilot's own characters registered for Fittings are shown, to that
//! pilot alone: nobody else, managers included, sees anyone's skills.

use tether_plugin_sdk::esi::{self, Error as EsiError, Subject};
use tether_plugin_sdk::identity::{Character, Viewer};
use tether_plugin_sdk::jobs::{self, JobError, NewJob};
use tether_plugin_sdk::storage::{self, Value as Db};
use tether_plugin_sdk::{
    Column, PageError, Table, Tone, Value, action, actions, badge, character, time,
};

use crate::{int, query, text};

/// The schedule (and job) reading the registered characters' skills.
pub(crate) const SKILLS: &str = "skills";
/// Characters read per run: each is one ESI call of the 100 a run may make.
const PER_RUN: usize = 90;

/// The viewer's own characters registered for Fittings, the one they act
/// as (Change character) first.
pub(crate) fn mine(viewer: &Viewer) -> Vec<Character> {
    let acting = tether_plugin_sdk::identity::acting().map_or(viewer.main.id, |c| c.id);
    let mut mine: Vec<Character> = esi::characters()
        .into_iter()
        .filter(|c| viewer.characters.iter().any(|v| v.id == c.id))
        .collect();
    mine.sort_by_key(|c| c.id != acting);
    mine
}

fn retry(what: &str, err: impl std::fmt::Debug) -> JobError {
    JobError::Retry(format!("{what}: {err:?}"))
}

/// Reads the skills of every registered character, oldest read first, and
/// forgets characters no longer registered. More than a run can read
/// queue another run.
pub(crate) fn sync() -> Result<(), JobError> {
    let registered = esi::characters();
    let ids: Vec<i64> = registered.iter().map(|c| c.id).collect();
    let ids_param = Db::json(serde_json::json!(ids).to_string());
    storage::transaction(&[
        storage::Statement::new(
            "DELETE FROM character_skills WHERE character_id <> ALL(\
             ARRAY(SELECT jsonb_array_elements_text($1::jsonb)::bigint))",
            vec![ids_param.clone()],
        ),
        storage::Statement::new(
            "DELETE FROM skill_reads WHERE character_id <> ALL(\
             ARRAY(SELECT jsonb_array_elements_text($1::jsonb)::bigint))",
            vec![ids_param],
        ),
    ])
    .map_err(|e| retry("forgetting characters", e))?;
    let read_at: Vec<(i64, Option<String>)> =
        query("SELECT character_id, read_at FROM skill_reads", &[])
            .map_err(|e| retry("reading when skills were read", e))?
            .iter()
            .map(|r| (int(r, 0), Some(text(r, 1)).filter(|t| !t.is_empty())))
            .collect();
    let mut order = registered;
    // Never read first, then the oldest read.
    order.sort_by_key(|c| {
        read_at
            .iter()
            .find(|(id, _)| *id == c.id)
            .and_then(|(_, at)| at.clone())
            .unwrap_or_default()
    });
    let more = order.len() > PER_RUN;
    for c in order.iter().take(PER_RUN) {
        read(c).map_err(|e| retry("reading skills", e))?;
    }
    if more {
        jobs::enqueue(NewJob::new(SKILLS).key("skills_more"))
            .map_err(|e| retry("queueing the rest", e))?;
    }
    Ok(())
}

/// Reads one character's skills (ESI's active levels: what an Alpha clone
/// may use) and records when, or why not.
pub(crate) fn read(c: &Character) -> Result<(), PageError> {
    let answer = esi::get("character-skills", Subject::Character(c.id), &[], None);
    let problem = match answer {
        Ok(answer) => {
            let body: serde_json::Value = serde_json::from_str(&answer.body)
                .map_err(|e| crate::failed("reading skills", e))?;
            let skills: Vec<serde_json::Value> = body["skills"]
                .as_array()
                .map(|all| {
                    all.iter()
                        .filter_map(|s| {
                            Some(serde_json::json!({
                                "skill_id": s["skill_id"].as_i64()?,
                                "level": s["active_skill_level"].as_i64()?,
                            }))
                        })
                        .collect()
                })
                .unwrap_or_default();
            storage::transaction(&[
                storage::Statement::new(
                    "DELETE FROM character_skills WHERE character_id = $1",
                    vec![c.id.into()],
                ),
                storage::Statement::new(
                    "INSERT INTO character_skills (character_id, skill_id, level) \
                     SELECT $1, x.skill_id, x.level \
                     FROM jsonb_to_recordset($2::jsonb) AS x(skill_id bigint, level integer)",
                    vec![
                        c.id.into(),
                        Db::json(serde_json::Value::Array(skills).to_string()),
                    ],
                ),
            ])
            .map_err(|e| crate::failed("storing skills", e))?;
            None
        }
        Err(EsiError::Token | EsiError::NotRegistered) => {
            Some("EVE access ended: register it again")
        }
        Err(_) => Some("EVE didn't answer; tried again later"),
    };
    storage::execute(
        "INSERT INTO skill_reads (character_id, name, read_at, problem) \
         VALUES ($1, $2, CASE WHEN $3::text IS NULL THEN now() END, $3) \
         ON CONFLICT (character_id) DO UPDATE SET name = EXCLUDED.name, \
         read_at = coalesce(EXCLUDED.read_at, skill_reads.read_at), problem = EXCLUDED.problem",
        &[
            c.id.into(),
            c.name.clone().into(),
            problem.map(str::to_owned).into(),
        ],
    )
    .map_err(|e| crate::failed("recording a skills read", e))?;
    Ok(())
}

/// What one character lacks of `needed` (skill, name, level): each skill
/// with the level it has (0 when untrained).
fn missing(character: i64, needed: &[(i64, String, i64)]) -> Result<Vec<String>, PageError> {
    let has: Vec<(i64, i64)> = query(
        "SELECT skill_id, level FROM character_skills WHERE character_id = $1",
        &[character.into()],
    )?
    .iter()
    .map(|r| (int(r, 0), int(r, 1)))
    .collect();
    Ok(needed
        .iter()
        .filter_map(|(skill, name, level)| {
            let trained = has.iter().find(|(s, _)| s == skill).map_or(0, |(_, l)| *l);
            (trained < *level).then(|| {
                let name = if name.is_empty() {
                    format!("Skill {skill}")
                } else {
                    name.clone()
                };
                if trained == 0 {
                    format!("{name} {} (untrained)", roman(*level))
                } else {
                    format!("{name} {} (has {})", roman(*level), roman(trained))
                }
            })
        })
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

/// "Can I fly it": each of the viewer's registered characters, whether
/// its skills cover `needed`, what's missing, and Save to EVE.
pub(crate) fn table(mine: &[Character], needed: &[(i64, String, i64)]) -> Result<Table, PageError> {
    let mut table = Table::new(vec![
        Column::text("Character"),
        Column::text("Can fly it"),
        Column::text("Missing skills"),
        Column::text("Skills read"),
        Column::text(""),
    ])
    .title("Can I fly it");
    for c in mine {
        let read = query(
            "SELECT read_at, problem FROM skill_reads WHERE character_id = $1",
            &[c.id.into()],
        )?;
        let read_at = read.first().map(|r| text(r, 0)).filter(|t| !t.is_empty());
        let problem = read.first().map(|r| text(r, 1)).filter(|t| !t.is_empty());
        let (verdict, lacking): (Value, Value) = match &read_at {
            None => (badge("Not read yet", Tone::Neutral).into(), "".into()),
            Some(_) => {
                let lacking = missing(c.id, needed)?;
                if lacking.is_empty() {
                    (badge("Yes", Tone::Success).into(), "".into())
                } else {
                    (
                        badge("No", Tone::Danger).into(),
                        crate::clip(&lacking.join(", "), 1000).into(),
                    )
                }
            }
        };
        let when: Value = match (read_at, problem) {
            (_, Some(problem)) => problem.into(),
            (Some(at), None) => time(at),
            (None, None) => "Not yet: Read skills again".into(),
        };
        table = table.row(vec![
            character(c.id, c.name.clone()).into(),
            verdict,
            lacking,
            when,
            actions(vec![
                action("Save to EVE", "save_to_eve")
                    .field("character", c.id.to_string())
                    .tone(Tone::Accent),
                action("Read skills again", "read_skills").field("character", c.id.to_string()),
            ]),
        ]);
    }
    Ok(table)
}

/// Whether `id` is one of the viewer's registered characters.
pub(crate) fn own(viewer: &Viewer, id: i64) -> Result<Character, PageError> {
    mine(viewer)
        .into_iter()
        .find(|c| c.id == id)
        .ok_or(PageError::Forbidden)
}
