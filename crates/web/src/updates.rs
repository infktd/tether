//! Platform update checks (F14): once a day, ask GitHub for Tether's latest
//! release and tell admins on the dashboard if it is newer than this build.
//! Admins can switch it off; then Tether never contacts GitHub for this.
//! Upgrading stays a manual image-tag change (N14).

use std::time::Duration;

use serde::Deserialize;
use serde_json::json;
use tether_db::accounts::AccountId;
use tether_db::audit::{self, Actor};
use tether_db::{PgPool, settings};
use tether_jobs::schedule::ScheduleSpec;
use tether_jobs::{JobError, NewJob, Registry};

use crate::AppState;
use crate::error::AppError;

pub const UPDATE_JOB: &str = "platform.update_check";
/// This build's version.
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");
/// `bool`, default on.
const ENABLED: &str = "updates.enabled";
/// `{tag, url, checked_at}`, `{none: true, checked_at}` or `{error, checked_at}`.
const LATEST: &str = "updates.latest";
/// A release response is a few KiB.
const MAX_BODY: u64 = 256 * 1024;

/// Where releases are published. Tests point it at a mock.
#[derive(Debug, Clone)]
pub struct UpdateSource {
    pub api_base: String,
    /// `owner/name`.
    pub repo: String,
}

impl UpdateSource {
    pub fn github() -> Self {
        Self {
            api_base: "https://api.github.com".to_owned(),
            repo: "infktd/tether".to_owned(),
        }
    }
}

pub fn schedules() -> Vec<ScheduleSpec> {
    vec![ScheduleSpec::new(
        "platform.update_check",
        UPDATE_JOB,
        Duration::from_secs(24 * 60 * 60),
    )]
}

pub async fn enabled(db: &PgPool) -> Result<bool, sqlx::Error> {
    Ok(settings::get(db, ENABLED)
        .await?
        .and_then(|v| v.as_bool())
        .unwrap_or(true))
}

pub async fn set_enabled(state: &AppState, actor: AccountId, on: bool) -> Result<(), AppError> {
    let was_on = enabled(&state.db).await?;
    let mut tx = state.db.begin().await?;
    settings::set(&mut *tx, ENABLED, on.into()).await?;
    audit::record(
        &mut *tx,
        Actor::Account(actor),
        "updates.enabled",
        None,
        json!({ "enabled": on }),
    )
    .await?;
    if on && !was_on {
        queue_check(&mut tx).await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Queues a check unless one is already waiting or running: repeated
/// clicks mustn't turn into repeated calls to GitHub.
async fn queue_check(tx: &mut sqlx::PgTransaction<'_>) -> Result<(), sqlx::Error> {
    let pending: i64 = sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM core.jobs WHERE kind = $1 AND state IN ('queued', 'running')"#,
        UPDATE_JOB
    )
    .fetch_one(&mut **tx)
    .await?;
    if pending == 0 {
        tether_jobs::enqueue(&mut **tx, NewJob::new(UPDATE_JOB, json!({}))).await?;
    }
    Ok(())
}

/// Queues a check now (the dashboard's button). Audited: it makes Tether
/// contact GitHub.
pub async fn check_now(state: &AppState, actor: AccountId) -> Result<(), AppError> {
    if !enabled(&state.db).await? {
        return Err(AppError::bad_request(
            "Update checks are off. Turn them on first.",
        ));
    }
    let mut tx = state.db.begin().await?;
    audit::record(
        &mut *tx,
        Actor::Account(actor),
        "updates.check",
        None,
        json!({}),
    )
    .await?;
    queue_check(&mut tx).await?;
    tx.commit().await?;
    Ok(())
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    html_url: String,
}

/// Asks GitHub for the latest release and records it. Does nothing when
/// switched off. GitHub's answer is shown to admins, so it is checked
/// first: a plain version tag, and a link into this repository only.
pub async fn check(
    db: &PgPool,
    http: &reqwest::Client,
    source: &UpdateSource,
) -> Result<(), JobError> {
    if !enabled(db).await.map_err(JobError::retry)? {
        return Ok(());
    }
    // Not before an owner exists: whoever runs the instance gets the
    // chance to switch it off before Tether first contacts GitHub.
    if !tether_db::accounts::owner_exists(db)
        .await
        .map_err(JobError::retry)?
    {
        return Ok(());
    }
    let url = format!("{}/repos/{}/releases/latest", source.api_base, source.repo);
    let checked_at = chrono::Utc::now().to_rfc3339();
    let result = async {
        let response = http
            .get(&url)
            .header("accept", "application/vnd.github+json")
            .send()
            .await
            .map_err(|e| e.without_url().to_string())?;
        match response.status().as_u16() {
            200 => {}
            // No releases published yet.
            404 => return Ok(None),
            status => return Err(format!("GitHub answered HTTP {status}")),
        }
        if response.content_length().is_some_and(|n| n > MAX_BODY) {
            return Err("GitHub sent an oversized response".to_owned());
        }
        let body = response
            .bytes()
            .await
            .map_err(|e| e.without_url().to_string())?;
        if body.len() as u64 > MAX_BODY {
            return Err("GitHub sent an oversized response".to_owned());
        }
        let release: Release = serde_json::from_slice(&body)
            .map_err(|_| "unreadable release from GitHub".to_owned())?;
        Ok(Some(release))
    }
    .await;
    let value = match result {
        Ok(Some(release)) => {
            let tag = release.tag_name.trim();
            if tag.is_empty()
                || tag.len() > 40
                || !tag
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+'))
            {
                json!({ "error": "GitHub sent an odd release tag", "checked_at": checked_at })
            } else {
                let link = release_link(&release.html_url, &source.repo);
                json!({ "tag": tag, "url": link, "checked_at": checked_at })
            }
        }
        Ok(None) => json!({ "none": true, "checked_at": checked_at }),
        Err(error) => {
            settings::set(
                db,
                LATEST,
                json!({ "error": error, "checked_at": checked_at }),
            )
            .await
            .map_err(JobError::retry)?;
            return Err(JobError::retry(error));
        }
    };
    settings::set(db, LATEST, value)
        .await
        .map_err(JobError::retry)?;
    Ok(())
}

/// What the dashboard shows.
#[derive(Debug, Clone, Default)]
pub struct Status {
    pub enabled: bool,
    pub current: &'static str,
    pub latest: Option<String>,
    pub url: Option<String>,
    /// The latest release is newer than this build.
    pub newer: bool,
    pub checked_at: Option<String>,
    pub error: Option<String>,
    /// Checked, and nothing has been released yet.
    pub no_releases: bool,
}

pub async fn status(db: &PgPool) -> Result<Status, sqlx::Error> {
    let latest = settings::get(db, LATEST).await?.unwrap_or_default();
    let text = |key: &str| latest.get(key).and_then(|v| v.as_str()).map(str::to_owned);
    let tag = text("tag");
    Ok(Status {
        enabled: enabled(db).await?,
        current: CURRENT,
        newer: tag.as_deref().is_some_and(|t| is_newer(t, CURRENT)),
        latest: tag,
        // Checked again on the way out: whatever wrote it, only a link into
        // the release pages is shown.
        url: text("url").and_then(|u| release_link(&u, &UpdateSource::github().repo)),
        checked_at: text("checked_at").map(|at| {
            chrono::DateTime::parse_from_rfc3339(&at)
                .map_or(at, |t| t.format("%Y-%m-%d %H:%M").to_string())
        }),
        error: text("error"),
        no_releases: latest.get("none").is_some(),
    })
}

/// The release notes link, if it points at this repository's releases.
fn release_link(url: &str, repo: &str) -> Option<String> {
    let expected = format!("https://github.com/{repo}/releases/");
    (url.starts_with(&expected) && !url.chars().any(|c| c.is_whitespace() || c == '"'))
        .then(|| url.to_owned())
}

/// `v1.2.3` style tags compared numerically; anything unparseable (or a
/// pre-release) is never "newer".
pub fn is_newer(tag: &str, current: &str) -> bool {
    fn parse(v: &str) -> Option<(u64, u64, u64)> {
        let v = v.strip_prefix('v').unwrap_or(v);
        let mut parts = v.split('.');
        let version = (
            parts.next()?.parse().ok()?,
            parts.next()?.parse().ok()?,
            parts.next()?.parse().ok()?,
        );
        parts.next().is_none().then_some(version)
    }
    matches!((parse(tag), parse(current)), (Some(t), Some(c)) if t > c)
}

/// The client for GitHub: no redirects, so it can't be sent elsewhere, and
/// a User-Agent that names the software but not this instance (GitHub
/// needn't learn the alliance's domain).
pub fn http_client() -> Result<reqwest::Client, reqwest::Error> {
    reqwest::Client::builder()
        .user_agent(format!("tether/{CURRENT}"))
        .timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .build()
}

pub fn register_jobs(
    registry: &mut Registry,
    db: PgPool,
    http: reqwest::Client,
    source: UpdateSource,
) {
    registry.register(UPDATE_JOB, move |_job| {
        let (db, source, http) = (db.clone(), source.clone(), http.clone());
        async move { check(&db, &http, &source).await }
    });
}

#[cfg(test)]
mod tests {
    use super::is_newer;

    #[test]
    fn compares_versions_numerically() {
        assert!(is_newer("v0.2.0", "0.1.0"));
        assert!(is_newer("0.10.0", "0.9.9"));
        assert!(!is_newer("v0.1.0", "0.1.0"));
        assert!(!is_newer("v0.0.9", "0.1.0"));
        assert!(!is_newer("v0.2.0-rc1", "0.1.0"));
        assert!(!is_newer("latest", "0.1.0"));
    }
}
