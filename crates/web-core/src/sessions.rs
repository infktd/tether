//! An account's signed-in sessions (the account menu's Sessions page), and
//! signing them out: one, every one but this browser's, or, for an admin
//! with `admin.users` in sudo mode, every one of someone else's. Each is
//! audited.
//!
//! A session is named by its plain id (`core.sessions.id`), never by the
//! cookie or its hash, and only ever among the signed-in account's own.
//! What the page shows of a browser is a coarse label from a fixed list,
//! read once from its User-Agent at sign-in ([`device_label`]); nothing
//! else about it (no User-Agent, no IP address) is kept.

use axum::http::{HeaderMap, header};
use serde_json::json;
use tether_db::accounts::AccountId;
use tether_db::audit::{self, Actor};
use tether_db::auth as db;

use crate::AppState;
use crate::auth::CurrentSession;
use crate::error::AppError;

/// A coarse label for the browser behind `headers` ("Firefox on Windows"),
/// from a fixed list of browsers and systems: never any of the header's
/// own text, so nothing more about the browser is kept.
pub fn device_of(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .and_then(device_label)
}

/// See [`device_of`]. Order matters: Edge and Opera say Chrome too, Chrome
/// says Safari, and iPads and Android say Mac OS X and Linux.
pub fn device_label(user_agent: &str) -> Option<String> {
    let has = |needles: &[&str]| needles.iter().any(|n| user_agent.contains(n));
    let browser = if has(&["Edg/", "EdgA/", "EdgiOS/"]) {
        Some("Edge")
    } else if has(&["OPR/", "Opera"]) {
        Some("Opera")
    } else if has(&["Firefox/", "FxiOS/"]) {
        Some("Firefox")
    } else if has(&["Chrome/", "CriOS/", "Chromium/"]) {
        Some("Chrome")
    } else if has(&["Safari/"]) {
        Some("Safari")
    } else {
        None
    };
    let system = if has(&["Windows"]) {
        Some("Windows")
    } else if has(&["iPhone", "iPad", "iPod"]) {
        Some("iOS")
    } else if has(&["Android"]) {
        Some("Android")
    } else if has(&["CrOS"]) {
        Some("ChromeOS")
    } else if has(&["Macintosh", "Mac OS X"]) {
        Some("macOS")
    } else if has(&["Linux"]) {
        Some("Linux")
    } else {
        None
    };
    // Only the words above: nothing of the header's own.
    match (browser, system) {
        (Some(b), Some(s)) => Some(format!("{b} on {s}")),
        (Some(one), None) | (None, Some(one)) => Some(one.to_owned()),
        (None, None) => None,
    }
}

/// The browser session making the request (its id), or 403 for anything
/// else: access tokens never manage sessions.
fn browser_session(session: &CurrentSession) -> Result<i64, AppError> {
    match (session.token_scopes.as_ref(), session.session_id) {
        (None, Some(id)) => Ok(id),
        _ => Err(AppError::forbidden()),
    }
}

/// The account's live sessions, and which of them is this browser's.
pub async fn list(
    state: &AppState,
    session: &CurrentSession,
) -> Result<(Vec<db::SessionRow>, i64), AppError> {
    let current = browser_session(session)?;
    Ok((db::sessions_of(&state.db, session.account).await?, current))
}

/// Signs out one of the account's other sessions. Not this browser's
/// own: that's Log out.
pub async fn sign_out(state: &AppState, session: &CurrentSession, id: i64) -> Result<(), AppError> {
    let current = browser_session(session)?;
    if id == current {
        return Err(AppError::bad_request(
            "That's this browser's session: use Log out in the account menu.",
        ));
    }
    let mut tx = state.db.begin().await?;
    let Some(device) = db::end_session(&mut *tx, session.account, id).await? else {
        return Err(AppError::not_found(
            "No such session: it may have ended already.",
        ));
    };
    audit::record(
        &mut *tx,
        Actor::Account(session.account),
        "session.sign_out",
        Some(&format!("account:{}", session.account.0)),
        json!({ "session_id": id, "device": device }),
    )
    .await?;
    tx.commit().await?;
    tracing::info!(
        account = session.account.0,
        session_id = id,
        "session signed out"
    );
    Ok(())
}

/// Signs out every session of the account but this browser's; how many.
pub async fn sign_out_others(state: &AppState, session: &CurrentSession) -> Result<u64, AppError> {
    let current = browser_session(session)?;
    let mut tx = state.db.begin().await?;
    let ended = db::end_other_sessions(&mut *tx, session.account, current).await?;
    audit::record(
        &mut *tx,
        Actor::Account(session.account),
        "session.sign_out_others",
        Some(&format!("account:{}", session.account.0)),
        json!({ "sessions": ended }),
    )
    .await?;
    tx.commit().await?;
    tracing::info!(
        account = session.account.0,
        sessions = ended,
        "other sessions signed out"
    );
    Ok(ended)
}

/// An admin signs `target` out everywhere (sudo mode): only an account
/// whose permissions are all the admin's, as deactivating, and not the
/// admin's own (that's the Sessions page). The caller checked
/// `admin.users`. How many sessions ended.
pub async fn sign_out_user(
    state: &AppState,
    admin: AccountId,
    target: AccountId,
) -> Result<u64, AppError> {
    crate::sudo::check(crate::sudo::Action::SignOutUser)?;
    if admin == target {
        return Err(AppError::bad_request(
            "That's your own account: use Sessions in the account menu.",
        ));
    }
    let mut tx = state.db.begin().await?;
    if tether_db::accounts::is_active(&mut *tx, target)
        .await?
        .is_none()
    {
        return Err(AppError::not_found("No such account."));
    }
    let mine = tether_db::permissions::effective_in(&mut tx, admin).await?;
    let theirs = tether_db::permissions::effective_in(&mut tx, target).await?;
    crate::admin::refuse_unless_held(&mine, &theirs)?;
    let ended = db::end_all_sessions(&mut *tx, target).await?;
    audit::record(
        &mut *tx,
        Actor::Account(admin),
        "session.sign_out_all",
        Some(&format!("account:{}", target.0)),
        json!({ "sessions": ended }),
    )
    .await?;
    tx.commit().await?;
    tracing::info!(
        admin = admin.0,
        account = target.0,
        sessions = ended,
        "account signed out everywhere"
    );
    Ok(ended)
}

/// "1 session", "3 sessions".
pub fn sessions(n: u64) -> String {
    if n == 1 {
        "1 session".to_owned()
    } else {
        format!("{n} sessions")
    }
}

#[cfg(test)]
mod tests {
    use super::device_label;

    #[test]
    fn devices_are_coarse_labels_from_a_fixed_list() {
        for (ua, label) in [
            (
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:131.0) Gecko/20100101 Firefox/131.0",
                Some("Firefox on Windows"),
            ),
            (
                "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Safari/605.1.15",
                Some("Safari on macOS"),
            ),
            (
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/129.0.0.0 Safari/537.36 Edg/129.0.0.0",
                Some("Edge on Windows"),
            ),
            (
                "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/129.0.0.0 Safari/537.36 OPR/114.0.0.0",
                Some("Opera on Linux"),
            ),
            (
                "Mozilla/5.0 (Linux; Android 14; Pixel 8) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/129.0.0.0 Mobile Safari/537.36",
                Some("Chrome on Android"),
            ),
            (
                "Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) CriOS/129.0 Mobile/15E148 Safari/604.1",
                Some("Chrome on iOS"),
            ),
            (
                "Mozilla/5.0 (X11; CrOS x86_64 14541.0.0) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/129.0.0.0 Safari/537.36",
                Some("Chrome on ChromeOS"),
            ),
            ("curl/8.7.1", None),
            ("", None),
            ("Mozilla/5.0 (Windows NT 10.0)", Some("Windows")),
        ] {
            assert_eq!(device_label(ua).as_deref(), label, "{ua}");
        }
    }
}
