//! Plugin data sources (F16): users offer characters as a plugin's data
//! source, which an admin approves, through an EVE SSO login that asks for
//! the plugin's data-source scopes plus those the account already granted
//! (so a new grant never drops an old one). User scopes need no step here:
//! Member requires them (see `compliance`). Also the Discord channels a
//! plugin may post to. Every change is audited.

use axum::response::Response;
use axum_extra::extract::CookieJar;
use serde_json::json;
use tether_db::accounts::AccountId;
use tether_db::audit::{self, Actor};
use tether_db::auth::Purpose;
use tether_db::plugin_esi as db;
use tether_esi::sso::SsoIdentity;

use crate::AppState;
use crate::error::AppError;

fn target(plugin: &str) -> String {
    format!("plugin:{plugin}")
}

/// Starts the login that offers a character as `plugin`'s data source.
pub async fn start_offer(
    state: &AppState,
    jar: CookieJar,
    account: AccountId,
    plugin: &str,
) -> Result<Response, AppError> {
    let running = state
        .plugins
        .running(plugin)
        .ok_or_else(|| AppError::not_found("No such app is running."))?;
    let wanted = &running.manifest.capabilities.esi.data_source;
    if wanted.is_empty() {
        return Err(AppError::bad_request("That app uses no data sources."));
    }
    let scopes = crate::compliance::ask_scopes(&state.db, account, wanted.iter().cloned()).await?;
    crate::auth::start_login(
        state,
        jar,
        "/dashboard",
        Purpose::DataSource(plugin.to_owned()),
        &scopes,
        Some(account),
    )
    .await
}

/// After an offer login, in the callback: records it, if the character
/// is on the signed-in account and SSO granted every scope the plugin
/// needs.
pub async fn finish(
    state: &AppState,
    account: AccountId,
    identity: &SsoIdentity,
    plugin: &str,
) -> Result<(), AppError> {
    let running = state
        .plugins
        .running(plugin)
        .ok_or_else(|| AppError::not_found("That app isn't running any more."))?;
    let needed = &running.manifest.capabilities.esi.data_source;
    let missing: Vec<&String> = needed
        .iter()
        .filter(|s| !identity.scopes.contains(s))
        .collect();
    if !missing.is_empty() {
        return Err(AppError::bad_request(format!(
            "EVE didn't grant {}. An admin may need to enable these scopes on Tether's EVE \
             application (developers.eveonline.com).",
            missing
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )));
    }
    let owner = db::character_account(&state.db, identity.character_id).await?;
    if owner != Some(account) {
        return Err(AppError::bad_request(
            "That character isn't on your account.",
        ));
    }
    let mut tx = state.db.begin().await?;
    db::offer_data_source(&mut *tx, plugin, identity.character_id, account).await?;
    audit::record(
        &mut *tx,
        Actor::Account(account),
        "plugin.data_source_offered",
        Some(&target(plugin)),
        json!({ "character_id": identity.character_id, "scopes": needed }),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Checks a character is the account's own.
async fn own(state: &AppState, account: AccountId, character: i64) -> Result<(), AppError> {
    match db::character_account(&state.db, character).await? {
        Some(owner) if owner == account => Ok(()),
        _ => Err(AppError::not_found("That character isn't on your account.")),
    }
}

/// The owner withdraws a character from being a data source.
pub async fn withdraw_offer(
    state: &AppState,
    account: AccountId,
    plugin: &str,
    character: i64,
) -> Result<(), AppError> {
    own(state, account, character).await?;
    remove_source(
        state,
        account,
        plugin,
        character,
        "plugin.data_source_withdrawn",
    )
    .await
}

async fn remove_source(
    state: &AppState,
    actor: AccountId,
    plugin: &str,
    character: i64,
    action: &str,
) -> Result<(), AppError> {
    let mut tx = state.db.begin().await?;
    if !db::remove_data_source(&mut *tx, plugin, character).await? {
        return Err(AppError::not_found("That character isn't a data source."));
    }
    audit::record(
        &mut *tx,
        Actor::Account(actor),
        action,
        Some(&target(plugin)),
        json!({ "character_id": character }),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// An admin approves an offered data source.
pub async fn approve_source(
    state: &AppState,
    admin: AccountId,
    plugin: &str,
    character: i64,
) -> Result<(), AppError> {
    let mut tx = state.db.begin().await?;
    if !db::approve_data_source(&mut *tx, plugin, character, admin).await? {
        return Err(AppError::not_found("That character wasn't offered."));
    }
    audit::record(
        &mut *tx,
        Actor::Account(admin),
        "plugin.data_source_approved",
        Some(&target(plugin)),
        json!({ "character_id": character }),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// An admin removes a data source (offered or approved).
pub async fn remove_source_as_admin(
    state: &AppState,
    admin: AccountId,
    plugin: &str,
    character: i64,
) -> Result<(), AppError> {
    remove_source(
        state,
        admin,
        plugin,
        character,
        "plugin.data_source_removed",
    )
    .await
}

/// An admin lets a plugin post to one of the ping channels, or stops it.
pub async fn set_channel(
    state: &AppState,
    admin: AccountId,
    plugin: &str,
    channel: i64,
    assigned: bool,
) -> Result<(), AppError> {
    let mut tx = state.db.begin().await?;
    if assigned {
        let config = crate::discord::config(state).await?;
        let guild = i64::try_from(config.guild_id).map_err(AppError::internal)?;
        if !tether_db::pings::is_channel(&mut *tx, channel, guild).await? {
            return Err(AppError::bad_request(
                "Choose one of the ping channels (set them up under Discord).",
            ));
        }
        if !db::assign_channel(&mut *tx, plugin, channel).await? {
            return Ok(());
        }
    } else if !db::unassign_channel(&mut *tx, plugin, channel).await? {
        return Err(AppError::not_found("That channel isn't assigned."));
    }
    audit::record(
        &mut *tx,
        Actor::Account(admin),
        if assigned {
            "plugin.channel_assigned"
        } else {
            "plugin.channel_removed"
        },
        Some(&target(plugin)),
        json!({ "channel_id": channel }),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}
