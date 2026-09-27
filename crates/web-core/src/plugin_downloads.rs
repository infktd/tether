//! Files apps offer for download (aa-memberaudit's data exports; approved
//! by Jay, 2026-09-27): an app with `downloads = true` hands over rows in a
//! job or submit, and the host writes the CSV itself (quoting cells and
//! defusing spreadsheet formulas), keeps it in Postgres, and serves it at
//! `/plugins/<id>/downloads/<name>` to holders of the app's permission
//! named with it, each download audited.

use std::future::Future;
use std::pin::Pin;
use std::sync::Weak;
use std::task::{Context, Poll};

use futures_core::Stream;
use tether_db::PgPool;
use tether_db::downloads::{self as db, Outcome};
use tether_plugins::services::{DownloadError, DownloadFile};

use crate::plugins::Plugins;

/// Downloads one app may have.
pub const MAX_DOWNLOADS: i64 = 20;
/// A download's size, at most (the CSV as served).
pub const MAX_BYTES: i64 = 50 * 1024 * 1024;
/// Rows one `append` may add.
pub const MAX_ROWS_PER_APPEND: usize = 5_000;
/// Appends one build may have (at 5,000 rows each, plenty).
const MAX_PARTS: i32 = 10_000;
const MAX_COLUMNS: usize = 60;
const MAX_CELL_CHARS: usize = 10_000;
const MAX_TITLE_CHARS: usize = 100;

fn invalid(why: impl Into<String>) -> DownloadError {
    DownloadError::Invalid(why.into())
}

fn unavailable(plugin: &str, err: &sqlx::Error) -> DownloadError {
    tracing::error!(plugin, error = %err, "plugin downloads");
    DownloadError::Unavailable
}

fn approved(plugins: &Weak<Plugins>, plugin: &str) -> Result<(), DownloadError> {
    let approved = plugins
        .upgrade()
        .and_then(|p| p.running(plugin))
        .is_some_and(|r| r.manifest.capabilities.downloads);
    if approved {
        Ok(())
    } else {
        Err(invalid("downloads need `downloads = true` in plugin.toml"))
    }
}

fn valid_name(name: &str) -> bool {
    (1..=50).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Characters a spreadsheet may take as starting a formula (OWASP's CSV
/// injection list, with the full-width forms some locales convert).
const FORMULA_STARTS: [char; 10] = [
    '=', '+', '-', '@', '\t', '\r', '\u{FF1D}', '\u{FF0B}', '\u{FF0D}', '\u{FF20}',
];

/// One cell as CSV: a formula a spreadsheet would run (`=`, `+`, `-`,
/// `@`, their full-width forms, a tab or carriage return first) gets a
/// leading `'`, unless it's a plain number; quoted when it holds a comma,
/// quote or line break, or starts or ends with a space.
pub fn csv_cell(cell: &str) -> String {
    let formula = cell.starts_with(FORMULA_STARTS) && cell.parse::<f64>().is_err();
    let cell = if formula {
        format!("'{cell}")
    } else {
        cell.to_owned()
    };
    if cell.contains([',', '"', '\n', '\r']) || cell.starts_with(' ') || cell.ends_with(' ') {
        format!("\"{}\"", cell.replace('"', "\"\""))
    } else {
        cell
    }
}

/// One CSV line, ending CRLF (RFC 4180).
pub fn csv_line(cells: &[String]) -> String {
    let mut line = cells
        .iter()
        .map(|c| csv_cell(c))
        .collect::<Vec<_>>()
        .join(",");
    line.push_str("\r\n");
    line
}

fn check_cells(cells: &[String]) -> Result<(), DownloadError> {
    if cells.iter().any(|c| c.chars().count() > MAX_CELL_CHARS) {
        return Err(invalid(format!(
            "a cell holds at most {MAX_CELL_CHARS} characters"
        )));
    }
    Ok(())
}

pub async fn begin(
    db_pool: &PgPool,
    plugins: &Weak<Plugins>,
    plugin: &str,
    name: &str,
    title: &str,
    permission: &str,
    header: &[String],
) -> Result<u32, DownloadError> {
    approved(plugins, plugin)?;
    if !valid_name(name) {
        return Err(invalid(
            "a download's name is 1 to 50 lowercase letters, digits and dashes",
        ));
    }
    let title = title.trim();
    if title.is_empty()
        || title.chars().count() > MAX_TITLE_CHARS
        || title.chars().any(char::is_control)
    {
        return Err(invalid(format!(
            "a download's title is 1 to {MAX_TITLE_CHARS} characters, on one line"
        )));
    }
    let declared = plugins
        .upgrade()
        .and_then(|p| p.running(plugin))
        .is_some_and(|r| r.manifest.permissions.contains_key(permission));
    if !declared {
        return Err(invalid(format!(
            "{permission:?} isn't one of this app's permissions"
        )));
    }
    if header.is_empty() || header.len() > MAX_COLUMNS {
        return Err(invalid(format!("1 to {MAX_COLUMNS} columns")));
    }
    check_cells(header)?;
    let columns = i32::try_from(header.len()).unwrap_or(i32::MAX);
    let started = db::begin(
        db_pool,
        plugin,
        name,
        title,
        permission,
        columns,
        &csv_line(header),
        MAX_DOWNLOADS,
    )
    .await
    .map_err(|e| unavailable(plugin, &e))?
    .ok_or_else(|| invalid(format!("at most {MAX_DOWNLOADS} downloads")))?;
    u32::try_from(started).map_err(|_| DownloadError::Unavailable)
}

fn refused(outcome: Outcome, name: &str, build: u32) -> Result<(), DownloadError> {
    match outcome {
        Outcome::Done => Ok(()),
        Outcome::Superseded => Err(DownloadError::Superseded),
        Outcome::TooLarge => Err(DownloadError::TooLarge),
        Outcome::TooManyParts => Err(invalid(format!(
            "at most {MAX_PARTS} appends a build: send more rows each time"
        ))),
        Outcome::NotBuilding => Err(invalid(format!(
            "no build {build} of {name:?} is under way: begin it first"
        ))),
    }
}

pub async fn append(
    db_pool: &PgPool,
    plugins: &Weak<Plugins>,
    plugin: &str,
    name: &str,
    build: u32,
    rows: &[Vec<String>],
) -> Result<(), DownloadError> {
    approved(plugins, plugin)?;
    if rows.len() > MAX_ROWS_PER_APPEND {
        return Err(invalid(format!(
            "at most {MAX_ROWS_PER_APPEND} rows a call"
        )));
    }
    let version = i32::try_from(build).map_err(|_| invalid("no such build"))?;
    let columns = match db::columns(db_pool, plugin, name, version)
        .await
        .map_err(|e| unavailable(plugin, &e))?
    {
        Ok(columns) => usize::try_from(columns).unwrap_or(0),
        Err(outcome) => return refused(outcome, name, build),
    };
    // Nothing to add: no empty part.
    if rows.is_empty() {
        return Ok(());
    }
    let mut csv = String::new();
    for row in rows {
        if row.len() != columns {
            return Err(invalid(format!(
                "each row has {columns} cells, as the header"
            )));
        }
        check_cells(row)?;
        csv.push_str(&csv_line(row));
        if i64::try_from(csv.len()).unwrap_or(i64::MAX) > MAX_BYTES {
            return Err(DownloadError::TooLarge);
        }
    }
    let count = i64::try_from(rows.len()).unwrap_or(i64::MAX);
    let outcome = db::append(
        db_pool, plugin, name, version, &csv, count, MAX_BYTES, MAX_PARTS,
    )
    .await
    .map_err(|e| unavailable(plugin, &e))?;
    refused(outcome, name, build)
}

pub async fn finish(
    db_pool: &PgPool,
    plugins: &Weak<Plugins>,
    plugin: &str,
    name: &str,
    build: u32,
) -> Result<(), DownloadError> {
    approved(plugins, plugin)?;
    let version = i32::try_from(build).map_err(|_| invalid("no such build"))?;
    let outcome = db::finish(db_pool, plugin, name, version)
        .await
        .map_err(|e| unavailable(plugin, &e))?;
    refused(outcome, name, build)
}

pub async fn files(db_pool: &PgPool, plugins: &Weak<Plugins>, plugin: &str) -> Vec<DownloadFile> {
    if approved(plugins, plugin).is_err() {
        return Vec::new();
    }
    match db::files(db_pool, plugin).await {
        Ok(files) => files
            .into_iter()
            .map(|f| DownloadFile {
                name: f.name,
                title: f.title,
                rows: u64::try_from(f.rows).unwrap_or(0),
                built_at: f.built_at.to_rfc3339(),
            })
            .collect(),
        Err(err) => {
            tracing::error!(plugin, error = %err, "listing plugin downloads");
            Vec::new()
        }
    }
}

/// Parts fetched per query while serving.
const PARTS_PER_FETCH: i64 = 16;

type Step = Pin<Box<dyn Future<Output = Option<(Result<String, std::io::Error>, Serving)>> + Send>>;

/// A finished file's parts, fetched a few at a time as the response is
/// sent, so a large file never sits whole in memory. Fails the response if
/// the file is replaced before it's all sent (its parts go), rather than
/// ending it short.
pub struct PartsStream {
    step: Option<Step>,
}

struct Serving {
    db: PgPool,
    plugin: String,
    name: String,
    version: i32,
    after: i32,
    sent: i64,
    bytes: i64,
    queued: std::collections::VecDeque<(i32, String)>,
}

impl PartsStream {
    /// `file`'s parts, as `db::files` gave it.
    pub fn new(db: PgPool, plugin: &str, file: &db::File) -> Self {
        let serving = Serving {
            db,
            plugin: plugin.to_owned(),
            name: file.name.clone(),
            version: file.version,
            after: -1,
            sent: 0,
            bytes: file.bytes,
            queued: std::collections::VecDeque::new(),
        };
        Self {
            step: Some(Box::pin(next_part(serving))),
        }
    }
}

impl Stream for PartsStream {
    type Item = Result<String, std::io::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let Some(step) = self.step.as_mut() else {
            return Poll::Ready(None);
        };
        match step.as_mut().poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(None) => {
                self.step = None;
                Poll::Ready(None)
            }
            Poll::Ready(Some((Err(err), _))) => {
                self.step = None;
                Poll::Ready(Some(Err(err)))
            }
            Poll::Ready(Some((part, serving))) => {
                self.step = Some(Box::pin(next_part(serving)));
                Poll::Ready(Some(part))
            }
        }
    }
}

async fn next_part(mut s: Serving) -> Option<(Result<String, std::io::Error>, Serving)> {
    if s.queued.is_empty() {
        match db::parts(
            &s.db,
            &s.plugin,
            &s.name,
            s.version,
            s.after,
            PARTS_PER_FETCH,
        )
        .await
        {
            Ok(parts) => s.queued.extend(parts),
            Err(err) => {
                tracing::error!(plugin = %s.plugin, error = %err, "serving a plugin download");
                return Some((Err(std::io::Error::other("the download failed")), s));
            }
        }
    }
    match s.queued.pop_front() {
        Some((seq, csv)) => {
            s.after = seq;
            s.sent = s
                .sent
                .saturating_add(i64::try_from(csv.len()).unwrap_or(i64::MAX));
            Some((Ok(csv), s))
        }
        None if s.sent < s.bytes => Some((
            Err(std::io::Error::other(
                "the file was replaced while downloading",
            )),
            s,
        )),
        None => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cells_are_quoted_and_formulas_defused() {
        assert_eq!(csv_cell("Jita"), "Jita");
        assert_eq!(csv_cell("a,b"), "\"a,b\"");
        assert_eq!(csv_cell("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(csv_cell("two\nlines"), "\"two\nlines\"");
        assert_eq!(csv_cell("=HYPERLINK(\"x\")"), "\"'=HYPERLINK(\"\"x\"\")\"");
        assert_eq!(csv_cell("@SUM(A1)"), "'@SUM(A1)");
        assert_eq!(csv_cell("+1+1"), "'+1+1");
        // Numbers stay numbers.
        assert_eq!(csv_cell("-1500.50"), "-1500.50");
        assert_eq!(csv_cell("+42"), "+42");
        assert_eq!(csv_cell(" padded"), "\" padded\"");
        // Full-width forms, which some locales take as formulas.
        assert_eq!(csv_cell("\u{FF1D}1+1"), "'\u{FF1D}1+1");
        assert_eq!(csv_cell("\u{FF20}SUM(A1)"), "'\u{FF20}SUM(A1)");
        assert_eq!(
            csv_line(&["a".to_owned(), "b,c".to_owned()]),
            "a,\"b,c\"\r\n"
        );
    }

    #[test]
    fn names_are_simple() {
        assert!(valid_name("wallet-journal"));
        assert!(!valid_name("Wallet"));
        assert!(!valid_name("../x"));
        assert!(!valid_name(""));
    }
}
