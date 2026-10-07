//! Apps' notices in Tether's notifications (the `notify` interface), as AA
//! apps `notify`: plain text, the app's name before the title, only to
//! accounts holding one of the app's own permissions (its audience, which
//! it could reach anyway through `holders`) or to an account that submitted
//! one of its forms, by the reference the host gave the app then (an
//! applicant, a requester; Jay, 2026-10-07), and within limits. Each is
//! stored as the app's and shown with its id, which no other app can take,
//! so no app passes for Tether or another app; and each app keeps only its
//! newest few of an account's notices, so none can push the rest out.

use std::sync::Weak;
use std::time::{Duration, Instant};

use crate::plugins::Plugins;
use tether_db::PgPool;
use tether_db::accounts::AccountId;
use tether_db::notifications::Level;
use tether_plugins::manifest::Manifest;
use tether_plugins::services::{NotifyError, NotifyLevel};

use crate::ratelimit::RateLimiter;

/// The longest title an app may send, its name not counted.
pub const MAX_TITLE: usize = 100;
/// The longest message.
pub const MAX_MESSAGE: usize = 1_000;
/// Notices one app may send an hour, every recipient counted.
pub const PER_APP_HOUR: usize = 500;
/// Notices one app may send one account an hour.
pub const PER_ACCOUNT_HOUR: usize = 20;
/// The most holders one `holders` call may reach.
pub const MAX_HOLDERS: usize = 200;
/// An app's notices an account keeps, its newest; the rest of the
/// account's list stays Tether's and other apps'.
pub const KEPT_PER_APP: i64 = 5;

const HOUR: Duration = Duration::from_secs(60 * 60);

/// What apps have sent lately.
#[derive(Debug)]
pub struct Limits {
    apps: RateLimiter<String>,
    accounts: RateLimiter<(String, i64)>,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            apps: RateLimiter::new(PER_APP_HOUR, HOUR),
            accounts: RateLimiter::new(PER_ACCOUNT_HOUR, HOUR),
        }
    }
}

/// A notice checked and ready to send.
struct Notice {
    title: String,
    message: String,
    level: Level,
}

fn running(plugins: &Weak<Plugins>, plugin: &str) -> Result<std::sync::Arc<Manifest>, NotifyError> {
    let manifest = plugins
        .upgrade()
        .and_then(|p| p.running(plugin))
        .map(|r| r.manifest.clone())
        .ok_or(NotifyError::Unavailable)?;
    if !manifest.capabilities.notify {
        return Err(NotifyError::Invalid(
            "sending notices needs `notify = true` in plugin.toml".to_owned(),
        ));
    }
    Ok(manifest)
}

fn text(what: &str, value: &str, max: usize) -> Result<String, NotifyError> {
    let clean = tether_plugins::host::printable(value, max.saturating_mul(4));
    let clean = clean.trim();
    if clean.is_empty() {
        return Err(NotifyError::Invalid(format!("the {what} is empty")));
    }
    if clean.chars().count() > max {
        return Err(NotifyError::Invalid(format!(
            "the {what} is longer than {max} characters"
        )));
    }
    Ok(clean.to_owned())
}

fn notice(
    manifest: &Manifest,
    title: &str,
    message: &str,
    level: NotifyLevel,
) -> Result<Notice, NotifyError> {
    let title = text("title", title, MAX_TITLE)?;
    let message = text("message", message, MAX_MESSAGE)?;
    Ok(Notice {
        // As AA apps title theirs; the app's id goes beside it.
        title: format!("{}: {title}", manifest.plugin.name),
        message,
        level: match level {
            NotifyLevel::Info => Level::Info,
            NotifyLevel::Success => Level::Success,
            NotifyLevel::Warning => Level::Warning,
            NotifyLevel::Danger => Level::Danger,
        },
    })
}

fn unavailable(plugin: &str, err: &sqlx::Error) -> NotifyError {
    tracing::error!(plugin, error = %err, "sending an app's notice");
    NotifyError::Unavailable
}

/// Whether `account` holds one of the app's own permissions.
async fn in_audience(db: &PgPool, manifest: &Manifest, account: i64) -> Result<bool, sqlx::Error> {
    let held = tether_db::permissions::effective(db, AccountId(account)).await?;
    let prefix = format!("plugin.{}.", manifest.plugin.id);
    Ok(manifest
        .permissions
        .keys()
        .any(|name| held.contains(&format!("{prefix}{name}"))))
}

/// Counts one against the app's hourly limit.
fn charge(limits: &Limits, plugin: &str) -> Result<(), NotifyError> {
    limits
        .apps
        .check(plugin.to_owned(), Instant::now())
        .map_err(|_| {
            NotifyError::Invalid(format!(
                "at most {PER_APP_HOUR} notices an hour: try again later"
            ))
        })
}

/// Within this account's hourly limit from the app.
fn allowed(limits: &Limits, plugin: &str, account: i64) -> bool {
    limits
        .accounts
        .check((plugin.to_owned(), account), Instant::now())
        .is_ok()
}

async fn send(db: &PgPool, plugin: &str, account: i64, notice: &Notice) -> Result<(), NotifyError> {
    let mut tx = db.begin().await.map_err(|e| unavailable(plugin, &e))?;
    tether_db::notifications::notify_from_app(
        &mut tx,
        AccountId(account),
        plugin,
        notice.level,
        &notice.title,
        &notice.message,
        KEPT_PER_APP,
    )
    .await
    .map_err(|e| unavailable(plugin, &e))?;
    tx.commit().await.map_err(|e| unavailable(plugin, &e))
}

/// `notify.account`.
#[allow(clippy::too_many_arguments)]
pub async fn to_account(
    db: &PgPool,
    plugins: &Weak<Plugins>,
    limits: &Limits,
    plugin: &str,
    account: i64,
    title: &str,
    message: &str,
    level: NotifyLevel,
) -> Result<bool, NotifyError> {
    let manifest = running(plugins, plugin)?;
    let notice = notice(&manifest, title, message, level)?;
    // Every try counts, those that reach nobody too: asking about
    // accounts isn't free.
    charge(limits, plugin)?;
    if !in_audience(db, &manifest, account)
        .await
        .map_err(|e| unavailable(plugin, &e))?
        || !allowed(limits, plugin, account)
    {
        return Ok(false);
    }
    send(db, plugin, account, &notice).await?;
    tracing::info!(plugin, recipients = 1, "app notice sent");
    Ok(true)
}

/// `notify.holders`.
#[allow(clippy::too_many_arguments)]
pub async fn to_holders(
    db: &PgPool,
    plugins: &Weak<Plugins>,
    limits: &Limits,
    plugin: &str,
    permission: &str,
    title: &str,
    message: &str,
    level: NotifyLevel,
    except: Option<i64>,
) -> Result<u32, NotifyError> {
    let manifest = running(plugins, plugin)?;
    if !manifest.permissions.contains_key(permission) {
        return Err(NotifyError::Invalid(format!(
            "{permission:?} isn't one of this app's permissions"
        )));
    }
    let notice = notice(&manifest, title, message, level)?;
    let holders = tether_db::permissions_audit::holders(
        db,
        &format!("plugin.{}.{permission}", manifest.plugin.id),
    )
    .await
    .map_err(|e| unavailable(plugin, &e))?;
    let recipients: Vec<i64> = holders
        .iter()
        .map(|h| h.account_id)
        .filter(|a| Some(*a) != except)
        .collect();
    if recipients.len() > MAX_HOLDERS {
        return Err(NotifyError::Invalid(format!(
            "{} hold {permission:?}: notices go to at most {MAX_HOLDERS}",
            recipients.len()
        )));
    }
    let mut reached = 0;
    for account in recipients {
        charge(limits, plugin)?;
        if allowed(limits, plugin, account) {
            send(db, plugin, account, &notice).await?;
            reached += 1;
        }
    }
    tracing::info!(plugin, recipients = reached, "app notice sent");
    Ok(reached)
}

/// `notify.submitter-reference`: the form poster's reference, `account`
/// being the viewer the host built (the caller makes sure).
pub async fn submitter_reference(
    db: &PgPool,
    plugins: &Weak<Plugins>,
    plugin: &str,
    account: i64,
) -> Result<String, NotifyError> {
    running(plugins, plugin)?;
    tether_db::submitters::reference(db, plugin, AccountId(account))
        .await
        .map_err(|e| unavailable(plugin, &e))
}

/// Whether `reference` could be one the host made: 32 lowercase hex
/// digits. Anything else isn't looked up.
pub(crate) fn well_formed(reference: &str) -> bool {
    reference.len() == 32
        && reference
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// `notify.submitter`: to the account behind one of the app's own
/// references, whatever it holds; nobody else.
#[allow(clippy::too_many_arguments)]
pub async fn to_submitter(
    db: &PgPool,
    plugins: &Weak<Plugins>,
    limits: &Limits,
    plugin: &str,
    reference: &str,
    title: &str,
    message: &str,
    level: NotifyLevel,
) -> Result<bool, NotifyError> {
    let manifest = running(plugins, plugin)?;
    let notice = notice(&manifest, title, message, level)?;
    if !well_formed(reference) {
        return Err(NotifyError::Invalid(
            "that isn't a submitter reference".to_owned(),
        ));
    }
    // Every try counts, as `account`'s.
    charge(limits, plugin)?;
    let Some(account) = tether_db::submitters::account(db, plugin, reference)
        .await
        .map_err(|e| unavailable(plugin, &e))?
    else {
        return Ok(false);
    };
    if !allowed(limits, plugin, account.0) {
        return Ok(false);
    }
    send(db, plugin, account.0, &notice).await?;
    tracing::info!(plugin, recipients = 1, "app notice sent to a submitter");
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::well_formed;

    #[test]
    fn only_references_the_host_could_have_made_are_looked_up() {
        assert!(well_formed("0123456789abcdef0123456789abcdef"));
        for bad in [
            "",
            "0123456789abcdef0123456789abcde",
            "0123456789abcdef0123456789abcdef0",
            "0123456789ABCDEF0123456789abcdef",
            "0123456789abcdef0123456789abcde%",
            "42",
        ] {
            assert!(!well_formed(bad), "{bad}");
        }
    }
}
