//! Checks on plugin pages before they reach a template.
//!
//! Escaping is the templates' job (askama escapes everything). This module
//! rejects pages that are malformed, oversized, or carry links that could
//! point outside the plugin.

use std::collections::BTreeSet;

use crate::host::{Action, Entity, FieldKind, Form, Profile, Progress};
use crate::host::{Page, Section, Value};

pub const MAX_SECTIONS: usize = 40;
pub const MAX_TABS: usize = 10;
pub const MAX_STATS: usize = 8;
pub const MAX_COLUMNS: usize = 20;
pub const MAX_ROWS: usize = 500;
pub const MAX_FIELDS: usize = 40;
pub const MAX_TEXT: usize = 2 * 1024;
pub const MAX_LINK_PATH: usize = 200;
/// Links beside a page's title.
pub const MAX_PAGE_LINKS: usize = 8;
/// A code block's text (a fitting with its cargo runs past [`MAX_TEXT`]).
pub const MAX_CODE_TEXT: usize = 16 * 1024;
/// Badges after a profile's name.
pub const MAX_PROFILE_BADGES: usize = 8;
/// Cards in one grid.
pub const MAX_CARDS: usize = 100;
/// How often a page may ask to be reloaded, in seconds: what it asks for
/// is brought into this range.
pub const MIN_REFRESH_SECONDS: u32 = 5;
pub const MAX_REFRESH_SECONDS: u32 = 300;
/// Everything on a page together: all text, link paths and times, plus
/// [`VALUE_COST`] per value.
pub const MAX_PAGE_BYTES: usize = 1024 * 1024;
/// Values (stats, table cells, card fields) on a page, across tables and
/// tabs.
pub const MAX_VALUES: usize = 10_000;
/// What each value costs against the page budget, whatever its type.
pub const VALUE_COST: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct PageProblem(pub String);

fn problem(text: impl Into<String>) -> PageProblem {
    PageProblem(text.into())
}

/// Fields per form.
pub const MAX_FIELDS_PER_FORM: usize = 30;
/// Longest text a text field or textarea may take.
pub const MAX_FIELD_LENGTH: u32 = 10_000;
/// Options in a select.
pub const MAX_OPTIONS: usize = 100;

/// Buttons in one `actions` value.
pub const MAX_ACTIONS: usize = 4;
/// Hidden values one action posts.
pub const MAX_ACTION_FIELDS: usize = 10;

struct Budget {
    bytes: usize,
    values: usize,
    form_ids: BTreeSet<String>,
    /// The forms actions post as: none may be a form's id.
    action_forms: BTreeSet<String>,
}

impl Budget {
    fn bytes(&mut self, n: usize) -> Result<(), PageProblem> {
        self.bytes += n;
        if self.bytes > MAX_PAGE_BYTES {
            return Err(problem(format!(
                "the page is bigger than {MAX_PAGE_BYTES} bytes"
            )));
        }
        Ok(())
    }

    fn text(&mut self, what: &str, value: &str) -> Result<(), PageProblem> {
        if value.len() > MAX_TEXT {
            return Err(problem(format!(
                "{what} is {} bytes; the limit is {MAX_TEXT}",
                value.len()
            )));
        }
        self.bytes(value.len())
    }

    fn value(&mut self) -> Result<(), PageProblem> {
        self.values += 1;
        if self.values > MAX_VALUES {
            return Err(problem(format!(
                "the page has more than {MAX_VALUES} values"
            )));
        }
        self.bytes(VALUE_COST)
    }
}

/// Checks a page. A problem is the plugin's bug; it is reported to admins
/// and the page isn't shown.
pub fn check(page: &Page) -> Result<(), PageProblem> {
    let mut budget = Budget {
        bytes: 0,
        values: 0,
        form_ids: BTreeSet::new(),
        action_forms: BTreeSet::new(),
    };
    if page.title.trim().is_empty() {
        return Err(problem("the page title is empty"));
    }
    budget.text("the page title", &page.title)?;
    if let Some(description) = &page.description {
        budget.text("the page description", description)?;
    }
    if page.links.len() > MAX_PAGE_LINKS {
        return Err(problem(format!(
            "{} page links; the limit is {MAX_PAGE_LINKS}",
            page.links.len()
        )));
    }
    for link in &page.links {
        budget.value()?;
        budget.text("a page link label", &link.label)?;
        check_link_path(&link.path)?;
        budget.bytes(link.path.len())?;
    }
    if page.tabs.len() > MAX_TABS {
        return Err(problem(format!(
            "{} tabs; the limit is {MAX_TABS}",
            page.tabs.len()
        )));
    }
    let sections = page.sections.len() + page.tabs.iter().map(|t| t.sections.len()).sum::<usize>();
    if sections > MAX_SECTIONS {
        return Err(problem(format!(
            "{sections} sections; the limit is {MAX_SECTIONS}"
        )));
    }
    for section in &page.sections {
        check_section(section, &mut budget)?;
    }
    for tab in &page.tabs {
        budget.text("a tab label", &tab.label)?;
        for section in &tab.sections {
            check_section(section, &mut budget)?;
        }
    }
    if let Some(both) = budget.action_forms.intersection(&budget.form_ids).next() {
        return Err(problem(format!(
            "{both:?} is both a form and an action's form"
        )));
    }
    Ok(())
}

fn check_action(action: &Action, budget: &mut Budget) -> Result<(), PageProblem> {
    budget.text("an action label", &action.label)?;
    check_form_name("an action's form", &action.form)?;
    budget.action_forms.insert(action.form.clone());
    if action.fields.len() > MAX_ACTION_FIELDS {
        return Err(problem(format!(
            "an action has {} fields; the limit is {MAX_ACTION_FIELDS}",
            action.fields.len()
        )));
    }
    let mut names = BTreeSet::new();
    for (name, value) in &action.fields {
        check_form_name("an action's field name", name)?;
        if !names.insert(name.as_str()) {
            return Err(problem(format!("an action has two fields called {name:?}")));
        }
        // Browsers rewrite line breaks and NULs in hidden fields, so such a
        // value could never post back as drawn.
        if value.chars().any(char::is_control) {
            return Err(problem(format!(
                "an action's field {name:?} has a control character"
            )));
        }
        budget.text("an action's field", value)?;
    }
    if let Some(confirm) = &action.confirm {
        budget.text("an action's confirmation", confirm)?;
    }
    Ok(())
}

/// Every value on a page, in its sections and tabs.
fn page_values(page: &Page) -> impl Iterator<Item = &Value> {
    page.sections
        .iter()
        .chain(page.tabs.iter().flat_map(|t| t.sections.iter()))
        .flat_map(|section| -> Box<dyn Iterator<Item = &Value> + '_> {
            match section {
                Section::Stats(stats) => Box::new(stats.iter().map(|s| &s.value)),
                Section::Table(table) => Box::new(table.rows.iter().flatten()),
                Section::Card(card) => Box::new(card.fields.iter().map(|(_, v)| v)),
                Section::Profile(profile) => Box::new(profile.facts.iter().map(|(_, v)| v)),
                Section::Cards(grid) => Box::new(
                    grid.items
                        .iter()
                        .flat_map(|card| card.profile.facts.iter().map(|(_, v)| v)),
                ),
                Section::Text(_) | Section::Form(_) | Section::Code(_) => {
                    Box::new(std::iter::empty())
                }
            }
        })
}

/// The action on a page that posts as `form` with exactly these fields
/// (in any order), if the page has one: a posted action must be one the
/// page offered.
pub fn find_action<'p>(
    page: &'p Page,
    form: &str,
    posted: &[(String, String)],
) -> Option<&'p Action> {
    let names: BTreeSet<&str> = posted.iter().map(|(name, _)| name.as_str()).collect();
    if names.len() != posted.len() {
        return None;
    }
    let same = |action: &Action| {
        action.form == form
            && action.fields.len() == posted.len()
            && posted.iter().all(|pair| action.fields.contains(pair))
    };
    page_values(page)
        .flat_map(|value| match value {
            Value::Action(action) => std::slice::from_ref(action),
            Value::Actions(actions) => actions.as_slice(),
            _ => &[],
        })
        .find(|action| same(action))
}

fn check_section(section: &Section, budget: &mut Budget) -> Result<(), PageProblem> {
    match section {
        Section::Stats(stats) => {
            if stats.len() > MAX_STATS {
                return Err(problem(format!(
                    "{} stats in one row; the limit is {MAX_STATS}",
                    stats.len()
                )));
            }
            for stat in stats {
                budget.text("a stat label", &stat.label)?;
                check_value(&stat.value, budget)?;
                if let Some(caption) = &stat.caption {
                    budget.text("a stat caption", caption)?;
                }
            }
        }
        Section::Table(table) => {
            if let Some(title) = &table.title {
                budget.text("a table title", title)?;
            }
            if let Some(empty) = &table.empty {
                budget.text("a table's empty text", empty)?;
            }
            if table.columns.is_empty() || table.columns.len() > MAX_COLUMNS {
                return Err(problem(format!(
                    "a table has {} columns; between 1 and {MAX_COLUMNS} are allowed",
                    table.columns.len()
                )));
            }
            if table.rows.len() > MAX_ROWS {
                return Err(problem(format!(
                    "a table has {} rows; the limit is {MAX_ROWS}",
                    table.rows.len()
                )));
            }
            for column in &table.columns {
                budget.text("a column label", &column.label)?;
            }
            for (i, row) in table.rows.iter().enumerate() {
                if row.len() != table.columns.len() {
                    return Err(problem(format!(
                        "row {i} of a table has {} cells for {} columns",
                        row.len(),
                        table.columns.len()
                    )));
                }
                for value in row {
                    check_value(value, budget)?;
                }
            }
        }
        Section::Card(card) => {
            budget.text("a card title", &card.title)?;
            if let Some(description) = &card.description {
                budget.text("a card description", description)?;
            }
            if card.fields.len() > MAX_FIELDS {
                return Err(problem(format!(
                    "a card has {} fields; the limit is {MAX_FIELDS}",
                    card.fields.len()
                )));
            }
            for (label, value) in &card.fields {
                budget.text("a card field label", label)?;
                check_value(value, budget)?;
            }
        }
        Section::Text(text) => budget.text("a paragraph", text)?,
        Section::Form(form) => check_form(form, budget)?,
        Section::Code(code) => {
            if let Some(title) = &code.title {
                budget.text("a code block title", title)?;
            }
            if let Some(label) = &code.copy_label {
                budget.text("a copy label", label)?;
            }
            if code.text.len() > MAX_CODE_TEXT {
                return Err(problem(format!(
                    "a code block is {} bytes; the limit is {MAX_CODE_TEXT}",
                    code.text.len()
                )));
            }
            budget.value()?;
            budget.bytes(code.text.len())?;
        }
        Section::Profile(profile) => check_profile(profile, budget)?,
        Section::Cards(grid) => {
            if grid.items.len() > MAX_CARDS {
                return Err(problem(format!(
                    "a card grid has {} cards; the limit is {MAX_CARDS}",
                    grid.items.len()
                )));
            }
            for card in &grid.items {
                check_profile(&card.profile, budget)?;
                if let Some(path) = &card.link {
                    check_link_path(path)?;
                    budget.bytes(path.len())?;
                }
            }
        }
    }
    Ok(())
}

fn check_profile(profile: &Profile, budget: &mut Budget) -> Result<(), PageProblem> {
    check_entity(&profile.subject, budget)?;
    for entity in [&profile.corporation, &profile.alliance]
        .into_iter()
        .flatten()
    {
        check_entity(entity, budget)?;
    }
    if let Some(subtitle) = &profile.subtitle {
        budget.text("a profile subtitle", subtitle)?;
    }
    if profile.facts.len() > MAX_FIELDS {
        return Err(problem(format!(
            "a profile has {} facts; the limit is {MAX_FIELDS}",
            profile.facts.len()
        )));
    }
    for (label, value) in &profile.facts {
        budget.text("a profile fact label", label)?;
        check_value(value, budget)?;
    }
    if profile.badges.len() > MAX_PROFILE_BADGES {
        return Err(problem(format!(
            "a profile has {} badges; the limit is {MAX_PROFILE_BADGES}",
            profile.badges.len()
        )));
    }
    for badge in &profile.badges {
        budget.value()?;
        budget.text("a badge", &badge.label)?;
    }
    Ok(())
}

fn check_entity(entity: &Entity, budget: &mut Budget) -> Result<(), PageProblem> {
    budget.value()?;
    budget.text("an entity name", &entity.name)
}

/// An RFC 3339 instant, as `time`, `countdown` and `progress` take them.
fn check_time(time: &str, budget: &mut Budget) -> Result<(), PageProblem> {
    if time.len() <= 40 && chrono::DateTime::parse_from_rfc3339(time).is_ok() {
        budget.bytes(time.len())
    } else {
        Err(problem(format!(
            "{:?} isn't an RFC 3339 time",
            printable_prefix(time)
        )))
    }
}

fn check_progress(progress: &Progress, budget: &mut Budget) -> Result<(), PageProblem> {
    if !(progress.fraction.is_finite() && (0.0..=1.0).contains(&progress.fraction)) {
        return Err(problem("a progress fraction isn't between 0 and 1"));
    }
    if let Some(label) = &progress.label {
        budget.text("a progress label", label)?;
    }
    match (&progress.from, &progress.to) {
        (None, None) => Ok(()),
        (Some(from), Some(to)) => {
            check_time(from, budget)?;
            check_time(to, budget)?;
            let at = |t: &str| chrono::DateTime::parse_from_rfc3339(t).ok();
            if at(to) > at(from) {
                Ok(())
            } else {
                Err(problem("a progress bar's `to` isn't after its `from`"))
            }
        }
        _ => Err(problem("a progress bar has only one of `from` and `to`")),
    }
}

/// How often the host reloads a page's content, in seconds: what the page
/// asks for, brought into [`MIN_REFRESH_SECONDS`] to
/// [`MAX_REFRESH_SECONDS`]. Never for a page with a form, whose fields
/// would be reset under someone typing.
pub fn refresh_seconds(page: &Page) -> Option<u32> {
    let has_form = page
        .sections
        .iter()
        .chain(page.tabs.iter().flat_map(|t| t.sections.iter()))
        .any(|s| matches!(s, Section::Form(_)));
    if has_form {
        return None;
    }
    page.refresh_seconds
        .map(|s| s.clamp(MIN_REFRESH_SECONDS, MAX_REFRESH_SECONDS))
}

/// Form ids and field names: `[a-z0-9_]`, 1 to 40, starting with a letter
/// (the host's own form fields start with `_`).
fn check_form_name(what: &str, name: &str) -> Result<(), PageProblem> {
    let fine = (1..=40).contains(&name.len())
        && name.as_bytes()[0].is_ascii_lowercase()
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
    if fine {
        Ok(())
    } else {
        Err(problem(format!(
            "{what} {:?} isn't 1 to 40 lowercase letters, digits and _, starting with a letter",
            printable_prefix(name)
        )))
    }
}

fn check_form(form: &Form, budget: &mut Budget) -> Result<(), PageProblem> {
    check_form_name("a form id", &form.id)?;
    if !budget.form_ids.insert(form.id.clone()) {
        return Err(problem(format!("two forms are called {:?}", form.id)));
    }
    if let Some(title) = &form.title {
        budget.text("a form title", title)?;
    }
    if let Some(description) = &form.description {
        budget.text("a form description", description)?;
    }
    budget.text("a submit label", &form.submit_label)?;
    if form.fields.is_empty() || form.fields.len() > MAX_FIELDS_PER_FORM {
        return Err(problem(format!(
            "a form has {} fields; between 1 and {MAX_FIELDS_PER_FORM} are allowed",
            form.fields.len()
        )));
    }
    let mut names = BTreeSet::new();
    for field in &form.fields {
        check_form_name("a field name", &field.name)?;
        if !names.insert(field.name.as_str()) {
            return Err(problem(format!("two fields are called {:?}", field.name)));
        }
        budget.value()?;
        budget.text("a field label", &field.label)?;
        if let Some(help) = &field.help {
            budget.text("a field's help", help)?;
        }
        match &field.kind {
            FieldKind::Text(input) | FieldKind::Textarea(input) => {
                if !(1..=MAX_FIELD_LENGTH).contains(&input.max_length) {
                    return Err(problem(format!(
                        "field {:?} has a max length outside 1 to {MAX_FIELD_LENGTH}",
                        field.name
                    )));
                }
                if let Some(value) = &input.value {
                    budget.bytes(value.len())?;
                }
                if let Some(placeholder) = &input.placeholder {
                    budget.text("a placeholder", placeholder)?;
                }
            }
            FieldKind::Number(input) => {
                let finite = |n: Option<f64>| n.is_none_or(f64::is_finite);
                if !finite(input.value) || !finite(input.min) || !finite(input.max) {
                    return Err(problem(format!(
                        "field {:?} has a number that isn't finite",
                        field.name
                    )));
                }
                if let (Some(min), Some(max)) = (input.min, input.max)
                    && min > max
                {
                    return Err(problem(format!("field {:?} has min above max", field.name)));
                }
            }
            FieldKind::Select(input) => {
                if input.options.is_empty() || input.options.len() > MAX_OPTIONS {
                    return Err(problem(format!(
                        "field {:?} has {} options; between 1 and {MAX_OPTIONS} are allowed",
                        field.name,
                        input.options.len()
                    )));
                }
                let mut values = BTreeSet::new();
                for choice in &input.options {
                    budget.value()?;
                    budget.text("an option", &choice.label)?;
                    budget.text("an option value", &choice.value)?;
                    if !values.insert(choice.value.as_str()) {
                        return Err(problem(format!(
                            "field {:?} has two options with the same value",
                            field.name
                        )));
                    }
                }
                if let Some(value) = &input.value
                    && !values.contains(value.as_str())
                {
                    return Err(problem(format!(
                        "field {:?} starts on a value that isn't one of its options",
                        field.name
                    )));
                }
            }
            FieldKind::Checkbox(_) => {}
        }
    }
    Ok(())
}

/// The form called `id` on a page, in its sections or tabs.
pub fn find_form<'p>(page: &'p Page, id: &str) -> Option<&'p Form> {
    page.sections
        .iter()
        .chain(page.tabs.iter().flat_map(|t| t.sections.iter()))
        .find_map(|section| match section {
            Section::Form(form) if form.id == id => Some(form),
            _ => None,
        })
}

/// Checks posted values against a form's fields: every field known, none
/// twice, required ones filled, texts within their length, numbers in
/// range, selects one of their options. Returns one value per field, in
/// the form's order (checkboxes `true`/`false`, empty optional fields
/// empty). The error is for the person who posted it.
pub fn check_submission(
    form: &Form,
    posted: &[(String, String)],
) -> Result<Vec<(String, String)>, String> {
    let mut seen = BTreeSet::new();
    for (name, _) in posted {
        if !form.fields.iter().any(|f| &f.name == name) {
            return Err("The form has a field it didn't ask for.".to_owned());
        }
        if !seen.insert(name.as_str()) {
            return Err("The form has a field twice.".to_owned());
        }
    }
    let mut values = Vec::with_capacity(form.fields.len());
    for field in &form.fields {
        let raw = posted
            .iter()
            .find(|(name, _)| name == &field.name)
            .map(|(_, value)| value.as_str());
        let label = &field.label;
        let value = match &field.kind {
            FieldKind::Checkbox(_) => match raw {
                None | Some("") if field.required => {
                    return Err(format!("{label} must be ticked."));
                }
                None | Some("") => "false".to_owned(),
                Some("on" | "true") => "true".to_owned(),
                Some(_) => return Err(format!("{label}: tick it or leave it.")),
            },
            FieldKind::Text(input) | FieldKind::Textarea(input) => {
                let value = raw.unwrap_or("");
                if field.required && value.trim().is_empty() {
                    return Err(format!("{label} is required."));
                }
                if value.chars().count() > input.max_length as usize {
                    return Err(format!(
                        "{label} is longer than {} characters.",
                        input.max_length
                    ));
                }
                value.to_owned()
            }
            FieldKind::Number(input) => {
                let value = raw.unwrap_or("").trim();
                if value.is_empty() {
                    if field.required {
                        return Err(format!("{label} is required."));
                    }
                    String::new()
                } else {
                    let n: f64 = value
                        .parse()
                        .ok()
                        .filter(|n: &f64| n.is_finite())
                        .ok_or_else(|| format!("{label} must be a number."))?;
                    if input.integer && n.fract() != 0.0 {
                        return Err(format!("{label} must be a whole number."));
                    }
                    if input.min.is_some_and(|min| n < min) || input.max.is_some_and(|max| n > max)
                    {
                        return Err(format!("{label} is out of range."));
                    }
                    // One spelling, whatever was typed (`1e2`, `+3`, `3.0`),
                    // so the plugin reads what was checked.
                    if input.integer {
                        if n.abs() > 9_007_199_254_740_992.0 {
                            return Err(format!("{label} is out of range."));
                        }
                        (n as i64).to_string()
                    } else {
                        n.to_string()
                    }
                }
            }
            FieldKind::Select(input) => {
                let value = raw.unwrap_or("");
                if value.is_empty() {
                    if field.required {
                        return Err(format!("{label} is required."));
                    }
                    String::new()
                } else if input.options.iter().any(|c| c.value == value) {
                    value.to_owned()
                } else {
                    return Err(format!("{label}: choose one of the options."));
                }
            }
        };
        values.push((field.name.clone(), value));
    }
    Ok(values)
}

fn check_value(value: &Value, budget: &mut Budget) -> Result<(), PageProblem> {
    budget.value()?;
    match value {
        Value::Text(text) => budget.text("a value", text),
        Value::Number(_) => Ok(()),
        Value::Isk(isk) => {
            if isk.is_finite() {
                Ok(())
            } else {
                Err(problem("an ISK amount isn't a finite number"))
            }
        }
        Value::Time(time) | Value::Countdown(time) => check_time(time, budget),
        Value::Badge(badge) => budget.text("a badge", &badge.label),
        Value::Link(link) => {
            budget.text("a link label", &link.label)?;
            check_link_path(&link.path)?;
            budget.bytes(link.path.len())
        }
        Value::Entity(entity) => budget.text("an entity name", &entity.name),
        // Only the plugin's own pages: the host writes the site's address
        // before it, so a page can't hand out any other address.
        Value::Share(path) => {
            check_link_path(path)?;
            budget.bytes(path.len())
        }
        Value::Progress(progress) => check_progress(progress, budget),
        Value::Action(action) => check_action(action, budget),
        Value::Actions(actions) => {
            if actions.is_empty() || actions.len() > MAX_ACTIONS {
                return Err(problem(format!(
                    "{} actions in one value; between 1 and {MAX_ACTIONS} are allowed",
                    actions.len()
                )));
            }
            for action in actions {
                budget.value()?;
                check_action(action, budget)?;
            }
            Ok(())
        }
    }
}

/// Links stay inside the plugin: path segments of ASCII letters,
/// digits, `-`, `_` and `.`, no `..`, no scheme, no leading slash.
pub fn check_link_path(path: &str) -> Result<(), PageProblem> {
    let fine = path.len() <= MAX_LINK_PATH
        && !path.starts_with('/')
        && path.split('/').all(|segment| {
            !segment.is_empty()
                && segment != "."
                && segment != ".."
                && segment
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        });
    if fine || path.is_empty() {
        Ok(())
    } else {
        Err(problem(format!(
            "{:?} isn't a path to one of the plugin's pages",
            printable_prefix(path)
        )))
    }
}

fn printable_prefix(text: &str) -> String {
    text.chars()
        .take(60)
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::{Card, Column, Link, Stat, Table, Tone};

    fn card(value: Value) -> Section {
        Section::Card(Card {
            title: "c".to_owned(),
            description: None,
            fields: vec![("v".to_owned(), value)],
        })
    }

    fn progress(from: Option<&str>, to: Option<&str>, fraction: f64) -> Value {
        Value::Progress(Progress {
            fraction,
            from: from.map(str::to_owned),
            to: to.map(str::to_owned),
            label: None,
        })
    }

    fn act(form: &str, fields: &[(&str, &str)]) -> Action {
        Action {
            label: "Go".to_owned(),
            form: form.to_owned(),
            fields: fields
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
            tone: Tone::Neutral,
            confirm: None,
        }
    }

    #[test]
    fn progress_bars_are_checked() {
        let at = |from, to, fraction| {
            let mut p = page();
            p.sections.push(card(progress(from, to, fraction)));
            check(&p)
        };
        let (a, b) = ("2026-09-24T18:00:00Z", "2026-09-25T18:00:00Z");
        assert_eq!(at(None, None, 0.5), Ok(()));
        assert_eq!(at(Some(a), Some(b), 0.0), Ok(()));
        for (from, to, fraction) in [
            (None, None, 1.5),
            (None, None, -0.1),
            (None, None, f64::NAN),
            (Some(a), None, 0.0),
            (None, Some(b), 0.0),
            (Some(b), Some(a), 0.0),
            (Some(a), Some(a), 0.0),
            (Some(a), Some("tomorrow"), 0.0),
        ] {
            assert!(
                at(from, to, fraction).is_err(),
                "{from:?} {to:?} {fraction}"
            );
        }
    }

    #[test]
    fn refresh_is_clamped_and_never_under_a_form() {
        let mut p = page();
        assert_eq!(refresh_seconds(&p), None);
        for (asked, got) in [(0, 5), (1, 5), (30, 30), (86_400, 300)] {
            p.refresh_seconds = Some(asked);
            assert_eq!(refresh_seconds(&p), Some(got));
        }
        p.tabs.push(crate::host::Tab {
            label: "t".to_owned(),
            sections: vec![Section::Form(Form {
                id: "f".to_owned(),
                title: None,
                description: None,
                fields: Vec::new(),
                submit_label: "Go".to_owned(),
            })],
        });
        assert_eq!(refresh_seconds(&p), None);
    }

    #[test]
    fn actions_are_checked_and_found_exactly() {
        let mut p = page();
        p.sections.push(card(Value::Actions(vec![
            act("decide", &[("id", "7"), ("verdict", "yes")]),
            act("decide", &[("id", "7"), ("verdict", "no")]),
        ])));
        assert_eq!(check(&p), Ok(()));
        let posted = |pairs: &[(&str, &str)]| -> Vec<(String, String)> {
            pairs
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect()
        };
        let found = find_action(&p, "decide", &posted(&[("verdict", "no"), ("id", "7")]));
        assert_eq!(found.map(|a| a.fields[1].1.as_str()), Some("no"));
        for bad in [
            posted(&[("id", "8"), ("verdict", "no")]),
            posted(&[("id", "7")]),
            posted(&[("id", "7"), ("verdict", "no"), ("x", "1")]),
            posted(&[("id", "7"), ("id", "7")]),
        ] {
            assert!(find_action(&p, "decide", &bad).is_none(), "{bad:?}");
        }
        assert!(find_action(&p, "other", &posted(&[("id", "7"), ("verdict", "no")])).is_none());

        for bad in [
            vec![act("Bad Form", &[])],
            vec![act("ok", &[("_form", "x")])],
            vec![act("ok", &[("id", "1"), ("id", "2")])],
            vec![act("ok", &[("note", "two\nlines")])],
            (0..=MAX_ACTIONS).map(|_| act("ok", &[])).collect(),
            Vec::new(),
        ] {
            let mut p = page();
            p.sections.push(card(Value::Actions(bad)));
            assert!(check(&p).is_err());
        }
    }

    fn page() -> Page {
        Page {
            title: "Test".to_owned(),
            description: None,
            sections: Vec::new(),
            tabs: Vec::new(),
            links: Vec::new(),
            refresh_seconds: None,
        }
    }

    fn table(rows: Vec<Vec<Value>>) -> Section {
        Section::Table(Table {
            title: None,
            columns: vec![
                Column {
                    label: "a".to_owned(),
                    numeric: false,
                },
                Column {
                    label: "b".to_owned(),
                    numeric: true,
                },
            ],
            rows,
            empty: None,
        })
    }

    #[test]
    fn a_well_formed_page_passes() {
        let mut p = page();
        p.sections.push(Section::Stats(vec![Stat {
            label: "x".to_owned(),
            value: Value::Number(1),
            caption: None,
        }]));
        p.sections.push(table(vec![vec![
            Value::Text("a".to_owned()),
            Value::Time("2026-09-24T18:00:00Z".to_owned()),
        ]]));
        assert_eq!(check(&p), Ok(()));
    }

    #[test]
    fn malformed_pages_fail() {
        let mut empty_title = page();
        empty_title.title = " ".to_owned();
        assert!(check(&empty_title).is_err());

        let mut ragged = page();
        ragged.sections.push(table(vec![vec![Value::Number(1)]]));
        assert!(
            check(&ragged)
                .unwrap_err()
                .0
                .contains("1 cells for 2 columns")
        );

        let mut too_long = page();
        too_long.sections.push(table(
            (0..=MAX_ROWS)
                .map(|_| vec![Value::Number(1), Value::Number(2)])
                .collect(),
        ));
        assert!(check(&too_long).is_err());

        let mut nan = page();
        nan.sections.push(Section::Stats(vec![Stat {
            label: "t".to_owned(),
            value: Value::Isk(f64::NAN),
            caption: None,
        }]));
        assert!(check(&nan).is_err());
    }

    fn grid_card(link: Option<&str>, facts: Vec<(String, Value)>) -> crate::host::ProfileCard {
        crate::host::ProfileCard {
            profile: Profile {
                subject: Entity {
                    kind: crate::host::EntityKind::Character,
                    id: 90_000_001,
                    name: "Example Pilot".to_owned(),
                },
                subtitle: None,
                corporation: None,
                alliance: None,
                facts,
                badges: Vec::new(),
            },
            link: link.map(str::to_owned),
        }
    }

    #[test]
    fn card_grids_are_checked_like_profiles() {
        let grid = |items| {
            let mut p = page();
            p.sections.push(Section::Cards(crate::host::CardGrid {
                items,
                register: true,
            }));
            p
        };
        assert_eq!(
            check(&grid(vec![grid_card(Some("character/1"), Vec::new())])),
            Ok(())
        );
        // Links stay inside the plugin.
        for bad in ["https://evil.example", "/admin", "../x"] {
            assert!(check(&grid(vec![grid_card(Some(bad), Vec::new())])).is_err());
        }
        // Facts are checked as values, and their actions can be posted.
        assert!(
            check(&grid(vec![grid_card(
                None,
                vec![("t".to_owned(), Value::Time("soon".to_owned()))]
            )]))
            .is_err()
        );
        let with_action = grid(vec![grid_card(
            None,
            vec![(
                "Update".to_owned(),
                Value::Action(act("refresh", &[("character", "1")])),
            )],
        )]);
        assert_eq!(check(&with_action), Ok(()));
        assert!(
            find_action(
                &with_action,
                "refresh",
                &[("character".to_owned(), "1".to_owned())]
            )
            .is_some()
        );
        let too_many = grid(
            (0..=MAX_CARDS)
                .map(|_| grid_card(None, Vec::new()))
                .collect(),
        );
        assert!(check(&too_many).unwrap_err().0.contains("cards"));
    }

    #[test]
    fn links_stay_inside_the_plugin() {
        for ok in ["", "about", "moons/40161234", "a-b_c.d/e"] {
            assert_eq!(check_link_path(ok), Ok(()), "{ok}");
        }
        for bad in [
            "/admin",
            "//evil.example",
            "https://evil.example",
            "javascript:alert(1)",
            "../core",
            "a/../../b",
            "a//b",
            "a?b=c",
            "a#frag",
            "a b",
            "Ünïcode",
        ] {
            assert!(check_link_path(bad).is_err(), "{bad}");
        }
        let mut p = page();
        p.sections.push(Section::Stats(vec![Stat {
            label: "l".to_owned(),
            value: Value::Link(Link {
                label: "go".to_owned(),
                primary: false,
                path: "https://evil.example".to_owned(),
            }),
            caption: None,
        }]));
        assert!(check(&p).is_err());
    }

    #[test]
    fn links_to_share_are_the_plugins_own_pages() {
        let shared = |path: &str| {
            let mut p = page();
            p.sections.push(card(Value::Share(path.to_owned())));
            check(&p)
        };
        for ok in ["links/0f3a/add", "request/ABC123", ""] {
            assert_eq!(shared(ok), Ok(()), "{ok}");
        }
        for bad in [
            "https://evil.example/x",
            "//evil.example",
            "/admin",
            "../core",
            "a?b=c",
            "javascript:alert(1)",
            "a#b",
            "a%2e%2e",
            "a\\b",
        ] {
            assert!(shared(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn times_are_real_rfc3339_instants() {
        for (time, ok) in [
            ("2026-09-24T18:00:00Z", true),
            ("2026-09-24T18:00:00.123Z", true),
            ("2026-09-24T18:00:00+02:00", true),
            ("2026-09-24", false),
            ("yesterday", false),
            // The right shape, but not a real instant.
            ("9999-99-99T99:99:99+99:99", false),
            ("2026-02-30T12:00:00Z", false),
        ] {
            let mut p = page();
            p.sections.push(Section::Stats(vec![Stat {
                label: "t".to_owned(),
                value: Value::Time(time.to_owned()),
                caption: None,
            }]));
            assert_eq!(check(&p).is_ok(), ok, "{time}");
        }
    }

    #[test]
    fn every_value_counts_against_the_page() {
        // Links with empty labels and long paths used to cost nothing.
        let path = "a".repeat(MAX_LINK_PATH);
        let row = || {
            vec![Value::Link(Link {
                label: String::new(),
                primary: false,
                path: path.clone(),
            })]
        };
        let mut p = page();
        for _ in 0..30 {
            p.sections.push(Section::Table(Table {
                title: None,
                columns: vec![Column {
                    label: "l".to_owned(),
                    numeric: false,
                }],
                rows: (0..MAX_ROWS).map(|_| row()).collect(),
                empty: None,
            }));
        }
        let err = check(&p).unwrap_err();
        assert!(
            err.0.contains("bigger than") || err.0.contains("values"),
            "{err}"
        );
    }
}
