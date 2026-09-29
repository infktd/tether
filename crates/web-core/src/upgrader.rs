//! Upgrading and rolling back Tether from the console (N14; Jay,
//! 2026-09-28). The app never touches Docker: it writes a request for the
//! updater container (`deploy/updater.sh`) into a volume only the two
//! share, and reads what happened from another the updater writes and the
//! app mounts read-only. The updater checks every request itself (only a
//! published tag of the image `.env` names; a rollback only to the image
//! it recorded before its last upgrade), so the worst a compromised app
//! can do is ask for a real published version.
//!
//! Each start records what it runs and the snapshot it took before
//! migrating ([`record_start`]): a rollback restores that snapshot when
//! the upgrade migrated, as `deploy/README.md`'s rollback does.

use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use tether_db::accounts::AccountId;
use tether_db::audit::{self, Actor};
use tether_db::{PgPool, settings};

use crate::AppState;
use crate::error::AppError;
use crate::updates;

/// `{version, revision, image, snapshot, at}` of this run's first start.
const STARTED: &str = "updates.started";
/// The same, of the version that ran before it.
const PREVIOUS: &str = "updates.previous_start";
/// The updater writes `alive` every few seconds while it runs.
const ALIVE_WITHIN: Duration = Duration::from_secs(60);
/// A request not picked up, or a step not finished, in this long is
/// considered stuck: the buttons come back.
const STUCK_AFTER: Duration = Duration::from_secs(30 * 60);
/// The largest status file read.
const MAX_STATUS: u64 = 4096;

/// Where the app meets the updater, and what it runs.
#[derive(Debug, Clone)]
pub struct Updater {
    /// Requests from the app (`UPDATE_REQUESTS_DIR`).
    pub requests: PathBuf,
    /// The updater's status (`UPDATE_STATUS_DIR`), read-only here.
    pub status: PathBuf,
    /// `TETHER_IMAGE`, as docker-compose.yml passes it.
    pub image: Option<String>,
    /// The commit this build is from (`TETHER_REVISION`, set by CI).
    pub revision: Option<String>,
}

impl Default for Updater {
    fn default() -> Self {
        Self {
            requests: PathBuf::from("/var/lib/tether/update-requests"),
            status: PathBuf::from("/var/lib/tether/update-status"),
            image: None,
            revision: None,
        }
    }
}

impl Updater {
    pub fn from_env() -> Self {
        let var = |name: &str| {
            std::env::var(name)
                .ok()
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
        };
        let default = Self::default();
        Self {
            requests: var("UPDATE_REQUESTS_DIR").map_or(default.requests, PathBuf::from),
            status: var("UPDATE_STATUS_DIR").map_or(default.status, PathBuf::from),
            image: var("TETHER_IMAGE"),
            revision: var("TETHER_REVISION").filter(|r| is_revision(r)),
        }
    }

    /// The tag the image follows (`edge`, `1.2.0`), when it's a published
    /// one.
    pub fn tag(&self) -> Option<&str> {
        let image = self.image.as_deref()?;
        if image.contains('@') {
            return None;
        }
        let (_, tag) = image.rsplit_once('/')?.1.split_once(':')?;
        is_tag(tag).then_some(tag)
    }

    /// The updater is running: it wrote its heartbeat lately.
    pub fn running(&self) -> bool {
        age(&self.status.join("alive")).is_some_and(|a| a < ALIVE_WITHIN)
    }

    /// Built from a clone (`install.sh --build`): upgraded on the server.
    pub fn from_source(&self) -> bool {
        self.image
            .as_deref()
            .is_some_and(|i| i == "tether:local" || i.ends_with("/tether:local"))
    }
}

/// A tag the updater takes: `X.Y.Z`, `X.Y`, `latest` or `edge`.
pub fn is_tag(tag: &str) -> bool {
    if tag == "edge" || tag == "latest" {
        return true;
    }
    let parts: Vec<&str> = tag.split('.').collect();
    (2..=3).contains(&parts.len())
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.len() <= 6 && p.bytes().all(|b| b.is_ascii_digit()))
}

fn is_revision(revision: &str) -> bool {
    (7..=40).contains(&revision.len()) && revision.bytes().all(|b| b.is_ascii_hexdigit())
}

/// A snapshot file name the updater passes to `tether rollback`.
fn is_snapshot_name(name: &str) -> bool {
    (1..=200).contains(&name.len())
        && name
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_alphanumeric())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// A version as admins read it: `1.2.0`, or `0.1.0 (a1b2c3d)` for a build
/// of main.
fn label(version: &str, revision: Option<&str>) -> String {
    match revision {
        Some(r) => format!("{version} ({})", r.chars().take(7).collect::<String>()),
        None => version.to_owned(),
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
struct Start {
    version: String,
    #[serde(default)]
    revision: Option<String>,
    #[serde(default)]
    snapshot: Option<String>,
}

/// Records this start: what runs and, when it migrated, the snapshot taken
/// first. A start of the same build keeps its first start's snapshot; a
/// new build moves the last one to [`PREVIOUS`].
pub async fn record_start(
    db: &PgPool,
    updater: &Updater,
    snapshot: Option<&str>,
) -> Result<(), sqlx::Error> {
    let stored = settings::get(db, STARTED).await?;
    let same = stored.as_ref().and_then(|s| {
        serde_json::from_value::<Start>(s.clone())
            .ok()
            .filter(|s| s.version == updates::CURRENT && s.revision == updater.revision)
    });
    let mut tx = db.begin().await?;
    if let Some(same) = same {
        // A restart (or a crash loop) of this build: its first snapshot is
        // the one from before the upgrade.
        if same.snapshot.is_none() && snapshot.is_some() {
            let mut value = stored.unwrap_or_default();
            value["snapshot"] = json!(snapshot);
            settings::set(&mut *tx, STARTED, value).await?;
        }
    } else {
        if let Some(stored) = stored {
            settings::set(&mut *tx, PREVIOUS, stored).await?;
        }
        settings::set(
            &mut *tx,
            STARTED,
            json!({
                "version": updates::CURRENT,
                "revision": updater.revision,
                "image": updater.image,
                "snapshot": snapshot,
                "at": Utc::now().to_rfc3339(),
            }),
        )
        .await?;
    }
    tx.commit().await
}

#[derive(Debug, Clone, Deserialize)]
struct RawStatus {
    #[serde(default)]
    action: String,
    #[serde(default)]
    state: String,
    #[serde(default)]
    message: String,
    #[serde(default)]
    at: String,
}

/// The updater's last report.
#[derive(Debug, Clone)]
pub struct Status {
    pub action: &'static str,
    /// `running`, `done` or `failed`.
    pub state: &'static str,
    pub message: String,
    pub at: String,
    pub stuck: bool,
}

/// What the System page shows.
#[derive(Debug, Clone, Default)]
pub struct View {
    pub running: String,
    pub image: Option<String>,
    /// The updater is running (it wrote lately).
    pub available: bool,
    pub from_source: bool,
    /// A request waits, or a step is under way.
    pub busy: bool,
    pub status: Option<Status>,
    /// The tag Upgrade asks for.
    pub upgrade_to: Option<String>,
    /// The version Roll back goes to, which the admin types to confirm.
    pub rollback_to: Option<String>,
    /// The rollback restores the snapshot taken before this version
    /// migrated.
    pub rollback_restores: bool,
}

fn age(path: &Path) -> Option<Duration> {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
}

fn read_status(dir: &Path) -> Option<Status> {
    let path = dir.join("status.json");
    let meta = std::fs::metadata(&path).ok()?;
    if !meta.is_file() || meta.len() > MAX_STATUS {
        return None;
    }
    let raw: RawStatus = serde_json::from_slice(&std::fs::read(&path).ok()?).ok()?;
    let action = match raw.action.as_str() {
        "upgrade" => "upgrade",
        "rollback" => "rollback",
        _ => "request",
    };
    let state = match raw.state.as_str() {
        "running" => "running",
        "done" => "done",
        _ => "failed",
    };
    let at = DateTime::parse_from_rfc3339(&raw.at).ok();
    let stuck = state == "running"
        && at.is_none_or(|at| {
            Utc::now()
                .signed_duration_since(at)
                .to_std()
                .unwrap_or_default()
                > STUCK_AFTER
        });
    Some(Status {
        action,
        state,
        message: raw.message.chars().take(300).collect(),
        at: at.map_or_else(String::new, |t| t.format("%Y-%m-%d %H:%M").to_string()),
        stuck,
    })
}

pub async fn view(state: &AppState) -> Result<View, AppError> {
    let updater = &state.updater;
    let available = updater.running();
    let status = read_status(&updater.status);
    let pending = age(&updater.requests.join("request")).is_some_and(|a| a < STUCK_AFTER);
    let busy = pending
        || status
            .as_ref()
            .is_some_and(|s| s.state == "running" && !s.stuck);
    let started: Start = settings::get(&state.db, STARTED)
        .await?
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default();
    let previous: Option<Start> = settings::get(&state.db, PREVIOUS)
        .await?
        .and_then(|v| serde_json::from_value(v).ok());
    let can_roll_back = updater.status.join("previous").is_file();
    let checks = updates::status(&state.db).await?;
    let upgrade_to = match updater.tag() {
        // Edge moves: the newest main, whenever asked.
        Some("edge") => Some("edge".to_owned()),
        _ => checks
            .latest
            .filter(|_| checks.newer)
            .map(|t| t.trim_start_matches('v').to_owned())
            .filter(|t| is_tag(t)),
    };
    let current = label(updates::CURRENT, updater.revision.as_deref());
    let rollback_to = can_roll_back.then(|| {
        previous.as_ref().map_or_else(
            || "previous".to_owned(),
            |p| {
                let back = label(&p.version, p.revision.as_deref());
                if back == current {
                    "previous".to_owned()
                } else {
                    back
                }
            },
        )
    });
    Ok(View {
        running: current,
        image: updater.image.clone(),
        available,
        from_source: updater.from_source(),
        busy,
        status,
        upgrade_to,
        rollback_restores: rollback_to.is_some()
            && started.snapshot.as_deref().is_some_and(is_snapshot_name),
        rollback_to,
    })
}

/// A request id: only for matching the updater's report to the request.
fn request_id() -> String {
    format!(
        "{:016x}",
        Utc::now().timestamp_nanos_opt().unwrap_or_default()
    )
}

/// Writes a request for the updater in one rename, so it never reads half.
async fn write_request(updater: &Updater, lines: &[(&str, &str)]) -> Result<(), AppError> {
    let text: String = lines.iter().map(|(k, v)| format!("{k}={v}\n")).collect();
    let tmp = updater.requests.join(".request.tmp");
    tokio::fs::write(&tmp, text).await.map_err(|err| {
        tracing::warn!(error = %err, "writing an update request");
        AppError::bad_request(
            "The updater's folder isn't writable: run deploy/install.sh again on the server.",
        )
    })?;
    tokio::fs::rename(&tmp, updater.requests.join("request"))
        .await
        .map_err(|err| {
            tracing::warn!(error = %err, "writing an update request");
            AppError::bad_request(
                "The updater's folder isn't writable: run deploy/install.sh again on the server.",
            )
        })
}

async fn ready(state: &AppState) -> Result<View, AppError> {
    let view = view(state).await?;
    if view.from_source {
        return Err(AppError::bad_request(
            "This install builds Tether from source: upgrade on the server with git pull and deploy/install.sh --build.",
        ));
    }
    if !view.available {
        return Err(AppError::bad_request(
            "The updater isn't running: run deploy/install.sh again on the server to add it.",
        ));
    }
    if view.busy {
        return Err(AppError::bad_request(
            "An upgrade or rollback is already under way.",
        ));
    }
    Ok(view)
}

/// Asks the updater to move to `tag` (what the page offered).
pub async fn upgrade(state: &AppState, actor: AccountId, tag: &str) -> Result<(), AppError> {
    crate::sudo::check(crate::sudo::Action::PlatformUpgrade)?;
    let view = ready(state).await?;
    if view.upgrade_to.as_deref() != Some(tag) || !is_tag(tag) {
        return Err(AppError::bad_request(
            "That version isn't offered any more: reload the page.",
        ));
    }
    let id = request_id();
    let mut tx = state.db.begin().await?;
    audit::record(
        &mut *tx,
        Actor::Account(actor),
        "platform.upgrade",
        None,
        json!({ "from": view.running, "to": tag, "request": id }),
    )
    .await?;
    // Audited first: nothing is asked of the updater unrecorded.
    tx.commit().await?;
    write_request(
        &state.updater,
        &[("id", &id), ("action", "upgrade"), ("tag", tag)],
    )
    .await
}

/// Asks the updater to go back one step; `confirmation` is the version
/// typed.
pub async fn rollback(
    state: &AppState,
    actor: AccountId,
    confirmation: &str,
) -> Result<(), AppError> {
    crate::sudo::check(crate::sudo::Action::PlatformRollback)?;
    let view = ready(state).await?;
    let Some(to) = view.rollback_to.clone() else {
        return Err(AppError::bad_request("There's nothing to roll back to."));
    };
    if confirmation.trim() != to {
        return Err(AppError::bad_request(format!(
            "Type {to} to confirm the rollback."
        )));
    }
    let snapshot = if view.rollback_restores {
        settings::get(&state.db, STARTED)
            .await?
            .and_then(|v| v.get("snapshot").and_then(Value::as_str).map(str::to_owned))
            .filter(|s| is_snapshot_name(s))
    } else {
        None
    };
    let id = request_id();
    let mut tx = state.db.begin().await?;
    audit::record(
        &mut *tx,
        Actor::Account(actor),
        "platform.rollback",
        None,
        json!({ "from": view.running, "to": to, "snapshot": snapshot, "request": id }),
    )
    .await?;
    let mut lines = vec![("id", id.as_str()), ("action", "rollback")];
    if let Some(snapshot) = snapshot.as_deref() {
        lines.push(("snapshot", snapshot));
    }
    tx.commit().await?;
    write_request(&state.updater, &lines).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_published_tags() {
        for tag in ["edge", "latest", "1.2", "1.2.3", "10.20.300"] {
            assert!(is_tag(tag), "{tag}");
        }
        for tag in [
            "",
            "1",
            "1.2.3.4",
            "v1.2.3",
            "1..2",
            "local",
            "1.2.3-rc1",
            "edge\n",
        ] {
            assert!(!is_tag(tag), "{tag}");
        }
    }

    #[test]
    fn the_image_says_which_tag_it_follows() {
        let with = |image: &str| Updater {
            image: Some(image.to_owned()),
            ..Updater::default()
        };
        assert_eq!(with("ghcr.io/acme/tether:edge").tag(), Some("edge"));
        assert_eq!(with("ghcr.io/acme/tether:1.2.0").tag(), Some("1.2.0"));
        assert_eq!(with("ghcr.io/acme/tether@sha256:abc").tag(), None);
        assert_eq!(with("tether:local").tag(), None);
        assert!(with("tether:local").from_source());
        assert!(!with("ghcr.io/acme/tether:edge").from_source());
    }

    #[test]
    fn snapshot_names_are_plain() {
        assert!(is_snapshot_name("core-20260928T101500Z.tsnap"));
        for name in ["", ".hidden", "../x", "a b", "a/b", "-x"] {
            assert!(!is_snapshot_name(name), "{name}");
        }
    }

    #[test]
    fn versions_read_with_their_commit() {
        assert_eq!(label("1.2.0", None), "1.2.0");
        assert_eq!(label("0.1.0", Some("a1b2c3d4e5")), "0.1.0 (a1b2c3d)");
    }
}
