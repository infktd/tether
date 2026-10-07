//! A skill set from a fitting in EFT format, as aa-memberaudit's "Create
//! skill set from fitting" (`views/admin.py`, `managers/general.py`
//! `update_or_create_from_fitting`): the skills its ship and every item
//! in it require (modules and their charges, drones, fighters, implants,
//! boosters and cargo), each at the highest level any of them needs, all
//! as required levels.
//!
//! Item names come from Tether's static data; the skills a type requires
//! (its dogma attributes) from ESI's public `universe-type`, read once
//! per type and kept.

use std::collections::BTreeMap;

use tether_plugin_sdk::esi::{self, Error as EsiError, Subject};
use tether_plugin_sdk::storage::{self, Statement, Value as Db};
use tether_plugin_sdk::{PageError, Submission, SubmitResult, log};

use crate::access::Access;
use crate::{failed, int, query, text};

/// Public endpoints read no one's data.
const PUBLIC: Subject = Subject::Character(0);
/// Item lines one fitting may have.
const MAX_LINES: usize = 300;
/// Types looked up on ESI in one go, within a form's 100 calls (the rest
/// are read on the next try, what was read kept).
const MAX_LOOKUPS: usize = 75;
/// The dogma attributes of a type's required skills and their levels
/// (aa-memberaudit's `EveDogmaAttributeId`): requiredSkill1 to 6.
const SKILL_ATTRIBUTES: [(i64, i64); 6] = [
    (182, 277),
    (183, 278),
    (184, 279),
    (1285, 1286),
    (1289, 1287),
    (1290, 1288),
];

/// A fitting as EFT writes it: its ship, its name and every item named.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Fitting {
    pub ship: String,
    pub name: String,
    pub items: Vec<String>,
}

/// `Name [1]`: a mutated module, as Pyfa writes it.
fn strip_mutation(text: &str) -> &str {
    if let Some(open) = text.rfind(" [")
        && let Some(inner) = text[open + 2..].strip_suffix(']')
        && !inner.is_empty()
        && inner.bytes().all(|b| b.is_ascii_digit())
    {
        return text[..open].trim_end();
    }
    text
}

/// Reads EFT text: `[Ship, Fitting name]`, then a line per module (`Module,
/// Charge`, maybe `/OFFLINE`), drones and cargo with `x5`, and `[Empty Low
/// slot]` placeholders.
pub(crate) fn read(text: &str) -> Result<Fitting, String> {
    let not_eft = || "This fitting doesn't look like EFT: it starts with [Ship, Name].".to_owned();
    let mut lines = text
        .trim_start_matches('\u{feff}')
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty());
    let title = lines.next().ok_or_else(not_eft)?;
    let (ship, name) = title
        .strip_prefix('[')
        .and_then(|t| t.strip_suffix(']'))
        .and_then(|t| t.split_once(','))
        .ok_or_else(not_eft)?;
    let ship = ship.trim().to_owned();
    if ship.is_empty() {
        return Err(not_eft());
    }
    let name = match name.trim() {
        "" => ship.clone(),
        name => name.to_owned(),
    };
    let mut items = Vec::new();
    for line in lines {
        // `[Empty Low slot]` and the like.
        if line.starts_with('[') && line.ends_with(']') {
            continue;
        }
        let mut line = line;
        if line.to_ascii_lowercase().ends_with("/offline") {
            line = line[..line.len() - "/offline".len()].trim_end();
        }
        line = strip_mutation(line);
        if let Some((rest, last)) = line.rsplit_once(' ')
            && let Some(digits) = last.strip_prefix('x').or_else(|| last.strip_prefix('X'))
            && !digits.is_empty()
            && digits.bytes().all(|b| b.is_ascii_digit())
        {
            line = rest.trim_end();
        }
        for part in line.split(',').map(str::trim).filter(|p| !p.is_empty()) {
            items.push(crate::clip(part, 100));
        }
        if items.len() > MAX_LINES {
            return Err(format!("A fitting has at most {MAX_LINES} items."));
        }
    }
    Ok(Fitting { ship, name, items })
}

/// A type's required skills, from its dogma attributes: each skill with
/// its level, where both are given (aa-memberaudit's
/// `_create_skills_from_attributes`).
pub(crate) fn required_skills(item: &serde_json::Value) -> Vec<(i64, i64)> {
    let attribute = |id: i64| -> Option<i64> {
        item["dogma_attributes"]
            .as_array()?
            .iter()
            .find(|a| a["attribute_id"].as_i64() == Some(id))?["value"]
            .as_f64()
            // Whole numbers kept as doubles.
            .map(|v| v.round() as i64)
    };
    SKILL_ATTRIBUTES
        .iter()
        .filter_map(|(skill, level)| {
            let skill = attribute(*skill).filter(|s| *s > 0)?;
            let level = attribute(*level)?;
            Some((skill, level.clamp(1, 5)))
        })
        .collect()
}

/// Each skill the types require, at the highest level any needs
/// (aa-memberaudit's `compress_skills`): kept ones, and the rest from ESI.
fn skills_of(types: &[i64]) -> Result<BTreeMap<i64, i64>, String> {
    let kept = storage::query(
        "SELECT type_id, skills::text FROM type_skills \
         WHERE type_id = ANY(string_to_array($1, ',')::bigint[])",
        &[crate::id_list(types).into()],
    )
    .map_err(|e| format!("reading item skills: {e:?}"))?;
    let mut by_type: BTreeMap<i64, Vec<(i64, i64)>> = kept
        .rows
        .iter()
        .map(|r| {
            let skills: Vec<(i64, i64)> = serde_json::from_str(&text(r, 1)).unwrap_or_default();
            (int(r, 0), skills)
        })
        .collect();
    let missing: Vec<i64> = types
        .iter()
        .copied()
        .filter(|t| !by_type.contains_key(t))
        .collect();
    let unreachable = "EVE's servers couldn't be reached to read the skills one or more items \
                       need. Please try again shortly.";
    let mut read = Vec::new();
    let mut outcome = Ok(());
    for (n, id) in missing.iter().enumerate() {
        if n >= MAX_LOOKUPS {
            outcome = Err(
                "This fitting has more items than one go reads: send it again to finish \
                 (what was read is kept)."
                    .to_owned(),
            );
            break;
        }
        let skills = match esi::get(
            "universe-type",
            PUBLIC,
            &[("type_id".to_owned(), id.to_string())],
            None,
        ) {
            Ok(answer) => match serde_json::from_str::<serde_json::Value>(&answer.body) {
                Ok(item) => required_skills(&item),
                Err(_) => {
                    outcome = Err(unreachable.to_owned());
                    break;
                }
            },
            // ESI doesn't know it: it requires nothing it can say.
            Err(EsiError::Status(404)) => Vec::new(),
            Err(err) => {
                log::warn(format!(
                    "reading type {id} for a skill set: {}",
                    esi::describe(&err)
                ));
                outcome = Err(unreachable.to_owned());
                break;
            }
        };
        read.push(serde_json::json!({ "type_id": id, "skills": skills }));
        by_type.insert(*id, skills);
    }
    if !read.is_empty() {
        storage::execute(
            "INSERT INTO type_skills (type_id, skills) \
             SELECT type_id, skills FROM json_to_recordset($1::json) AS x(type_id bigint, skills jsonb) \
             ON CONFLICT (type_id) DO UPDATE SET skills = EXCLUDED.skills, read_at = now()",
            &[Db::json(serde_json::Value::Array(read).to_string())],
        )
        .map_err(|e| format!("keeping item skills: {e:?}"))?;
    }
    outcome?;
    let mut skills: BTreeMap<i64, i64> = BTreeMap::new();
    for (skill, level) in by_type.values().flatten() {
        let best = skills.entry(*skill).or_insert(*level);
        *best = (*best).max(*level);
    }
    Ok(skills)
}

/// What became of an import, for the page to say.
struct Imported {
    set: i64,
    name: String,
    created: bool,
    /// Lines naming nothing EVE knows, left out.
    unknown: Vec<String>,
}

/// Makes or replaces a skill set from the form's fitting.
fn import(access: &Access, submission: &Submission) -> Result<Result<Imported, String>, PageError> {
    let viewer = access.viewer;
    let fitting = match read(submission.value("fitting")) {
        Ok(fitting) => fitting,
        Err(why) => return Ok(Err(why)),
    };
    let mut names: Vec<&str> = vec![fitting.ship.as_str()];
    names.extend(fitting.items.iter().map(String::as_str));
    let found = match crate::types::by_names(&names) {
        Ok(found) => found,
        Err(why) => return Ok(Err(why)),
    };
    let Some(ship) = found
        .get(&fitting.ship.to_lowercase())
        .filter(|t| t.category_id == crate::types::SHIPS)
    else {
        return Ok(Err(format!("\"{}\" isn't a ship.", fitting.ship)));
    };
    let mut unknown: Vec<String> = fitting
        .items
        .iter()
        .filter(|n| !found.contains_key(&n.to_lowercase()))
        .cloned()
        .collect();
    unknown.sort();
    unknown.dedup();
    let mut types: Vec<i64> = found.values().map(|t| t.id).collect();
    types.sort_unstable();
    types.dedup();
    let skills = match skills_of(&types) {
        Ok(skills) => skills,
        Err(why) => return Ok(Err(why)),
    };
    if skills.is_empty() {
        return Ok(Err("Nothing in this fitting needs a skill.".to_owned()));
    }
    let skill_types: Vec<i64> = skills.keys().copied().collect();
    let named = match crate::types::by_ids(&skill_types) {
        Ok(named) => named,
        Err(why) => return Ok(Err(why)),
    };
    let mut remembered: Vec<&crate::types::Type> = named.values().collect();
    remembered.push(ship);
    if let Err(why) = crate::types::remember(remembered) {
        return Ok(Err(why));
    }
    let name = match submission.value("name").trim() {
        "" => crate::clip(&fitting.name, 100),
        name => name.to_owned(),
    };
    let existing = query(
        "SELECT id FROM skill_sets WHERE name = $1",
        &[name.clone().into()],
    )?;
    let existing = existing.first().map(|r| int(r, 0));
    if existing.is_some() && !submission.checked("overwrite") {
        return Ok(Err(format!(
            "A skill set named \"{name}\" already exists: tick overwrite to replace it."
        )));
    }
    let group = match submission.value("group") {
        "" => None,
        g => Some(g.parse::<i64>().map_err(|_| PageError::Forbidden)?),
    };
    // aa-memberaudit's description of a set made from a fitting.
    let description = format!(
        "Generated from EFT fitting '{}' by {} at {}",
        crate::clip(&fitting.name, 200),
        viewer.main.name,
        chrono::Utc::now().format("%Y-%m-%d %H:%M")
    );
    let rows: Vec<serde_json::Value> = skills
        .iter()
        .map(|(skill, level)| serde_json::json!({ "skill_id": skill, "required": level }))
        .collect();
    let rows = Db::json(serde_json::Value::Array(rows).to_string());
    let set = match existing {
        Some(id) => {
            storage::transaction(&[
                Statement::new(
                    "UPDATE skill_sets SET description = $2, ship_type_id = $3, modified_at = now(), \
                     modified_by = $4 WHERE id = $1",
                    vec![
                        id.into(),
                        description.into(),
                        ship.id.into(),
                        viewer.main.name.clone().into(),
                    ],
                ),
                Statement::new(
                    "DELETE FROM skill_set_skills WHERE set_id = $1",
                    vec![id.into()],
                ),
                Statement::new(
                    "INSERT INTO skill_set_skills (set_id, skill_id, required_level) \
                     SELECT $1, x.skill_id, x.required \
                     FROM json_to_recordset($2::json) AS x(skill_id bigint, required int)",
                    vec![id.into(), rows],
                ),
            ])
            .map_err(|e| failed("saving the skill set", e))?;
            id
        }
        None => {
            let added = storage::query(
                "WITH s AS (INSERT INTO skill_sets (name, description, ship_type_id, modified_at, modified_by) \
                            VALUES ($1, $3, $4, now(), $5) ON CONFLICT (name) DO NOTHING RETURNING id), \
                      k AS (INSERT INTO skill_set_skills (set_id, skill_id, required_level) \
                            SELECT s.id, x.skill_id, x.required FROM s, \
                                   json_to_recordset($2::json) AS x(skill_id bigint, required int) \
                            RETURNING set_id) \
                 SELECT id FROM s",
                &[
                    name.clone().into(),
                    rows,
                    description.into(),
                    ship.id.into(),
                    viewer.main.name.clone().into(),
                ],
            )
            .map_err(|e| failed("saving the skill set", e))?;
            match added.rows.first() {
                Some(r) => int(r, 0),
                None => return Ok(Err(format!("A skill set named \"{name}\" already exists."))),
            }
        }
    };
    if let Some(group) = group {
        storage::execute(
            "INSERT INTO skill_set_group_sets (group_id, set_id) \
             SELECT id, $2 FROM skill_set_groups WHERE id = $1 ON CONFLICT DO NOTHING",
            &[group.into(), set.into()],
        )
        .map_err(|e| failed("adding the set to its group", e))?;
    }
    log::info(format!(
        "skill set {name:?} ({set}) {} from a fitting by {} ({})",
        if existing.is_some() {
            "replaced"
        } else {
            "made"
        },
        viewer.main.name,
        viewer.main.id
    ));
    Ok(Ok(Imported {
        set,
        name,
        created: existing.is_none(),
        unknown,
    }))
}

/// `import_fitting`, on Skill sets for `manage`.
pub(crate) fn import_fitting(
    access: &Access,
    submission: &Submission,
) -> Result<SubmitResult, PageError> {
    match import(access, submission)? {
        Err(why) => Ok(SubmitResult::Page(crate::sets::skill_sets_page(
            access,
            Some(&why),
        )?)),
        Ok(done) if done.unknown.is_empty() => Ok(SubmitResult::Redirect(format!(
            "skill-sets/set/{}",
            done.set
        ))),
        Ok(done) => {
            let note = format!(
                "Skill set {} {}. Left out, as EVE has no item by that name: {}.",
                done.name,
                if done.created { "made" } else { "replaced" },
                crate::clip(&done.unknown.join(", "), 500)
            );
            Ok(SubmitResult::Page(crate::sets::set_page(
                access,
                &done.set.to_string(),
                Some(&note),
            )?))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eft_names_every_item() {
        let fit = read(
            "\u{feff}[Rifter, Fast Tackle]\r\n\
             Damage Control II\n\
             [Empty Low slot]\n\
             \n\
             5MN Microwarpdrive II\n\
             Warp Scrambler II /OFFLINE\n\
             \n\
             200mm AutoCannon II, Republic Fleet EMP S\n\
             Small Polycarbon Engine Housing I [1]\n\
             \n\
             Warrior II x3\n\
             Nanite Repair Paste x50\n",
        )
        .unwrap();
        assert_eq!(fit.ship, "Rifter");
        assert_eq!(fit.name, "Fast Tackle");
        assert_eq!(
            fit.items,
            vec![
                "Damage Control II",
                "5MN Microwarpdrive II",
                "Warp Scrambler II",
                "200mm AutoCannon II",
                "Republic Fleet EMP S",
                "Small Polycarbon Engine Housing I",
                "Warrior II",
                "Nanite Repair Paste",
            ]
        );
        assert_eq!(read("[Rifter,]").unwrap().name, "Rifter");
        for bad in ["", "Rifter", "[Rifter]", "[, Name]"] {
            assert!(read(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn required_skills_need_a_skill_and_its_level() {
        let item = serde_json::json!({ "dogma_attributes": [
            { "attribute_id": 182, "value": 3300.0 },
            { "attribute_id": 277, "value": 4.0 },
            { "attribute_id": 1285, "value": 3301.0 },
            { "attribute_id": 1286, "value": 2.0 },
            // A skill without its level is left out, as aa-memberaudit's.
            { "attribute_id": 183, "value": 3302.0 },
        ]});
        assert_eq!(required_skills(&item), vec![(3300, 4), (3301, 2)]);
        assert!(required_skills(&serde_json::json!({})).is_empty());
    }
}
