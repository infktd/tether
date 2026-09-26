//! Personal access tokens (F19): an account's tokens for bots and scripts.
//! Made and revoked only in a browser session (a token can never make
//! another); shown once; stored hashed. A token's scopes are some of the
//! account's permissions, and `account:read`; at use it holds only those
//! the account still holds.

use chrono::Utc;
use serde_json::json;
use tether_core::{Secret, hash_token, new_token};
use tether_db::accounts::AccountId;
use tether_db::audit::{self, Actor};
use tether_db::personal_tokens as db;

use crate::AppState;
use crate::auth::{ACCOUNT_READ, PAT_PREFIX};
use crate::error::AppError;

pub const MAX_TOKENS: i64 = 20;
pub const MAX_DAYS: i64 = 365;

/// What a token may be given: `account:read`, and the account's
/// permissions.
pub async fn offered(state: &AppState, account: AccountId) -> Result<Vec<String>, AppError> {
    let mut scopes = vec![ACCOUNT_READ.to_owned()];
    scopes.extend(tether_db::permissions::effective(&state.db, account).await?);
    Ok(scopes)
}

/// Makes a token; returns it, the only time it's shown.
pub async fn create(
    state: &AppState,
    actor: AccountId,
    name: &str,
    scopes: &[String],
    days: &str,
) -> Result<Secret<String>, AppError> {
    crate::sudo::check(crate::sudo::Action::AccessToken)?;
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 60 || name.chars().any(char::is_control) {
        return Err(AppError::bad_request(
            "Name the token (at most 60 characters), so you know what uses it.",
        ));
    }
    let days: i64 = days
        .trim()
        .parse()
        .ok()
        .filter(|d| (1..=MAX_DAYS).contains(d))
        .ok_or_else(|| AppError::bad_request(format!("Tokens last 1 to {MAX_DAYS} days.")))?;
    let offered = offered(state, actor).await?;
    let mut chosen: Vec<String> = scopes.to_vec();
    chosen.sort();
    chosen.dedup();
    if chosen.is_empty() {
        return Err(AppError::bad_request("Choose what the token may do."));
    }
    if chosen.len() > 100 {
        return Err(AppError::bad_request("A token carries at most 100 scopes."));
    }
    if let Some(extra) = chosen.iter().find(|s| !offered.contains(s)) {
        return Err(AppError::bad_request(format!(
            "You don't hold {extra}, so a token of yours can't either."
        )));
    }
    let secret = new_token().map_err(AppError::internal)?;
    let token = Secret::new(format!("{PAT_PREFIX}{}", secret.expose()));
    let prefix: String = token.expose().chars().take(PAT_PREFIX.len() + 6).collect();
    let mut tx = state.db.begin().await?;
    // Counted under the account's row lock.
    tether_db::pings::lock_sender(&mut tx, actor).await?;
    if db::count(&mut *tx, actor).await? >= MAX_TOKENS {
        return Err(AppError::bad_request(format!(
            "An account has at most {MAX_TOKENS} tokens: revoke one first."
        )));
    }
    let id = db::insert(
        &mut *tx,
        db::NewToken {
            account: actor,
            name,
            token_hash: &hash_token(token.expose()),
            prefix: &prefix,
            scopes: &chosen,
            expires_at: Utc::now() + chrono::Duration::days(days),
        },
    )
    .await?;
    audit::record(
        &mut *tx,
        Actor::Account(actor),
        "access_token.create",
        Some(&format!("access_token:{id}")),
        json!({ "name": name, "scopes": chosen, "days": days, "prefix": prefix }),
    )
    .await?;
    tx.commit().await?;
    Ok(token)
}

pub async fn revoke(state: &AppState, actor: AccountId, id: i64) -> Result<(), AppError> {
    let mut tx = state.db.begin().await?;
    let name = db::revoke(&mut *tx, actor, id)
        .await?
        .ok_or_else(|| AppError::not_found("No such token."))?;
    audit::record(
        &mut *tx,
        Actor::Account(actor),
        "access_token.revoke",
        Some(&format!("access_token:{id}")),
        json!({ "name": name }),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}
