//! Fittings (Alliance Auth's allianceauth-fittings, asked for by Jay:
//! doctrine designers paste EFT fits and list them in doctrines).
//!
//! - **Doctrines** (AA's dashboard): a card per doctrine, with its icon
//!   hull, and a page per doctrine listing its fits.
//! - **All fits**: every fit, searchable; a page per fit with its modules
//!   by slot, the EFT text to copy, notes, doctrines, categories and
//!   required skills.
//! - **Categories**: AA's tags on fits and doctrines, which limit who sees
//!   them to groups.
//! - **New fit** (EFT text), **New doctrine**, **New category**, and
//!   editing and deleting all three, for `manage`.
//!
//! AA's permissions and rules exactly: `access_fittings` sees,
//! `manage` changes and sees everything. A category with groups is seen
//! only by their members, one without by everyone. A doctrine in
//! categories is seen through any category the viewer sees; one in none
//! is public. A fit is public unless one of its categories (its own or its
//! doctrines') has groups; then it's seen through any category the viewer
//! sees.
//!
//! Item names become type ids through ESI's public universe endpoints
//! (see `lookup`), kept in storage.

mod categories;
mod doctrines;
mod eft;
mod fits;
mod lookup;
mod pilots;

use tether_plugin_sdk::identity::{self, Viewer};
use tether_plugin_sdk::jobs::{Job, JobError};
use tether_plugin_sdk::storage::{self, Value as Db};
use tether_plugin_sdk::{Page, PageError, Plugin, Request, Submission, SubmitResult};

struct Fittings;

impl Plugin for Fittings {
    fn render(request: Request) -> Result<Page, PageError> {
        let viewer = identity::viewer().ok_or(PageError::Forbidden)?;
        let access = Access::of(&viewer);
        let parts: Vec<&str> = request.path.split('/').collect();
        match parts.as_slice() {
            [""] => doctrines::list(&access),
            ["fits"] => fits::list(&access, request.search()),
            ["fit", id] => fits::page(&access, &viewer, number(id)?, None),
            ["doctrine", id] => doctrines::page(&access, number(id)?),
            ["categories"] => categories::list(&access),
            ["category", id] => categories::page(&access, number(id)?),
            ["add-fit"] if access.manage => fits::add_page(None),
            ["add-doctrine"] if access.manage => doctrines::add_page(None),
            ["add-category"] if access.manage => categories::add_page(None),
            ["edit", "fit", id] if access.manage => fits::edit_page(&access, number(id)?, None),
            ["edit", "doctrine", id] if access.manage => {
                doctrines::edit_page(&access, number(id)?, None)
            }
            ["edit", "category", id] if access.manage => {
                categories::edit_page(&access, number(id)?, None)
            }
            _ => Err(PageError::NotFound),
        }
    }

    fn submit(submission: Submission) -> Result<SubmitResult, PageError> {
        let viewer = identity::viewer().ok_or(PageError::Forbidden)?;
        let access = Access::of(&viewer);
        let path = submission.request.path.clone();
        let parts: Vec<&str> = path.split('/').collect();
        let form = submission.form.as_str();
        // A pilot's own: saving a fit to their character, reading its skills.
        if let (["fit", id], "save_to_eve" | "read_skills") = (parts.as_slice(), form) {
            let id = number(id)?;
            let character = number(submission.value("character"))?;
            return if form == "save_to_eve" {
                fits::save_to_eve(&access, &viewer, id, character)
            } else {
                fits::read_skills(&access, &viewer, id, character)
            };
        }
        // Everything else changes fits, doctrines or categories.
        if !access.manage {
            return Err(PageError::Forbidden);
        }
        let posted = |name: &str| number(submission.value(name));
        let changed = match (parts.as_slice(), form) {
            (["add-fit"], "fit") => fits::save(&access, &viewer, None, &submission),
            (["fits"], "delete_fit") => fits::delete(&viewer, posted("fit")?),
            (["edit", "fit", id], _) => {
                let id = number(id)?;
                match form {
                    "fit" => fits::save(&access, &viewer, Some(id), &submission),
                    "add_doctrine" => fits::add_to_doctrine(id, posted("doctrine")?),
                    "remove_doctrine" => fits::remove_from_doctrine(id, posted("doctrine")?),
                    "delete_fit" => fits::delete(&viewer, id),
                    _ => Err(PageError::NotFound),
                }
            }
            (["add-doctrine"], "doctrine") => doctrines::save(&access, None, &submission),
            (["edit", "doctrine", id], _) => {
                let id = number(id)?;
                match form {
                    "doctrine" => doctrines::save(&access, Some(id), &submission),
                    "add_fit" => doctrines::add_fit(id, posted("fit")?),
                    "remove_fit" => doctrines::remove_fit(id, posted("fit")?),
                    "delete_doctrine" => doctrines::delete(&viewer, id),
                    _ => Err(PageError::NotFound),
                }
            }
            (["categories"], "delete_category") => categories::delete(&viewer, posted("category")?),
            (["add-category"], "category") => categories::save(&access, None, &submission),
            (["edit", "category", id], _) => {
                let id = number(id)?;
                match form {
                    "category" => categories::save(&access, Some(id), &submission),
                    "add_group" => categories::add_group(&access, id, posted("group")?),
                    "remove_group" => categories::remove(id, "group", posted("group")?),
                    "add_doctrine" => categories::add(id, "doctrine", posted("doctrine")?),
                    "remove_doctrine" => categories::remove(id, "doctrine", posted("doctrine")?),
                    "add_fit" => categories::add(id, "fit", posted("fit")?),
                    "remove_fit" => categories::remove(id, "fit", posted("fit")?),
                    "delete_category" => categories::delete(&viewer, id),
                    _ => Err(PageError::NotFound),
                }
            }
            _ => Err(PageError::NotFound),
        };
        // Fleet Pings and FAT see doctrines as they are now.
        if changed.is_ok() {
            doctrines::share();
        }
        changed
    }

    fn run_job(job: Job) -> Result<(), JobError> {
        match job.name.as_str() {
            lookup::DETAILS => {
                doctrines::share();
                lookup::details()
            }
            pilots::SKILLS => pilots::sync(),
            other => Err(JobError::Permanent(format!("no job {other}"))),
        }
    }
}

tether_plugin_sdk::export!(Fittings);

// ---- who sees what ------------------------------------------------------------

/// What the viewer may do, and the groups they're in.
pub(crate) struct Access {
    /// AA's `fittings.manage`: add, edit and delete, and see everything.
    pub manage: bool,
    /// The viewer's group ids, for `string_to_array($1, ',')`.
    groups: String,
}

impl Access {
    fn of(viewer: &Viewer) -> Self {
        let manage = viewer.can("manage");
        let groups: Vec<i64> = if manage {
            Vec::new()
        } else {
            identity::groups().into_iter().map(|g| g.id).collect()
        };
        Self {
            manage,
            groups: id_list(&groups),
        }
    }

    /// The first two parameters of every query that checks what the
    /// viewer sees (see [`CATEGORY_SEEN`]): their groups, and whether
    /// they manage.
    pub fn params(&self) -> Vec<Db> {
        vec![self.groups.clone().into(), self.manage.into()]
    }

    /// [`params`](Self::params), then `more` as `$3`, `$4`...
    pub fn with(&self, more: Vec<Db>) -> Vec<Db> {
        let mut params = self.params();
        params.extend(more);
        params
    }
}

/// A category `c` the viewer sees (AA's `_get_accessible_categories`):
/// every one for managers, else one without groups or with one of the
/// viewer's. `$1`, `$2`: [`Access::params`].
pub(crate) const CATEGORY_SEEN: &str = "($2 \
     OR NOT EXISTS (SELECT 1 FROM category_groups cg WHERE cg.category_id = c.id) \
     OR EXISTS (SELECT 1 FROM category_groups cg WHERE cg.category_id = c.id \
                AND cg.group_id = ANY(string_to_array($1, ',')::bigint[])))";

/// A doctrine `d` the viewer sees (AA's `_check_doc_access`): one in no
/// category, or in a category they see.
pub(crate) fn doctrine_seen() -> String {
    format!(
        "($2 OR NOT EXISTS (SELECT 1 FROM category_doctrines sd WHERE sd.doctrine_id = d.id) \
         OR EXISTS (SELECT 1 FROM category_doctrines sd JOIN categories c ON c.id = sd.category_id \
                    WHERE sd.doctrine_id = d.id AND {CATEGORY_SEEN}))"
    )
}

/// A fit `f` the viewer sees (AA's `_check_fit_access`): a public one (none
/// of its categories, its own or its doctrines', has groups), or one in a
/// category they see.
pub(crate) fn fit_seen() -> String {
    format!(
        "($2 OR NOT EXISTS (SELECT 1 FROM fit_categories sf \
                            JOIN category_groups cg ON cg.category_id = sf.category_id \
                            WHERE sf.fit_id = f.id) \
         OR EXISTS (SELECT 1 FROM fit_categories sf JOIN categories c ON c.id = sf.category_id \
                    WHERE sf.fit_id = f.id AND {CATEGORY_SEEN}))"
    )
}

// ---- helpers ------------------------------------------------------------------

pub(crate) fn failed(what: &str, err: impl std::fmt::Debug) -> PageError {
    PageError::Failed(format!("{what}: {err:?}"))
}

pub(crate) fn query(sql: &str, params: &[Db]) -> Result<Vec<Vec<Db>>, PageError> {
    storage::query(sql, params)
        .map(|r| r.rows)
        .map_err(|e| failed("reading", e))
}

pub(crate) fn execute(what: &str, sql: &str, params: &[Db]) -> Result<u64, PageError> {
    storage::execute(sql, params).map_err(|e| failed(what, e))
}

pub(crate) fn int(row: &[Db], i: usize) -> i64 {
    row.get(i).and_then(Db::as_integer).unwrap_or_default()
}

pub(crate) fn opt_int(row: &[Db], i: usize) -> Option<i64> {
    row.get(i).and_then(Db::as_integer)
}

pub(crate) fn text(row: &[Db], i: usize) -> String {
    row.get(i)
        .and_then(Db::as_text)
        .unwrap_or_default()
        .to_owned()
}

/// Ids for `string_to_array($n, ',')::bigint[]`.
pub(crate) fn id_list(ids: &[i64]) -> String {
    ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",")
}

/// A positive id from a path segment or a posted value.
fn number(text: &str) -> Result<i64, PageError> {
    text.parse::<i64>()
        .ok()
        .filter(|n| *n > 0)
        .ok_or(PageError::NotFound)
}

/// At most `max` bytes of `text`, cut at a character, with "…" if cut
/// (the host takes 2 KiB per piece of text).
pub(crate) fn clip(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut end = max.saturating_sub('…'.len_utf8());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", text[..end].trim_end())
}

/// The categories of each of `ids` (fits or doctrines, by `sql`, which
/// selects owner id and category id and name for `$3`) the viewer sees,
/// as "A, B".
pub(crate) fn category_names(
    access: &Access,
    sql: &str,
    ids: &[i64],
) -> Result<std::collections::BTreeMap<i64, String>, PageError> {
    let mut names: std::collections::BTreeMap<i64, Vec<String>> = Default::default();
    if ids.is_empty() {
        return Ok(Default::default());
    }
    for r in query(sql, &access.with(vec![id_list(ids).into()]))? {
        names.entry(int(&r, 0)).or_default().push(text(&r, 1));
    }
    Ok(names
        .into_iter()
        .map(|(id, list)| (id, clip(&list.join(", "), 1000)))
        .collect())
}

/// A page's own primary button, for managers: Edit on a record, New
/// doctrine and New category on their lists. Tether draws the views and,
/// elsewhere, the manifest's New fit.
pub(crate) fn primary(page: Page, access: &Access, label: &str, path: &str) -> Page {
    if access.manage {
        page.button(label, path)
    } else {
        page
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clip_cuts_at_a_character() {
        assert_eq!(clip("short", 10), "short");
        assert_eq!(clip("ééééé", 7), "éé…");
        assert!(clip(&"x".repeat(3000), 2000).len() <= 2000);
    }
}
