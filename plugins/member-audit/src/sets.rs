//! Skill Sets (named skill lists, such as a doctrine) and Reports.

use tether_plugin_sdk::identity::Viewer;
use tether_plugin_sdk::storage::{self, Value as Db};
use tether_plugin_sdk::{
    Column, Field, Form, Page, PageError, Section, Submission, SubmitResult, Table, Tone, badge,
    character, log,
};

use crate::access::Access;
use crate::{failed, int, name_of, query, text, with_rows};

/// Skill sets and the skills in one, so the Skill Sets page stays within
/// the host's page limits.
const MAX_SETS: i64 = 30;
const MAX_SKILLS_PER_SET: usize = 50;

struct SkillSet {
    id: i64,
    name: String,
    skills: Vec<(i64, String, i64)>,
}

fn skill_sets() -> Result<Vec<SkillSet>, PageError> {
    let sets = query("SELECT id, name FROM skill_sets ORDER BY name", &[])?;
    let mut out = Vec::new();
    for s in &sets {
        let id = int(s, 0);
        let skills = query(
            &format!(
                "SELECT skill_id, {skill}, level FROM skill_set_skills k WHERE set_id = $1 ORDER BY 2",
                skill = name_of("k.skill_id")
            ),
            &[id.into()],
        )?;
        out.push(SkillSet {
            id,
            name: text(s, 1),
            skills: skills
                .iter()
                .map(|r| (int(r, 0), text(r, 1), int(r, 2)))
                .collect(),
        });
    }
    Ok(out)
}

/// Characters meeting every skill of the set, of those `scope` (SQL over
/// `characters c`, with parameters from `$2`) picks.
fn able(set: &SkillSet, scope: &(String, Vec<Db>)) -> Result<Vec<(i64, String)>, PageError> {
    let (condition, scope_params) = scope;
    let sql = format!(
        "SELECT c.character_id, c.name FROM characters c WHERE {condition} AND NOT EXISTS ( \
           SELECT 1 FROM skill_set_skills k WHERE k.set_id = $1 AND NOT EXISTS ( \
             SELECT 1 FROM skills s WHERE s.character_id = c.character_id \
               AND s.skill_id = k.skill_id AND s.active_level >= k.level)) \
         ORDER BY c.name LIMIT 500"
    );
    let mut params: Vec<Db> = vec![set.id.into()];
    params.extend(scope_params.iter().cloned());
    Ok(query(&sql, &params)?
        .iter()
        .map(|r| (int(r, 0), text(r, 1)))
        .collect())
}

/// The viewer's own characters, as a scope for [`able`].
fn own(viewer: &Viewer) -> (String, Vec<Db>) {
    let ids: Vec<String> = viewer.characters.iter().map(|c| c.id.to_string()).collect();
    (
        "c.character_id = ANY(string_to_array($2, ',')::bigint[])".to_owned(),
        vec![ids.join(",").into()],
    )
}

/// Which skill sets a character can use, and what each still needs: for
/// the Character Sheet's Skill Sets tab.
pub(crate) fn for_character(id: i64) -> Result<Vec<(String, Vec<String>)>, PageError> {
    let sets = skill_sets()?;
    let mut out = Vec::new();
    for set in sets {
        let missing = query(
            &format!(
                "SELECT {skill}, k.level, coalesce(s.active_level, 0) FROM skill_set_skills k \
                 LEFT JOIN skills s ON s.character_id = $2 AND s.skill_id = k.skill_id \
                 WHERE k.set_id = $1 AND coalesce(s.active_level, 0) < k.level ORDER BY 1",
                skill = name_of("k.skill_id")
            ),
            &[set.id.into(), id.into()],
        )?;
        out.push((
            set.name,
            missing
                .iter()
                .map(|r| format!("{} {} (has {})", text(r, 0), int(r, 1), int(r, 2)))
                .collect(),
        ));
    }
    Ok(out)
}

/// The Skill Sets page: for `view_skill_sets` (which of your characters
/// can use each), and for `manage` (to add and delete them).
pub(crate) fn skill_sets_page(access: &Access, note: Option<&str>) -> Result<Page, PageError> {
    let viewer = access.viewer;
    // The manifest's rule asks for view_skill_sets too.
    if !access.skill_sets {
        return Err(PageError::NotFound);
    }
    let sets = skill_sets()?;
    let mine = own(viewer);
    let mut page = Page::new("Skill sets").description(
        "Named lists of skills, such as a doctrine, and which of your characters can use them",
    );
    if let Some(note) = note {
        page = page.text(note);
    }
    let mut rows = Vec::new();
    for set in &sets {
        let able = able(set, &mine)?;
        let skills: Vec<String> = set
            .skills
            .iter()
            .map(|(_, name, level)| format!("{name} {level}"))
            .collect();
        let mut listed = skills.join(", ");
        if listed.chars().count() > 1500 {
            listed = listed.chars().take(1500).collect::<String>() + "…";
        }
        rows.push(vec![
            set.name.clone().into(),
            listed.into(),
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
    page = page.table(with_rows(
        Table::new(vec![
            Column::text("Skill set"),
            Column::text("Skills"),
            Column::text("Your characters who can"),
        ])
        .empty("No skill sets yet."),
        rows,
    ));
    if viewer.can("manage") {
        page = page.form(
            Form::new("add_set", "Add skill set")
                .title("New skill set")
                .description("One skill per line with its level, as `Caldari Battleship 4`. Only skills some member has trained are known. Secure Groups with a skill set filter follow its changes: changing or deleting a set changes who is in them.")
                .field(Field::text("name", "Name", 100).required())
                .field(Field::textarea("skills", "Skills", 5000).required()),
        );
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
    }
    Ok(page)
}

/// `Skill Name 4` lines, matched to known skills.
pub(crate) fn parse_skills(text: &str) -> Result<Vec<(i64, i64)>, String> {
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
    let mut out = Vec::new();
    for line in lines {
        let line: String = line.chars().take(100).collect();
        let line = line.as_str();
        let (name, level) = line
            .rsplit_once(' ')
            .ok_or_else(|| format!("\"{line}\" needs a level after the skill"))?;
        let level: i64 = level
            .trim()
            .parse()
            .ok()
            .filter(|l| (1..=5).contains(l))
            .ok_or_else(|| format!("\"{line}\": the level is 1 to 5"))?;
        let found = storage::query(
            "SELECT id FROM names WHERE lower(name) = lower($1) \
             AND id IN (SELECT DISTINCT skill_id FROM skills) LIMIT 1",
            &[name.trim().into()],
        )
        .map_err(|e| format!("reading skills: {e:?}"))?;
        let id =
            found.rows.first().map(|r| int(r, 0)).ok_or_else(|| {
                format!("\"{}\" isn't a skill any member has trained", name.trim())
            })?;
        out.push((id, level));
    }
    if out.is_empty() {
        return Err("List at least one skill.".to_owned());
    }
    Ok(out)
}

pub(crate) fn add_set(access: &Access, submission: &Submission) -> Result<SubmitResult, PageError> {
    let viewer = access.viewer;
    let name = submission.value("name").trim().to_owned();
    let skills = match parse_skills(submission.value("skills")) {
        Ok(skills) => skills,
        Err(why) => return Ok(SubmitResult::Page(skill_sets_page(access, Some(&why))?)),
    };
    let count = storage::query("SELECT count(*) FROM skill_sets", &[])
        .map_err(|e| failed("counting skill sets", e))?;
    if count.rows.first().map_or(0, |r| int(r, 0)) >= MAX_SETS {
        return Ok(SubmitResult::Page(skill_sets_page(
            access,
            Some(&format!(
                "There can be at most {MAX_SETS} skill sets: delete one first."
            )),
        )?));
    }
    let rows: Vec<serde_json::Value> = skills
        .iter()
        .map(|(skill, level)| serde_json::json!({ "skill_id": skill, "level": level }))
        .collect();
    // The set and its skills together, or neither.
    let added = storage::query(
        "WITH s AS (INSERT INTO skill_sets (name) VALUES ($1) ON CONFLICT (name) DO NOTHING RETURNING id), \
              k AS (INSERT INTO skill_set_skills (set_id, skill_id, level) \
                    SELECT s.id, x.skill_id, max(x.level) FROM s, \
                           json_to_recordset($2::json) AS x(skill_id bigint, level int) \
                    GROUP BY s.id, x.skill_id RETURNING set_id) \
         SELECT id FROM s",
        &[name.clone().into(), Db::json(serde_json::Value::Array(rows).to_string())],
    )
    .map_err(|e| failed("saving the skill set", e))?;
    if added.rows.is_empty() {
        return Ok(SubmitResult::Page(skill_sets_page(
            access,
            Some("A skill set with that name already exists."),
        )?));
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

/// Reports (aa-memberaudit's `reports_access`): the Skill Sets report,
/// over the characters in the viewer's scope.
pub(crate) fn reports(access: &Access) -> Result<Page, PageError> {
    if !access.reports {
        return Err(PageError::NotFound);
    }
    let sets = skill_sets()?;
    let scope = access.listed(2);
    let mut page = Page::new("Reports").description(format!(
        "Skill Sets: which characters can use each, of {}",
        access.scope_words()
    ));
    let mut summary = Vec::new();
    let mut tabs = Vec::new();
    for set in sets.iter().take(10) {
        let able = able(set, &scope)?;
        summary.push(vec![set.name.clone().into(), crate::count(able.len())]);
        tabs.push((
            set.name.clone(),
            with_rows(
                Table::new(vec![Column::text("Character")]).empty("Nobody yet."),
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
            Column::text("Skill set"),
            Column::numeric("Characters"),
        ])
        .title("Skill Sets")
        .empty("No skill sets yet."),
        summary,
    ));
    for (name, table) in tabs {
        page = page.tab(name, vec![Section::Table(table)]);
    }
    Ok(page)
}
