//! Bulletin Board (aa-bulletin-board).
//!
//! - **Bulletins** (`basic_access`): newest first, each with its title,
//!   who wrote it and when; opened on a page of its own. A bulletin
//!   limited to groups is seen only by their members; one limited to none
//!   by everyone with access.
//! - **Managing** (`manage_bulletins`, as aa-bulletin-board, who see every
//!   bulletin): write, edit and remove them, and limit them to groups.
//!
//! aa-bulletin-board's text is rich text; apps show text, not HTML, so a
//! bulletin is plain text in paragraphs (a blank line between them).

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use tether_plugin_sdk::identity::{self, Viewer};
use tether_plugin_sdk::storage::{self, Value as Db};
use tether_plugin_sdk::{
    Card, Column, Field, Form, Page, PageError, Plugin, Request, Submission, SubmitResult, Table,
    Tone, Value, action, character, link, log, time,
};

const MAX_TITLE: u32 = 255;
const MAX_CONTENT: u32 = 10_000;
/// A page's text sections hold 2 KiB each; a bulletin's paragraphs are cut
/// to this many bytes, at most this many of them.
const PARAGRAPH_BYTES: usize = 1_800;
const MAX_PARAGRAPHS: usize = 30;
/// Groups offered in the picker at most.
const MAX_OFFERED: usize = 100;
/// Groups one bulletin may be limited to.
const MAX_GROUPS: usize = 20;
/// A text value's bytes at most (the host's 2 KiB).
const MAX_VALUE_BYTES: usize = 1_900;
const LIST_ROWS: i64 = 500;

struct BulletinBoard;

impl Plugin for BulletinBoard {
    fn render(request: Request) -> Result<Page, PageError> {
        let viewer = identity::viewer().ok_or(PageError::Forbidden)?;
        let path = request.path.as_str();
        if let Some(id) = path.strip_prefix("bulletin/") {
            return bulletin_page(&viewer, id.parse().map_err(|_| PageError::NotFound)?);
        }
        if let Some(id) = path.strip_prefix("edit/") {
            // The page rule asks for manage_bulletins; so does this.
            if !manager(&viewer) {
                return Err(PageError::NotFound);
            }
            return edit_page(&viewer, id.parse().map_err(|_| PageError::NotFound)?, None);
        }
        match path {
            "" => list_page(&viewer),
            "new" if manager(&viewer) => new_page(&viewer, None),
            _ => Err(PageError::NotFound),
        }
    }

    fn submit(submission: Submission) -> Result<SubmitResult, PageError> {
        let viewer = identity::viewer().ok_or(PageError::Forbidden)?;
        // Only managers change bulletins, whatever page it's posted from.
        if !manager(&viewer) {
            return Err(PageError::Forbidden);
        }
        let path = submission.request.path.as_str();
        if let Some(id) = path.strip_prefix("edit/") {
            let id: i64 = id.parse().map_err(|_| PageError::NotFound)?;
            return match submission.form.as_str() {
                "bulletin" => save(&viewer, Some(id), &submission),
                "add_group" => add_group(&viewer, id, &submission),
                "remove_group" => remove_group(&viewer, id, &submission),
                "remove" => remove(&viewer, id),
                _ => Err(PageError::NotFound),
            };
        }
        match (path, submission.form.as_str()) {
            ("new", "bulletin") => save(&viewer, None, &submission),
            // A row's Remove on the list.
            ("", "remove") => {
                let id: i64 = submission
                    .value("bulletin")
                    .parse()
                    .map_err(|_| PageError::NotFound)?;
                remove(&viewer, id)
            }
            _ => Err(PageError::NotFound),
        }
    }
}

tether_plugin_sdk::export!(BulletinBoard);

// ---- helpers ---------------------------------------------------------------

fn failed(what: &str, err: impl std::fmt::Debug) -> PageError {
    PageError::Failed(format!("{what}: {err:?}"))
}

fn int(row: &[Db], i: usize) -> i64 {
    row.get(i).and_then(Db::as_integer).unwrap_or_default()
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

fn manager(viewer: &Viewer) -> bool {
    viewer.can("manage_bulletins")
}

/// A bulletin as stored.
struct Bulletin {
    id: i64,
    title: String,
    content: String,
    created: Option<DateTime<Utc>>,
    updated: Option<DateTime<Utc>>,
    author_id: i64,
    author_name: String,
    groups: Vec<i64>,
}

fn bulletin_from(row: &[Db], groups: Vec<i64>) -> Bulletin {
    Bulletin {
        id: int(row, 0),
        title: text(row, 1),
        content: text(row, 2),
        created: when(row, 3),
        updated: when(row, 4),
        author_id: int(row, 5),
        author_name: text(row, 6),
        groups,
    }
}

const COLUMNS: &str = "b.id, b.title, b.content, b.created_at, b.updated_at, b.author_id, \
     b.author_name, coalesce((SELECT string_agg(g.group_id::text, ',') \
         FROM bulletin_groups g WHERE g.bulletin_id = b.id), '')";

fn from_rows(rows: &[Vec<Db>]) -> Vec<Bulletin> {
    rows.iter()
        .map(|r| {
            let groups = text(r, 7)
                .split(',')
                .filter_map(|g| g.parse().ok())
                .collect();
            bulletin_from(r, groups)
        })
        .collect()
}

/// The newest bulletins with their groups, without their text (the list
/// needn't carry it: 500 long ones would be megabytes).
fn bulletins() -> Result<Vec<Bulletin>, PageError> {
    let columns = COLUMNS.replace("b.content", "''");
    let rows = storage::query(
        &format!(
            "SELECT {columns} FROM bulletins b ORDER BY b.created_at DESC, b.id DESC LIMIT {LIST_ROWS}"
        ),
        &[],
    )
    .map_err(|e| failed("reading bulletins", e))?;
    Ok(from_rows(&rows.rows))
}

/// One bulletin, however old.
fn one(id: i64) -> Result<Bulletin, PageError> {
    let rows = storage::query(
        &format!("SELECT {COLUMNS} FROM bulletins b WHERE b.id = $1"),
        &[id.into()],
    )
    .map_err(|e| failed("reading the bulletin", e))?;
    from_rows(&rows.rows)
        .into_iter()
        .next()
        .ok_or(PageError::NotFound)
}

/// Whether the viewer sees a bulletin: managers every one (as
/// aa-bulletin-board); others one limited to no group, or to one of theirs.
fn sees(viewer: &Viewer, mine: &[i64], bulletin: &Bulletin) -> bool {
    manager(viewer)
        || bulletin.groups.is_empty()
        || bulletin.groups.iter().any(|g| mine.contains(g))
}

fn my_groups() -> Vec<i64> {
    identity::groups().into_iter().map(|g| g.id).collect()
}

/// Group names the viewer knows (theirs, and those offered to them).
fn group_names() -> BTreeMap<i64, String> {
    identity::all_groups()
        .into_iter()
        .chain(identity::groups())
        .map(|g| (g.id, g.name))
        .collect()
}

fn names_of(groups: &[i64], names: &BTreeMap<i64, String>) -> String {
    if groups.is_empty() {
        return "Everyone".to_owned();
    }
    let mut named: Vec<String> = groups
        .iter()
        .map(|g| {
            names
                .get(g)
                .cloned()
                .unwrap_or_else(|| "a group".to_owned())
        })
        .collect();
    named.sort();
    fit(&named.join(", "))
}

/// Text cut to fit one of the host's text values.
fn fit(text: &str) -> String {
    if text.len() <= MAX_VALUE_BYTES {
        return text.to_owned();
    }
    let mut cut = MAX_VALUE_BYTES;
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}…", &text[..cut])
}

/// A bulletin's text as paragraphs: split at blank lines, each cut to fit
/// a text section, at most `MAX_PARAGRAPHS` (the rest joined to the last).
fn paragraphs(content: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for paragraph in content.replace("\r\n", "\n").split("\n\n") {
        let paragraph = paragraph.split_whitespace().collect::<Vec<_>>().join(" ");
        let mut rest = paragraph.as_str();
        while !rest.is_empty() {
            let mut cut = rest.len().min(PARAGRAPH_BYTES);
            while !rest.is_char_boundary(cut) {
                cut -= 1;
            }
            out.push(rest[..cut].to_owned());
            rest = &rest[cut..];
        }
    }
    if out.len() > MAX_PARAGRAPHS {
        // Saving refuses text this long; any saved before is cut to fit.
        let tail = out.split_off(MAX_PARAGRAPHS - 1).join(" ");
        out.push(fit(&tail));
    }
    out
}

fn with_links(page: Page, viewer: &Viewer) -> Page {
    let page = page.link("Bulletin Board", "");
    if manager(viewer) {
        page.button("Create bulletin", "new")
    } else {
        page
    }
}

// ---- pages -------------------------------------------------------------------

fn list_page(viewer: &Viewer) -> Result<Page, PageError> {
    let mine = my_groups();
    let names = group_names();
    let managing = manager(viewer);
    let mut columns = vec![
        Column::text("Bulletin"),
        Column::text("By"),
        Column::numeric("Created"),
        Column::numeric("Updated"),
    ];
    if managing {
        columns.push(Column::text("Groups"));
        columns.push(Column::text(""));
    }
    let mut table = Table::new(columns)
        .title("Bulletins")
        .empty("No bulletins yet.");
    for b in bulletins()?.iter().filter(|b| sees(viewer, &mine, b)) {
        let mut row: Vec<Value> = vec![
            link(b.title.clone(), format!("bulletin/{}", b.id)).into(),
            character(b.author_id, b.author_name.clone()).into(),
            b.created.map_or_else(|| "".into(), |t| time(rfc3339(t))),
            b.updated.map_or_else(|| "".into(), |t| time(rfc3339(t))),
        ];
        if managing {
            row.push(names_of(&b.groups, &names).into());
            row.push(
                action("Remove", "remove")
                    .field("bulletin", b.id.to_string())
                    .tone(Tone::Danger)
                    .confirm(format!("{} is removed for everyone.", b.title))
                    .into(),
            );
        }
        table = table.row(row);
    }
    Ok(with_links(
        Page::new("Bulletin Board").description("Bulletins, newest first"),
        viewer,
    )
    .table(table))
}

fn bulletin_page(viewer: &Viewer, id: i64) -> Result<Page, PageError> {
    let bulletin = one(id)?;
    // As aa-bulletin-board: one you may not see is as if it weren't there.
    if !sees(viewer, &my_groups(), &bulletin) {
        return Err(PageError::NotFound);
    }
    let mut about = Card::new("About this bulletin").field(
        "By",
        character(bulletin.author_id, bulletin.author_name.clone()),
    );
    if let Some(created) = bulletin.created {
        about = about.field("Created", time(rfc3339(created)));
    }
    if let Some(updated) = bulletin.updated {
        about = about.field("Updated", time(rfc3339(updated)));
    }
    if manager(viewer) {
        about = about.field("Groups", names_of(&bulletin.groups, &group_names()));
    }
    let mut page = with_links(Page::new(bulletin.title.clone()), viewer);
    if manager(viewer) {
        page = page.link("Edit", format!("edit/{id}"));
    }
    for paragraph in paragraphs(&bulletin.content) {
        page = page.text(paragraph);
    }
    Ok(page.card(about))
}

fn bulletin_form(title: &str, content: &str, submit: &str) -> Form {
    Form::new("bulletin", submit)
        .field(
            Field::text("title", "Title", MAX_TITLE)
                .value(title)
                .required(),
        )
        .field(
            Field::textarea("content", "Text", MAX_CONTENT)
                .value(content)
                .help("Plain text; a blank line starts a new paragraph.")
                .required(),
        )
}

fn new_page(viewer: &Viewer, problem: Option<(&str, &str, &str)>) -> Result<Page, PageError> {
    let mut page = with_links(
        Page::new("Create bulletin")
            .description("Everyone with access sees it, until you limit it to groups"),
        viewer,
    );
    let (title, content) = match problem {
        Some((note, title, content)) => {
            page = page.text(note);
            (title, content)
        }
        None => ("", ""),
    };
    Ok(page.form(bulletin_form(title, content, "Create bulletin")))
}

fn edit_page(
    viewer: &Viewer,
    id: i64,
    problem: Option<(&str, &str, &str)>,
) -> Result<Page, PageError> {
    let bulletin = one(id)?;
    let mut page = with_links(
        Page::new(format!("Edit {}", bulletin.title))
            .description("Its title, text and who sees it")
            .link("Bulletin", format!("bulletin/{id}")),
        viewer,
    );
    let (title, content) = match problem {
        Some((note, title, content)) => {
            page = page.text(note);
            (title.to_owned(), content.to_owned())
        }
        None => (bulletin.title.clone(), bulletin.content.clone()),
    };
    page = page.form(bulletin_form(&title, &content, "Save bulletin"));
    // Groups: those the manager may be offered, not on it yet.
    let offered: Vec<(String, String)> = identity::all_groups()
        .into_iter()
        .filter(|g| !bulletin.groups.contains(&g.id))
        .take(MAX_OFFERED)
        .map(|g| (g.id.to_string(), g.name))
        .collect();
    if !offered.is_empty() {
        page = page.form(
            Form::new("add_group", "Limit to group")
                .field(Field::select("group", "Group", offered).required()),
        );
    }
    let names = group_names();
    let mut groups = Table::new(vec![Column::text("Group"), Column::text("")])
        .title("Groups")
        .empty("No groups: everyone with access sees this bulletin.");
    for group in &bulletin.groups {
        let name = names
            .get(group)
            .cloned()
            .unwrap_or_else(|| "a group".to_owned());
        groups = groups.row(vec![
            name.clone().into(),
            action("Remove", "remove_group")
                .field("group", group.to_string())
                .confirm(format!(
                    "{name} no longer limits this bulletin; with no groups left, everyone \
                     with access sees it."
                ))
                .into(),
        ]);
    }
    Ok(page.table(groups).card(
        Card::new("Remove this bulletin").field(
            "Remove",
            action("Remove bulletin", "remove")
                .tone(Tone::Danger)
                .confirm(format!("{} is removed for everyone.", bulletin.title)),
        ),
    ))
}

// ---- forms -------------------------------------------------------------------

fn save(
    viewer: &Viewer,
    id: Option<i64>,
    submission: &Submission,
) -> Result<SubmitResult, PageError> {
    let title = submission.value("title").trim();
    let content = submission.value("content").trim();
    let problem = if title.is_empty() || title.chars().count() > MAX_TITLE as usize {
        Some("A title is 1 to 255 characters.")
    } else if title.chars().any(char::is_control) {
        Some("A title is one line.")
    } else if content.is_empty() || content.chars().count() > MAX_CONTENT as usize {
        Some("The text is 1 to 10,000 characters.")
    } else if paragraphs(content).len() >= MAX_PARAGRAPHS {
        Some("The text is at most 29 paragraphs (a long one counts once for each 1,800 bytes).")
    } else {
        None
    };
    if let Some(note) = problem {
        return Ok(SubmitResult::Page(match id {
            Some(id) => edit_page(viewer, id, Some((note, title, content)))?,
            None => new_page(viewer, Some((note, title, content)))?,
        }));
    }
    let saved = match id {
        Some(id) => {
            let changed = storage::execute(
                "UPDATE bulletins SET title = $1, content = $2, updated_at = now() WHERE id = $3",
                &[title.into(), content.into(), id.into()],
            )
            .map_err(|e| failed("saving the bulletin", e))?;
            if changed == 0 {
                return Err(PageError::NotFound);
            }
            id
        }
        None => {
            let rows = storage::query(
                "INSERT INTO bulletins (title, content, author_id, author_name) \
                 VALUES ($1, $2, $3, $4) RETURNING id",
                &[
                    title.into(),
                    content.into(),
                    viewer.main.id.into(),
                    viewer.main.name.clone().into(),
                ],
            )
            .map_err(|e| failed("creating the bulletin", e))?;
            rows.rows
                .first()
                .map(|r| int(r, 0))
                .ok_or(PageError::NotFound)?
        }
    };
    log::info(format!(
        "bulletin {saved} ({title}) saved by {} ({})",
        viewer.main.name, viewer.main.id
    ));
    Ok(SubmitResult::Redirect(format!("bulletin/{saved}")))
}

fn add_group(viewer: &Viewer, id: i64, submission: &Submission) -> Result<SubmitResult, PageError> {
    let bulletin = one(id)?;
    if bulletin.groups.len() >= MAX_GROUPS {
        return Ok(SubmitResult::Page(edit_page(
            viewer,
            id,
            Some((
                "A bulletin is limited to at most 20 groups.",
                &bulletin.title,
                &bulletin.content,
            )),
        )?));
    }
    let group: i64 = submission
        .value("group")
        .parse()
        .map_err(|_| PageError::NotFound)?;
    // Only a group the manager may be offered.
    if !identity::all_groups().iter().any(|g| g.id == group) {
        return Err(PageError::NotFound);
    }
    storage::execute(
        "INSERT INTO bulletin_groups (bulletin_id, group_id) VALUES ($1, $2) ON CONFLICT DO NOTHING",
        &[id.into(), group.into()],
    )
    .map_err(|e| failed("limiting the bulletin", e))?;
    log::info(format!(
        "bulletin {id} limited to group {group} by {} ({})",
        viewer.main.name, viewer.main.id
    ));
    Ok(SubmitResult::Redirect(format!("edit/{id}")))
}

fn remove_group(
    viewer: &Viewer,
    id: i64,
    submission: &Submission,
) -> Result<SubmitResult, PageError> {
    let group: i64 = submission
        .value("group")
        .parse()
        .map_err(|_| PageError::NotFound)?;
    storage::execute(
        "DELETE FROM bulletin_groups WHERE bulletin_id = $1 AND group_id = $2",
        &[id.into(), group.into()],
    )
    .map_err(|e| failed("unlimiting the bulletin", e))?;
    log::info(format!(
        "bulletin {id} no longer limited to group {group}, by {} ({})",
        viewer.main.name, viewer.main.id
    ));
    Ok(SubmitResult::Redirect(format!("edit/{id}")))
}

fn remove(viewer: &Viewer, id: i64) -> Result<SubmitResult, PageError> {
    storage::execute("DELETE FROM bulletins WHERE id = $1", &[id.into()])
        .map_err(|e| failed("removing the bulletin", e))?;
    log::info(format!(
        "bulletin {id} removed by {} ({})",
        viewer.main.name, viewer.main.id
    ));
    Ok(SubmitResult::Redirect(String::new()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_becomes_paragraphs_that_fit() {
        assert_eq!(
            paragraphs("Form up at\n1DQ.\n\nBring  a  fit."),
            vec!["Form up at 1DQ.".to_owned(), "Bring a fit.".to_owned()]
        );
        let long = "é".repeat(2_000);
        let parts = paragraphs(&long);
        assert!(parts.iter().all(|p| p.len() <= PARAGRAPH_BYTES));
        assert_eq!(parts.concat(), long);
        let many = vec!["x"; 50].join("\n\n");
        assert_eq!(paragraphs(&many).len(), MAX_PARAGRAPHS);
        // A long tail (text saved before the check) is cut to fit.
        let tail = vec!["y".repeat(100); 60].join("\n\n");
        assert!(paragraphs(&tail).iter().all(|p| p.len() <= 2_048));
    }
}
