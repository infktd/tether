//! Token Management (AA's): an account's stored SSO tokens, with their
//! scopes, refreshed or deleted by their owner. Tokens themselves never
//! leave the vault.

use std::time::Instant;

use axum::http::StatusCode;
use serde_json::json;
use tether_db::PgPool;
use tether_db::accounts::{self, AccountId, LossCause};
use tether_db::audit::{self, Actor};
use tether_db::tokens::{self, AccountToken};
use tether_esi::vault::{TokenVault, VaultError};

use crate::error::AppError;
use crate::state::Limits;

pub async fn list(db: &PgPool, account: AccountId) -> Result<Vec<AccountToken>, AppError> {
    Ok(tokens::for_account(db, account).await?)
}

/// The account's token for the character (else not found: nobody learns
/// about other accounts' characters).
async fn own(db: &PgPool, account: AccountId, character: i64) -> Result<AccountToken, AppError> {
    list(db, account)
        .await?
        .into_iter()
        .find(|t| t.character_id == character)
        .ok_or_else(|| AppError::not_found("You have no token for that character."))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refreshed {
    /// EVE issued a fresh access token.
    Valid,
    /// EVE says the token is dead: log in with the character again.
    Revoked,
    /// The character now belongs to another EVE account: it left.
    Sold,
}

/// Refreshes a token now, with the ownership check a refresh always does.
pub async fn refresh(
    db: &PgPool,
    vault: &TokenVault,
    limits: &Limits,
    account: AccountId,
    character: i64,
) -> Result<Refreshed, AppError> {
    // Each refresh is a call to EVE SSO from Tether's one client id.
    if let Err(wait) = limits.token_refresh.check(account.0, Instant::now()) {
        return Err(AppError::too_many_requests(wait.as_secs().max(1)));
    }
    own(db, account, character).await?;
    let refreshed = match vault.verify(character).await {
        Ok(()) => Refreshed::Valid,
        Err(VaultError::Revoked | VaultError::NoToken) => Refreshed::Revoked,
        Err(VaultError::OwnerChanged) => {
            // Proof of a sale: it goes at once, as in the ownership check.
            if let Some(lost) = accounts::lose_ownership(db, character, LossCause::Sold).await? {
                crate::ownership::after_lost(db, &lost).await?;
            }
            Refreshed::Sold
        }
        Err(err @ (VaultError::Unavailable(_) | VaultError::NotConfigured)) => {
            tracing::warn!(character, error = %err, "token refresh: SSO unavailable");
            return Err(AppError::new(
                StatusCode::BAD_GATEWAY,
                "EVE SSO didn't answer. Try again in a moment.",
            ));
        }
        Err(err) => {
            tracing::warn!(character, error = %err, "token refresh");
            return Err(AppError::new(
                StatusCode::BAD_GATEWAY,
                "The token couldn't be refreshed. Try again in a moment.",
            ));
        }
    };
    // A dead token can break compliance.
    crate::states::evaluate_account(db, account).await?;
    Ok(refreshed)
}

/// Deletes a token. The character stays until the dead-token rules take
/// it (a day later, unless its owner logs in with it again).
pub async fn delete(
    db: &PgPool,
    vault: &TokenVault,
    account: AccountId,
    character: i64,
) -> Result<(), AppError> {
    own(db, account, character).await?;
    let mut tx = db.begin().await?;
    if !tokens::wipe(&mut *tx, account, character).await? {
        return Err(AppError::not_found("That token is already deleted."));
    }
    audit::record(
        &mut *tx,
        Actor::Account(account),
        "token.delete",
        Some(&format!("character:{character}")),
        json!({}),
    )
    .await?;
    tx.commit().await?;
    vault.forget(character);
    crate::states::evaluate_account(db, account).await?;
    Ok(())
}
