//! Skill Sets and Reports.
//!
//! A skill set is aa-memberaudit's: a named list of skills, each with a
//! required level, a recommended level or both, such as what a doctrine
//! ship needs; with a description, a ship (for show) and whether pilots
//! see it on their own sheets (`is_visible`). Skill set groups gather
//! sets, doctrines among them, and the sheet and the reports show sets by
//! group. A character can use a set when it has every required level.

use std::collections::BTreeMap;

use tether_plugin_sdk::identity::Viewer;
use tether_plugin_sdk::storage::{self, Statement, Value as Db};
use tether_plugin_sdk::{
    Column, Field, Form, Page, PageError, RecordPanel, Section, Submission, SubmitResult, Table,
    Tone, Value, action, badge, character, item_type, link, log,
};

use crate::access::Access;
use crate::pages::roman;
use crate::{boolean, failed, int, name_of, opt_int, query, text, with_rows};

/// Skill sets and the skills in one, so the Skill Sets page stays within
/// the host's page limits.
const MAX_SETS: i64 = 30;
const MAX_SKILLS_PER_SET: usize = 50;
/// Skill set groups, at most.
const MAX_GROUPS: i64 = 100;
/// aa-memberaudit's label for sets in no group.
pub(crate) const UNGROUPED: &str = "[Ungrouped]";

/// A skill of a set: its required level, its recommended level, or both.
#[derive(Clone)]
pub(crate) struct SetSkill {
    pub id: i64,
    pub name: String,
    pub required: Option<i64>,
    pub recommended: Option<i64>,
}

impl SetSkill {
    /// `Gunnery IV`, with the recommended level after it: `Gunnery IV [V]`.
    pub fn label(&self) -> String {
        match (self.required, self.recommended) {
            (Some(r), Some(c)) => format!("{} {} [{}]", self.name, roman(r), roman(c)),
            (Some(r), None) => format!("{} {}", self.name, roman(r)),
            (None, Some(c)) => format!("{} [{}]", self.name, roman(c)),
            (None, None) => self.name.clone(),
        }
    }
}

#[derive(Clone)]
pub(crate) struct SkillSet {
    pub id: i64,
    pub name: String,
    pub description: String,
    pub ship: Option<(i64, String)>,
    /// Shown on pilots' own sheets (aa-memberaudit's `is_visible`).
    pub visible: bool,
    pub skills: Vec<SetSkill>,
}

impl SkillSet {
    /// Its name, with its ship's picture when it has one.
    pub fn value(&self) -> Value {
        match &self.ship {
            Some((id, _)) => item_type(*id, self.name.clone()).into(),
            None => self.name.clone().into(),
        }
    }
}

/// A skill set group, as aa-memberaudit's: a doctrine, say.
pub(crate) struct SetGroup {
    pub id: i64,
    pub name: String,
    pub description: String,
    pub doctrine: bool,
    pub active: bool,
    /// Its sets, by id.
    pub sets: Vec<i64>,
}

impl SetGroup {
    /// "Doctrine: name" for a doctrine (aa-memberaudit's `name_plus`).
    pub fn label(&self) -> String {
        if self.doctrine {
            format!("Doctrine: {}", self.name)
        } else {
            self.name.clone()
        }
    }

    /// As the sheet names it: a group not in use says so.
    pub fn sheet_label(&self) -> String {
        if self.active {
            self.label()
        } else {
            format!("{} [Not active]", self.name)
        }
    }
}

/// Every skill set by name, with its skills by name.
pub(crate) fn skill_sets() -> Result<Vec<SkillSet>, PageError> {
    let sets = query(
        &format!(
            "SELECT s.id, s.name, s.description, s.ship_type_id, {ship}, s.is_visible \
             FROM skill_sets s ORDER BY s.name",
            ship = name_of("s.ship_type_id")
        ),
        &[],
    )?;
    let skills = query(
        &format!(
            "SELECT k.set_id, k.skill_id, {skill}, k.required_level, k.recommended_level \
             FROM skill_set_skills k ORDER BY 3",
            skill = name_of("k.skill_id")
        ),
        &[],
    )?;
    let mut by_set: BTreeMap<i64, Vec<SetSkill>> = BTreeMap::new();
    for r in &skills {
        by_set.entry(int(r, 0)).or_default().push(SetSkill {
            id: int(r, 1),
            name: text(r, 2),
            required: opt_int(r, 3),
            recommended: opt_int(r, 4),
        });
    }
    Ok(sets
        .iter()
        .map(|s| {
            let id = int(s, 0);
            SkillSet {
                id,
                name: text(s, 1),
                description: text(s, 2),
                ship: opt_int(s, 3).map(|ship| (ship, text(s, 4))),
                visible: boolean(s, 5),
                skills: by_set.remove(&id).unwrap_or_default(),
            }
        })
        .collect())
}

/// Every skill set group by name, with its sets.
pub(crate) fn set_groups() -> Result<Vec<SetGroup>, PageError> {
    let groups = query(
        "SELECT id, name, description, is_doctrine, is_active FROM skill_set_groups ORDER BY name",
        &[],
    )?;
    let members = query("SELECT group_id, set_id FROM skill_set_group_sets", &[])?;
    Ok(groups
        .iter()
        .map(|g| {
            let id = int(g, 0);
            SetGroup {
                id,
                name: text(g, 1),
                description: text(g, 2),
                doctrine: boolean(g, 3),
                active: boolean(g, 4),
                sets: members
                    .iter()
                    .filter(|m| int(m, 0) == id)
                    .map(|m| int(m, 1))
                    .collect(),
            }
        })
        .collect())
}

/// The sets under each group, as aa-memberaudit's `compile_groups_map`:
/// each group by name with its sets (a set in two groups is under both),
/// then the sets in none.
pub(crate) fn grouped<'a>(
    sets: &'a [SkillSet],
    groups: &'a [SetGroup],
) -> Vec<(Option<&'a SetGroup>, Vec<&'a SkillSet>)> {
    let mut out: Vec<(Option<&SetGroup>, Vec<&SkillSet>)> = groups
        .iter()
        .map(|g| {
            (
                Some(g),
                sets.iter().filter(|s| g.sets.contains(&s.id)).collect(),
            )
        })
        .filter(|(_, sets): &(Option<&SetGroup>, Vec<&SkillSet>)| !sets.is_empty())
        .collect();
    let loose: Vec<&SkillSet> = sets
        .iter()
        .filter(|s| !groups.iter().any(|g| g.sets.contains(&s.id)))
        .collect();
    if !loose.is_empty() {
        out.push((None, loose));
    }
    out
}

/// Characters listed at most (the host's rows per table).
const MAX_LISTED: usize = 500;
/// Skill sets with a tab of their own on Reports (the host's tabs per
/// page).
const MAX_TABS: usize = 10;

/// SQL over `characters c` for those with every required level of set
/// `$1`, of those `condition` picks.
fn able_where(condition: &str) -> String {
    format!(
        "{condition} AND NOT EXISTS ( \
           SELECT 1 FROM skill_set_skills k WHERE k.set_id = $1 AND k.required_level IS NOT NULL \
           AND NOT EXISTS ( \
             SELECT 1 FROM skills s WHERE s.character_id = c.character_id \
               AND s.skill_id = k.skill_id AND s.active_level >= k.required_level))"
    )
}

fn able_params(set: &SkillSet, scope_params: &[Db]) -> Vec<Db> {
    let mut params: Vec<Db> = vec![set.id.into()];
    params.extend(scope_params.iter().cloned());
    params
}

/// Characters meeting every skill of the set, of those `scope` (SQL over
/// `characters c`, with parameters from `$2`) picks: the first
/// [`MAX_LISTED`] by name.
fn able(set: &SkillSet, scope: &(String, Vec<Db>)) -> Result<Vec<(i64, String)>, PageError> {
    let (condition, scope_params) = scope;
    let sql = format!(
        "SELECT c.character_id, c.name FROM characters c WHERE {} \
         ORDER BY c.name LIMIT {MAX_LISTED}",
        able_where(condition)
    );
    Ok(query(&sql, &able_params(set, scope_params))?
        .iter()
        .map(|r| (int(r, 0), text(r, 1)))
        .collect())
}

/// How many characters [`able`] would list, all of them.
fn able_count(set: &SkillSet, scope: &(String, Vec<Db>)) -> Result<i64, PageError> {
    let (condition, scope_params) = scope;
    let sql = format!(
        "SELECT count(*) FROM characters c WHERE {}",
        able_where(condition)
    );
    Ok(query(&sql, &able_params(set, scope_params))?
        .first()
        .map_or(0, |r| int(r, 0)))
}

/// The viewer's own characters, as a scope for [`able`].
fn own(viewer: &Viewer) -> (String, Vec<Db>) {
    let ids: Vec<String> = viewer.characters.iter().map(|c| c.id.to_string()).collect();
    (
        "c.character_id = ANY(string_to_array($2, ',')::bigint[])".to_owned(),
        vec![ids.join(",").into()],
    )
}

/// One skill set on a character's sheet: what it still needs.
pub(crate) struct SheetSet {
    pub group: String,
    pub doctrine: bool,
    pub set: SkillSet,
    pub missing_required: Vec<String>,
    pub missing_recommended: Vec<String>,
}

/// The skill sets on a character's sheet (aa-memberaudit's Skill Sets
/// tab): the visible ones, by group, each with the required and
/// recommended skills it lacks.
pub(crate) fn for_character(id: i64) -> Result<Vec<SheetSet>, PageError> {
    let sets: Vec<SkillSet> = skill_sets()?.into_iter().filter(|s| s.visible).collect();
    let groups = set_groups()?;
    let trained: BTreeMap<i64, i64> = query(
        "SELECT skill_id, active_level FROM skills WHERE character_id = $1",
        &[id.into()],
    )?
    .iter()
    .map(|r| (int(r, 0), int(r, 1)))
    .collect();
    let lacking = |set: &SkillSet, level: fn(&SetSkill) -> Option<i64>| -> Vec<String> {
        set.skills
            .iter()
            .filter_map(|k| {
                let wanted = level(k)?;
                let has = trained.get(&k.id).copied().unwrap_or(0);
                (has < wanted).then(|| format!("{} {} (has {})", k.name, roman(wanted), has))
            })
            .collect()
    };
    let mut out = Vec::new();
    for (group, members) in grouped(&sets, &groups) {
        for set in members {
            out.push(SheetSet {
                group: group.map_or_else(|| UNGROUPED.to_owned(), SetGroup::sheet_label),
                doctrine: group.is_some_and(|g| g.doctrine),
                missing_required: lacking(set, |k| k.required),
                missing_recommended: lacking(set, |k| k.recommended),
                set: set.clone(),
            });
        }
    }
    out.sort_by(|a, b| {
        (a.group.to_lowercase(), a.set.name.to_lowercase())
            .cmp(&(b.group.to_lowercase(), b.set.name.to_lowercase()))
    });
    Ok(out)
}

/// A skill set's details beside a character's Skill sets tab
/// (aa-memberaudit's): its description and ship, and each skill against
/// the character's level.
pub(crate) fn sheet_panel(character: i64, chosen: &SheetSet) -> Result<RecordPanel, PageError> {
    let set = &chosen.set;
    let trained: BTreeMap<i64, i64> = query(
        "SELECT skill_id, active_level FROM skills WHERE character_id = $1",
        &[character.into()],
    )?
    .iter()
    .map(|r| (int(r, 0), int(r, 1)))
    .collect();
    let mut panel =
        RecordPanel::new("set", "Skill set", set.name.clone()).context(chosen.group.clone());
    if !set.description.is_empty() {
        panel = panel.fact("Description", crate::clip(&set.description, 500));
    }
    if let Some((id, name)) = &set.ship {
        panel = panel.fact("Ship", item_type(*id, name.clone()));
    }
    panel = panel.fact(
        "Can use",
        if chosen.missing_required.is_empty() {
            badge("Has every required skill", Tone::Success)
        } else {
            badge(
                format!("{} required skills missing", chosen.missing_required.len()),
                Tone::Warning,
            )
        },
    );
    // The host's facts per panel: the table's columns name the rest.
    for skill in set.skills.iter().take(17) {
        let has = trained.get(&skill.id).copied().unwrap_or(0);
        let tone = if skill.required.is_some_and(|r| has < r) {
            Tone::Danger
        } else if skill.recommended.is_some_and(|r| has < r) {
            Tone::Warning
        } else {
            Tone::Success
        };
        let wanted = match (skill.required, skill.recommended) {
            (Some(r), Some(c)) => format!("{} [{}]", roman(r), roman(c)),
            (Some(r), None) => roman(r).to_owned(),
            (None, Some(c)) => format!("[{}]", roman(c)),
            (None, None) => String::new(),
        };
        let has = if has > 0 { roman(has) } else { "none" };
        panel = panel.fact(
            skill.name.clone(),
            badge(format!("{wanted}, has {has}"), tone),
        );
    }
    Ok(panel)
}

/// A set's skills as one line, clipped for a table cell.
fn skills_line(set: &SkillSet) -> String {
    crate::clip(
        &set.skills
            .iter()
            .map(SetSkill::label)
            .collect::<Vec<_>>()
            .join(", "),
        1500,
    )
}

/// The Skill Sets page: for `view_skill_sets` (which of your characters
/// can use each, by group), and for `manage` (to add and delete sets, and
/// to make groups).
pub(crate) fn skill_sets_page(access: &Access, note: Option<&str>) -> Result<Page, PageError> {
    let viewer = access.viewer;
    // The manifest's rule asks for view_skill_sets too.
    if !access.skill_sets {
        return Err(PageError::NotFound);
    }
    let manage = viewer.can("manage");
    let sets = skill_sets()?;
    let groups = set_groups()?;
    let mine = own(viewer);
    let mut page = Page::new("Skill sets").description(
        "Named lists of skills, such as a doctrine, and which of your characters can use them",
    );
    if let Some(note) = note {
        page = page.text(note);
    }
    let mut rows = Vec::new();
    for (group, members) in grouped(&sets, &groups) {
        for set in members {
            // Sets kept off pilots' sheets are for officers' reports.
            if !set.visible && !manage {
                continue;
            }
            let able = able(set, &mine)?;
            let mut name = set.value();
            if !set.visible {
                name = format!("{} (hidden from pilots)", set.name).into();
            }
            rows.push(vec![
                group
                    .map_or_else(|| UNGROUPED.to_owned(), SetGroup::label)
                    .into(),
                name,
                skills_line(set).into(),
                if able.is_empty() {
                    badge("None of yours", Tone::Neutral).into()
                } else {
                    able.iter()
                        .map(|(_, n)| n.clone())
                        .collect::<Vec<_>>()
                        .join(", ")
                        .into()
                },
            ]);
        }
    }
    page = page.table(with_rows(
        Table::new(vec![
            Column::text("Group"),
            Column::text("Skill set"),
            Column::text("Skills (required, [recommended])"),
            Column::text("Your characters who can"),
        ])
        .empty("No skill sets yet."),
        rows,
    ));
    if !manage {
        return Ok(page);
    }
    page = page.table(with_rows(
        Table::new(vec![
            Column::text("Skill set group"),
            Column::text("Doctrine"),
            Column::text("In use"),
            Column::text("Skill sets"),
        ])
        .title("Skill set groups")
        .empty("No skill set groups yet."),
        groups.iter().map(|g| {
            vec![
                link(g.name.clone(), format!("skill-sets/group/{}", g.id)).into(),
                yes_no(g.doctrine),
                yes_no(g.active),
                crate::clip(
                    &sets
                        .iter()
                        .filter(|s| g.sets.contains(&s.id))
                        .map(|s| s.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", "),
                    1500,
                )
                .into(),
            ]
        }),
    ));
    page = page.form(
        Form::new("add_set", "Add skill set")
            .title("New skill set")
            .description(
                "One skill per line with the level it requires, as `Caldari Battleship 4`; a \
                 level it recommends goes in brackets after it (`Caldari Battleship 4 [5]`), or \
                 alone (`Caldari Battleship [5]`). Only skills some member has trained are known. \
                 Secure Groups with a skill set filter follow its changes: changing or deleting \
                 a set changes who is in them.",
            )
            .field(Field::text("name", "Name", 100).required())
            .field(Field::textarea("description", "Description", 2000))
            .field(
                Field::text("ship", "Ship", 100)
                    .help("A ship's name, for its picture beside the set. Optional."),
            )
            .field(Field::checkbox(
                "visible",
                "Show it on pilots' own sheets",
                true,
            ))
            .field(Field::textarea("skills", "Skills", 5000).required()),
    );
    page = page.form(group_form(None, &sets));
    if !sets.is_empty() {
        let choices: Vec<(String, String)> = sets
            .iter()
            .take(100)
            .map(|set| (set.id.to_string(), set.name.clone()))
            .collect();
        page = page.form(
            Form::new("delete_set", "Delete skill set")
                .field(Field::select("set", "Skill set", choices).required())
                .field(Field::checkbox("confirm", "Yes, delete it", false).required()),
        );
    }
    Ok(page)
}

fn yes_no(yes: bool) -> Value {
    if yes {
        badge("Yes", Tone::Success).into()
    } else {
        badge("No", Tone::Neutral).into()
    }
}

/// The form adding a group, or changing one.
fn group_form(group: Option<&SetGroup>, sets: &[SkillSet]) -> Form {
    let members = group.map_or_else(String::new, |g| {
        sets.iter()
            .filter(|s| g.sets.contains(&s.id))
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    });
    let (id, label, title) = match group {
        Some(_) => ("save_group", "Save", "Change skill set group"),
        None => ("add_group", "Add group", "New skill set group"),
    };
    let field = |f: Field, value: Option<String>| match value {
        Some(v) => f.value(v),
        None => f,
    };
    Form::new(id, label)
        .title(title)
        .description(
            "Skill sets go together in groups, such as a doctrine's ships: the sheet and the \
             reports show them by group, and a doctrine's sets say so.",
        )
        .field(field(
            Field::text("name", "Name", 100).required(),
            group.map(|g| g.name.clone()),
        ))
        .field(field(
            Field::textarea("description", "Description", 2000),
            group.map(|g| g.description.clone()),
        ))
        .field(Field::checkbox(
            "doctrine",
            "A doctrine",
            group.is_some_and(|g| g.doctrine),
        ))
        .field(Field::checkbox(
            "active",
            "In use",
            group.is_none_or(|g| g.active),
        ))
        .field(
            Field::textarea("sets", "Skill sets", 10_000)
                .help("One skill set's name per line.")
                .value(members),
        )
}

/// A skill set group's own page, for `manage`: change or delete it.
pub(crate) fn group_page(access: &Access, id: &str, note: Option<&str>) -> Result<Page, PageError> {
    if !access.skill_sets || !access.viewer.can("manage") {
        return Err(PageError::NotFound);
    }
    let id: i64 = id.parse().map_err(|_| PageError::NotFound)?;
    let groups = set_groups()?;
    let group = groups
        .iter()
        .find(|g| g.id == id)
        .ok_or(PageError::NotFound)?;
    let sets = skill_sets()?;
    let mut page = Page::new(group.label())
        .description("A skill set group")
        .link("Skill sets", "skill-sets");
    if let Some(note) = note {
        page = page.text(note);
    }
    Ok(page
        .form(group_form(Some(group), &sets))
        .table(Table::new(vec![Column::text("")]).row(vec![
            action("Delete group", "delete_group")
                .field("group", id.to_string())
                .tone(Tone::Danger)
                .confirm("The group goes; its skill sets stay.")
                .into(),
        ])))
}

/// A skill's level: `4` or `IV`.
fn level(word: &str) -> Option<i64> {
    let level = match word.trim() {
        "I" | "i" => 1,
        "II" | "ii" => 2,
        "III" | "iii" => 3,
        "IV" | "iv" => 4,
        "V" | "v" => 5,
        n => n.parse().ok()?,
    };
    (1..=5).contains(&level).then_some(level)
}

/// One line of a set's skills: the skill's name, its required level, and
/// a recommended level in brackets (`Gunnery 4 [5]`, `Gunnery [5]`).
pub(crate) fn parse_line(line: &str) -> Result<(String, Option<i64>, Option<i64>), String> {
    let line: String = line.trim().chars().take(100).collect();
    let mut rest = line.as_str();
    let mut recommended = None;
    if let Some(open) = rest.rfind('[')
        && rest.ends_with(']')
    {
        recommended = Some(
            level(&rest[open + 1..rest.len() - 1])
                .ok_or_else(|| format!("\"{line}\": a level is 1 to 5"))?,
        );
        rest = rest[..open].trim_end();
    }
    let mut required = None;
    if let Some((name, word)) = rest.rsplit_once(' ')
        && let Some(l) = level(word)
    {
        required = Some(l);
        rest = name.trim_end();
    }
    if required.is_none() && recommended.is_none() {
        return Err(format!("\"{line}\" needs a level after the skill"));
    }
    if rest.is_empty() {
        return Err(format!("\"{line}\" needs a skill before its level"));
    }
    Ok((rest.to_owned(), required, recommended))
}

/// A skill's id with its required and recommended levels.
pub(crate) type Levels = (i64, Option<i64>, Option<i64>);

/// A set's skills, one per line, matched to known skills.
pub(crate) fn parse_skills(text: &str) -> Result<Vec<Levels>, String> {
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    if lines.len() > MAX_SKILLS_PER_SET {
        return Err(format!(
            "A skill set has at most {MAX_SKILLS_PER_SET} skills."
        ));
    }
    let mut out: Vec<Levels> = Vec::new();
    for line in lines {
        let (name, required, recommended) = parse_line(line)?;
        let found = storage::query(
            "SELECT id FROM names WHERE lower(name) = lower($1) \
             AND id IN (SELECT DISTINCT skill_id FROM skills) LIMIT 1",
            &[name.as_str().into()],
        )
        .map_err(|e| format!("reading skills: {e:?}"))?;
        let id = found
            .rows
            .first()
            .map(|r| int(r, 0))
            .ok_or_else(|| format!("\"{name}\" isn't a skill any member has trained"))?;
        // A skill named twice keeps its highest levels.
        match out.iter_mut().find(|(k, ..)| *k == id) {
            Some((_, req, rec)) => {
                *req = (*req).max(required);
                *rec = (*rec).max(recommended);
            }
            None => out.push((id, required, recommended)),
        }
    }
    if out.is_empty() {
        return Err("List at least one skill.".to_owned());
    }
    Ok(out)
}

/// The ship named, if any: one of EVE's ships.
fn ship(name: &str) -> Result<Option<crate::types::Type>, String> {
    let name = name.trim();
    if name.is_empty() {
        return Ok(None);
    }
    let found = crate::types::by_names(&[name])?;
    match found.into_values().next() {
        Some(t) if t.category_id == crate::types::SHIPS => {
            crate::types::remember([&t])?;
            Ok(Some(t))
        }
        _ => Err(format!("\"{name}\" isn't a ship")),
    }
}

pub(crate) fn add_set(access: &Access, submission: &Submission) -> Result<SubmitResult, PageError> {
    let viewer = access.viewer;
    let again = |why: &str| -> Result<SubmitResult, PageError> {
        Ok(SubmitResult::Page(skill_sets_page(access, Some(why))?))
    };
    let name = submission.value("name").trim().to_owned();
    let skills = match parse_skills(submission.value("skills")) {
        Ok(skills) => skills,
        Err(why) => return again(&why),
    };
    let ship = match ship(submission.value("ship")) {
        Ok(ship) => ship,
        Err(why) => return again(&why),
    };
    let count = storage::query("SELECT count(*) FROM skill_sets", &[])
        .map_err(|e| failed("counting skill sets", e))?;
    if count.rows.first().map_or(0, |r| int(r, 0)) >= MAX_SETS {
        return again(&format!(
            "There can be at most {MAX_SETS} skill sets: delete one first."
        ));
    }
    let rows: Vec<serde_json::Value> = skills
        .iter()
        .map(|(skill, required, recommended)| {
            serde_json::json!({ "skill_id": skill, "required": required, "recommended": recommended })
        })
        .collect();
    // The set and its skills together, or neither.
    let added = storage::query(
        "WITH s AS (INSERT INTO skill_sets (name, description, ship_type_id, is_visible, modified_at, modified_by) \
                    VALUES ($1, $3, $4, $5, now(), $6) ON CONFLICT (name) DO NOTHING RETURNING id), \
              k AS (INSERT INTO skill_set_skills (set_id, skill_id, required_level, recommended_level) \
                    SELECT s.id, x.skill_id, x.required, x.recommended FROM s, \
                           json_to_recordset($2::json) AS x(skill_id bigint, required int, recommended int) \
                    RETURNING set_id) \
         SELECT id FROM s",
        &[
            name.clone().into(),
            Db::json(serde_json::Value::Array(rows).to_string()),
            crate::clip(submission.value("description").trim(), 2000).into(),
            ship.map_or(Db::Null, |t| t.id.into()),
            submission.checked("visible").into(),
            viewer.main.name.clone().into(),
        ],
    )
    .map_err(|e| failed("saving the skill set", e))?;
    if added.rows.is_empty() {
        return again("A skill set with that name already exists.");
    }
    log::info(format!(
        "skill set {name:?} added by {} ({})",
        viewer.main.name, viewer.main.id
    ));
    Ok(SubmitResult::Redirect("skill-sets".into()))
}

pub(crate) fn delete_set(viewer: &Viewer, set: &str) -> Result<SubmitResult, PageError> {
    let id: i64 = set.parse().map_err(|_| PageError::NotFound)?;
    storage::execute("DELETE FROM skill_sets WHERE id = $1", &[id.into()])
        .map_err(|e| failed("deleting the skill set", e))?;
    log::info(format!(
        "skill set {id} deleted by {} ({})",
        viewer.main.name, viewer.main.id
    ));
    Ok(SubmitResult::Redirect("skill-sets".into()))
}

/// Adds a skill set group (`add_group`, on Skill sets) or changes one
/// (`save_group`, on its own page).
pub(crate) fn save_group(
    access: &Access,
    submission: &Submission,
) -> Result<SubmitResult, PageError> {
    let viewer = access.viewer;
    let path = submission.request.path.clone();
    let editing = path.strip_prefix("skill-sets/group/").map(str::to_owned);
    let again = |why: &str| -> Result<SubmitResult, PageError> {
        Ok(SubmitResult::Page(match &editing {
            Some(id) => group_page(access, id, Some(why))?,
            None => skill_sets_page(access, Some(why))?,
        }))
    };
    let name = submission.value("name").trim().to_owned();
    if name.is_empty() {
        return again("A group needs a name.");
    }
    let sets = skill_sets()?;
    let mut members = Vec::new();
    for line in submission
        .value("sets")
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
    {
        match sets
            .iter()
            .find(|s| s.name.to_lowercase() == line.to_lowercase())
        {
            Some(s) => members.push(s.id),
            None => return again(&format!("There's no skill set named \"{line}\".")),
        }
    }
    members.sort_unstable();
    members.dedup();
    let description = crate::clip(submission.value("description").trim(), 2000);
    let params: Vec<Db> = vec![
        name.clone().into(),
        description.into(),
        submission.checked("doctrine").into(),
        submission.checked("active").into(),
        viewer.main.name.clone().into(),
        crate::id_list(&members).into(),
    ];
    let group = match &editing {
        Some(id) => {
            let id: i64 = id.parse().map_err(|_| PageError::NotFound)?;
            let taken = query(
                "SELECT 1 FROM skill_set_groups WHERE name = $1 AND id <> $2",
                &[name.clone().into(), id.into()],
            )?;
            if !taken.is_empty() {
                return again("A group with that name already exists.");
            }
            let mut fields = params[..5].to_vec();
            fields.push(id.into());
            storage::transaction(&[
                Statement::new(
                    "UPDATE skill_set_groups SET name = $1, description = $2, is_doctrine = $3, \
                     is_active = $4, modified_at = now(), modified_by = $5 WHERE id = $6",
                    fields,
                ),
                Statement::new(
                    "DELETE FROM skill_set_group_sets WHERE group_id = $1",
                    vec![id.into()],
                ),
                Statement::new(
                    "INSERT INTO skill_set_group_sets (group_id, set_id) \
                     SELECT g.id, x FROM skill_set_groups g, \
                            unnest(string_to_array($2, ',')::bigint[]) AS x WHERE g.id = $1",
                    vec![id.into(), params[5].clone()],
                ),
            ])
            .map_err(|e| failed("saving the group", e))?;
            id
        }
        None => {
            let count = query("SELECT count(*) FROM skill_set_groups", &[])?;
            if count.first().map_or(0, |r| int(r, 0)) >= MAX_GROUPS {
                return again(&format!(
                    "There can be at most {MAX_GROUPS} skill set groups: delete one first."
                ));
            }
            let added = storage::query(
                "WITH g AS (INSERT INTO skill_set_groups (name, description, is_doctrine, is_active, \
                              modified_at, modified_by) \
                            VALUES ($1, $2, $3, $4, now(), $5) ON CONFLICT (name) DO NOTHING RETURNING id), \
                      m AS (INSERT INTO skill_set_group_sets (group_id, set_id) \
                            SELECT g.id, x FROM g, unnest(string_to_array($6, ',')::bigint[]) AS x \
                            RETURNING group_id) \
                 SELECT id FROM g",
                &params,
            )
            .map_err(|e| failed("saving the group", e))?;
            match added.rows.first() {
                Some(r) => int(r, 0),
                None => return again("A group with that name already exists."),
            }
        }
    };
    log::info(format!(
        "skill set group {name:?} ({group}) saved by {} ({})",
        viewer.main.name, viewer.main.id
    ));
    Ok(SubmitResult::Redirect("skill-sets".into()))
}

pub(crate) fn delete_group(viewer: &Viewer, group: &str) -> Result<SubmitResult, PageError> {
    let id: i64 = group.parse().map_err(|_| PageError::NotFound)?;
    storage::execute("DELETE FROM skill_set_groups WHERE id = $1", &[id.into()])
        .map_err(|e| failed("deleting the group", e))?;
    log::info(format!(
        "skill set group {id} deleted by {} ({})",
        viewer.main.name, viewer.main.id
    ));
    Ok(SubmitResult::Redirect("skill-sets".into()))
}

/// Reports (aa-memberaudit's `reports_access`): the Skill Sets report,
/// over the characters in the viewer's scope. Every set is counted, by
/// group; the first [`MAX_TABS`] by name each list their characters in a
/// tab.
pub(crate) fn reports(access: &Access) -> Result<Page, PageError> {
    if !access.reports {
        return Err(PageError::NotFound);
    }
    let sets = skill_sets()?;
    let groups = set_groups()?;
    let scope = access.listed(2);
    let mut page = Page::new("Reports").description(format!(
        "Skill Sets: which characters can use each, of {}",
        access.scope_words()
    ));
    let mut counts: BTreeMap<i64, i64> = BTreeMap::new();
    for set in &sets {
        counts.insert(set.id, able_count(set, &scope)?);
    }
    let mut summary = Vec::new();
    for (group, members) in grouped(&sets, &groups) {
        for set in members {
            summary.push(vec![
                group
                    .map_or_else(|| UNGROUPED.to_owned(), SetGroup::label)
                    .into(),
                set.value(),
                yes_no(group.is_some_and(|g| g.doctrine)),
                counts.get(&set.id).copied().unwrap_or(0).into(),
            ]);
        }
    }
    let mut tabs = Vec::new();
    for set in sets.iter().take(MAX_TABS) {
        let count = counts.get(&set.id).copied().unwrap_or(0);
        let able = able(set, &scope)?;
        let mut table = Table::new(vec![Column::text("Character")]).empty("Nobody yet.");
        if usize::try_from(count).unwrap_or(usize::MAX) > able.len() {
            table = table.title(format!(
                "The first {} of {}, by name",
                able.len(),
                crate::sheet::grouped(count)
            ));
        }
        tabs.push((
            set.name.clone(),
            with_rows(
                table,
                able.iter().map(|(id, n)| {
                    if access.may_open(*id) {
                        vec![
                            character(*id, n.clone())
                                .link(format!("character/{id}"))
                                .into(),
                        ]
                    } else {
                        vec![character(*id, n.clone()).into()]
                    }
                }),
            ),
        ));
    }
    page = page.table(with_rows(
        Table::new(vec![
            Column::text("Group"),
            Column::text("Skill set"),
            Column::text("Doctrine"),
            Column::numeric("Characters"),
        ])
        .title("Skill Sets")
        .empty("No skill sets yet."),
        summary,
    ));
    if sets.len() > MAX_TABS {
        page = page.text(format!(
            "The first {MAX_TABS} skill sets by name each list their characters in a tab below. \
             The others are counted above."
        ));
    }
    for (name, table) in tabs {
        page = page.tab(name, vec![Section::Table(table)]);
    }
    Ok(page)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_has_a_required_level_a_recommended_one_or_both() {
        assert_eq!(
            parse_line("Caldari Battleship 4").unwrap(),
            ("Caldari Battleship".to_owned(), Some(4), None)
        );
        assert_eq!(
            parse_line("Caldari Battleship IV [V]").unwrap(),
            ("Caldari Battleship".to_owned(), Some(4), Some(5))
        );
        assert_eq!(
            parse_line("  Caldari Battleship [5] ").unwrap(),
            ("Caldari Battleship".to_owned(), None, Some(5))
        );
        // A skill whose name ends in a number keeps it when a level follows.
        assert_eq!(
            parse_line("Gunnery 3 5").unwrap(),
            ("Gunnery 3".to_owned(), Some(5), None)
        );
        assert!(parse_line("Caldari Battleship").is_err());
        assert!(parse_line("Caldari Battleship 6").is_err());
        assert!(parse_line("Gunnery [7]").is_err());
        assert!(parse_line("4").is_err());
    }

    #[test]
    fn skills_read_as_aa_lists_them() {
        let skill = |required, recommended| SetSkill {
            id: 3300,
            name: "Gunnery".to_owned(),
            required,
            recommended,
        };
        assert_eq!(skill(Some(4), Some(5)).label(), "Gunnery IV [V]");
        assert_eq!(skill(Some(4), None).label(), "Gunnery IV");
        assert_eq!(skill(None, Some(5)).label(), "Gunnery [V]");
    }

    #[test]
    fn sets_are_grouped_as_aa_groups_them() {
        let set = |id: i64, name: &str| SkillSet {
            id,
            name: name.to_owned(),
            description: String::new(),
            ship: None,
            visible: true,
            skills: Vec::new(),
        };
        let sets = vec![set(1, "Ferox"), set(2, "Logi"), set(3, "Scout")];
        let group = |id: i64, name: &str, doctrine: bool, sets: Vec<i64>| SetGroup {
            id,
            name: name.to_owned(),
            description: String::new(),
            doctrine,
            active: true,
            sets,
        };
        let groups = vec![
            group(1, "Ferox fleet", true, vec![1, 2]),
            group(2, "Logistics", false, vec![2]),
            group(3, "Empty", false, vec![]),
        ];
        let out: Vec<(Option<String>, Vec<&str>)> = grouped(&sets, &groups)
            .into_iter()
            .map(|(g, s)| {
                (
                    g.map(SetGroup::label),
                    s.iter().map(|s| s.name.as_str()).collect(),
                )
            })
            .collect();
        assert_eq!(
            out,
            vec![
                (
                    Some("Doctrine: Ferox fleet".to_owned()),
                    vec!["Ferox", "Logi"]
                ),
                (Some("Logistics".to_owned()), vec!["Logi"]),
                (None, vec!["Scout"]),
            ]
        );
        let mut idle = group(4, "Old", true, vec![]);
        idle.active = false;
        assert_eq!(idle.sheet_label(), "Old [Not active]");
    }
}
