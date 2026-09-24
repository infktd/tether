//! Checks on plugin pages before they reach a template.
//!
//! Escaping is the templates' job (askama escapes everything). This module
//! rejects pages that are malformed, oversized, or carry links that could
//! point outside the plugin.

use crate::host::{Page, Section, Value};

pub const MAX_SECTIONS: usize = 40;
pub const MAX_TABS: usize = 10;
pub const MAX_STATS: usize = 8;
pub const MAX_COLUMNS: usize = 20;
pub const MAX_ROWS: usize = 500;
pub const MAX_FIELDS: usize = 40;
pub const MAX_TEXT: usize = 2 * 1024;
pub const MAX_LINK_PATH: usize = 200;
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

struct Budget {
    bytes: usize,
    values: usize,
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
    };
    if page.title.trim().is_empty() {
        return Err(problem("the page title is empty"));
    }
    budget.text("the page title", &page.title)?;
    if let Some(description) = &page.description {
        budget.text("the page description", description)?;
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
    Ok(())
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
    }
    Ok(())
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
        Value::Time(time) => {
            if time.len() <= 40 && chrono::DateTime::parse_from_rfc3339(time).is_ok() {
                budget.bytes(time.len())
            } else {
                Err(problem(format!(
                    "{:?} isn't an RFC 3339 time",
                    printable_prefix(time)
                )))
            }
        }
        Value::Badge(badge) => budget.text("a badge", &badge.label),
        Value::Link(link) => {
            budget.text("a link label", &link.label)?;
            check_link_path(&link.path)?;
            budget.bytes(link.path.len())
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
    use crate::host::{Column, Link, Stat, Table};

    fn page() -> Page {
        Page {
            title: "Test".to_owned(),
            description: None,
            sections: Vec::new(),
            tabs: Vec::new(),
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
                path: "https://evil.example".to_owned(),
            }),
            caption: None,
        }]));
        assert!(check(&p).is_err());
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
