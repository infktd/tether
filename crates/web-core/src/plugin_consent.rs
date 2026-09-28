//! Plugin data sources (F16), Alliance Auth style: pilots with the app's
//! add-owner permission ([`may_offer`]) add their own characters as a
//! plugin's data source (AA's Add Owner, on the app's own page) through an
//! EVE SSO login that asks for the plugin's data-source scopes plus those
//! the account already granted (so a new grant never drops an old one).
//! It's in use at once, with no admin approval (Jay, 2026-09-26: AA's
//! permissions and behaviour); admins see and remove any, and owners
//! withdraw their own. User scopes need no step here: pilots register
//! characters for the app (see `compliance`). Also the Discord channels a plugin may post to.
//! Every change is audited.

use axum::http::StatusCode;
use axum::response::Response;
use axum_extra::extract::CookieJar;
use serde_json::json;
use tether_db::accounts::AccountId;
use tether_db::audit::{self, Actor};
use tether_db::auth::Purpose;
use tether_db::plugin_esi as db;
use tether_esi::sso::SsoIdentity;
use tether_plugins::manifest::Manifest;

use crate::AppState;
use crate::error::AppError;
use crate::states;

fn target(plugin: &str) -> String {
    format!("plugin:{plugin}")
}

/// The app's permissions that add owners: those its manifest names
/// (`owner_permissions`, for AA names such as aa-contacts'
/// `manage_alliance_contacts`), else its `add_…` ones, as AA's
/// `add_refinery_owner` and `add_structure_owner` and aa-afat's
/// `add_fatlink` (whose FCs add their fleet boss). In AA only these add
/// owners, not an app's general management permission.
pub fn owner_permissions(manifest: &Manifest) -> Vec<&str> {
    manifest.owner_permissions()
}

/// Who may add a character as an app's data source (AA's Add Owner):
/// holders of one of [`owner_permissions`], and app admins
/// (`admin.plugins`). Only for those who may open the app's main page,
/// where their owners are listed.
pub fn may_offer(manifest: &Manifest, holds: impl Fn(&str) -> bool) -> bool {
    if manifest.capabilities.esi.data_source.is_empty() {
        return false;
    }
    // Not knowing the Blacklist here is fine: offering needs one of the
    // app's add_ permissions (or admin.plugins) anyway, which the
    // Blacklist holds only if an admin granted it.
    if !crate::plugins::may_open(&manifest.page_access(""), false, &holds) {
        return false;
    }
    let id = &manifest.plugin.id;
    holds(tether_core::permissions::ADMIN_PLUGINS)
        || owner_permissions(manifest)
            .into_iter()
            .any(|name| holds(&format!("plugin.{id}.{name}")))
}

/// Whether the account may offer characters to `manifest`'s app now.
async fn account_may_offer(
    state: &AppState,
    account: AccountId,
    manifest: &Manifest,
) -> Result<bool, AppError> {
    let held = tether_db::permissions::effective(&state.db, account).await?;
    Ok(may_offer(manifest, |p| held.contains(p)))
}

/// Refuses an owner the account may not add (any more): checked when the
/// login starts, and again when it comes back, before the character is
/// linked or its token kept.
pub async fn check_offer(
    state: &AppState,
    account: AccountId,
    plugin: &str,
) -> Result<(), AppError> {
    let running = state
        .plugins
        .running(plugin)
        .ok_or_else(|| AppError::not_found("That app isn't running any more."))?;
    if account_may_offer(state, account, &running.manifest).await? {
        Ok(())
    } else {
        Err(AppError::forbidden())
    }
}

/// Starts the login that adds a character as `plugin`'s data source, for
/// an account that may (see [`may_offer`]); back to the app's page `back`
/// (a checked link path; its main page when empty), with the character's
/// id as `owner` in the query.
pub async fn start_offer(
    state: &AppState,
    jar: CookieJar,
    account: AccountId,
    plugin: &str,
    back: &str,
) -> Result<Response, AppError> {
    let running = state
        .plugins
        .running(plugin)
        .ok_or_else(|| AppError::not_found("No such app is running."))?;
    let wanted = &running.manifest.capabilities.esi.data_source;
    if wanted.is_empty() {
        return Err(AppError::bad_request("That app uses no data sources."));
    }
    check_offer(state, account, plugin).await?;
    let scopes = crate::compliance::ask_scopes(&state.db, account, wanted.iter().cloned()).await?;
    tether_plugins::page::check_link_path(back)
        .map_err(|_| AppError::bad_request("That isn't one of the app's pages."))?;
    crate::auth::start_login(
        state,
        jar,
        &crate::plugins::page_href(plugin, back),
        Purpose::DataSource(plugin.to_owned()),
        &scopes,
        Some(account),
    )
    .await
}

/// After an Add owner login, in the callback: adds the character, in use
/// at once, if it's on the signed-in account and SSO granted every scope
/// the plugin needs, for the corporation EVE says it's in now (a character
/// new to Tether too); then the app's schedules run now, so it reads its
/// new owner at once.
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
    // Again: the permission may have gone while the pilot was at EVE.
    if !account_may_offer(state, account, &running.manifest).await? {
        return Err(AppError::forbidden());
    }
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
    // The source is for the corporation the character is in now. One
    // Tether has just met has none stored yet (the login's state refresh
    // comes after this), so ask EVE first: the owner is in use from the
    // first add. If EVE doesn't answer, a known character keeps its stored
    // corporation.
    if let Err(err) = states::refresh_affiliations(
        &state.db,
        &state.esi,
        &[identity.character_id],
        tether_esi::Priority::Interactive,
    )
    .await
    {
        tracing::warn!(
            character_id = identity.character_id,
            error = %err,
            "affiliation for a new owner failed"
        );
    }
    let mut tx = state.db.begin().await?;
    let corporation =
        match db::add_data_source(&mut *tx, plugin, identity.character_id, account).await? {
            Some(Some(corporation)) => corporation,
            Some(None) => {
                // No corporation to read for: added, it would be in use
                // for nothing. Nothing is written (the transaction drops),
                // but the character stays linked with its token, as after
                // a login during an ESI outage: its state now from what's
                // stored, and the account's affiliations again shortly.
                drop(tx);
                states::enqueue_refresh(&state.db, account).await?;
                states::evaluate_account(&state.db, account).await?;
                return Err(AppError::new(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "EVE didn't say which corporation that character is in just now. Add it \
                     again in a few minutes.",
                ));
            }
            None => return Err(AppError::not_found("That character isn't known.")),
        };
    audit::record(
        &mut *tx,
        Actor::Account(account),
        "plugin.data_source_added",
        Some(&target(plugin)),
        json!({
            "character_id": identity.character_id,
            "corporation_id": corporation,
            "scopes": needed,
        }),
    )
    .await?;
    tx.commit().await?;
    // The app reads its new owner now, not at its next scheduled run: the
    // pilot's doing, so audited as theirs. At most every minute for app
    // admins (as their Run now), every ten for everyone else (as a
    // registration's), so adding owners can't keep an app running. Best
    // effort: the owner stands whatever happens here.
    let admin = tether_db::permissions::effective(&state.db, account)
        .await?
        .contains(tether_core::permissions::ADMIN_PLUGINS);
    let gap = if admin {
        tether_jobs::schedule::RUN_NOW_GAP
    } else {
        crate::plugin_jobs::TRIGGERED_GAP
    };
    let why = json!({ "reason": "data_source_added", "character_id": identity.character_id });
    crate::plugin_jobs::run_app_schedules(
        &state.db,
        &running.manifest,
        Actor::Account(account),
        &why,
        gap,
    )
    .await;
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

/// An admin removes a data source.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(permissions: &str, sources: bool) -> Manifest {
        let esi = if sources {
            "[capabilities.esi]\ndata_source = [\"esi-industry.read_corporation_mining.v1\"]\n\n"
        } else {
            ""
        };
        Manifest::parse(&format!(
            "[plugin]\nid = \"acme.mine\"\nname = \"Mine\"\nversion = \"1.0.0\"\nhost_api = \"1\"\n\n\
             [capabilities]\n\n{esi}[permissions]\n{permissions}\n\n\
             [[pages]]\npath = \"\"\npermission = \"view\"\n"
        ))
        .unwrap()
    }

    #[test]
    fn owners_are_added_by_add_permissions_and_app_admins() {
        let m = manifest(
            "view = \"v\"\nmanage = \"m\"\nadd_fatlink = \"a\"\nother = \"o\"",
            true,
        );
        assert_eq!(owner_permissions(&m), vec!["add_fatlink"]);
        let with = |held: &[&str]| may_offer(&m, |p| held.contains(&p));
        assert!(!with(&["plugin.acme.mine.view"]));
        assert!(!with(&["plugin.acme.mine.view", "plugin.acme.mine.other"]));
        // As in AA, managing an app isn't adding its owners.
        assert!(!with(&["plugin.acme.mine.view", "plugin.acme.mine.manage"]));
        assert!(with(&[
            "plugin.acme.mine.view",
            "plugin.acme.mine.add_fatlink"
        ]));
        assert!(with(&["plugin.acme.mine.view", "admin.plugins"]));
        // Not without the main page, where the login comes back to.
        assert!(!with(&["plugin.acme.mine.add_fatlink"]));
        // Another app's permission of the same name is no use.
        assert!(!with(&[
            "plugin.acme.mine.view",
            "plugin.acme.other.add_fatlink"
        ]));
        // A manifest may name its own (AA's names, as aa-contacts').
        let named = Manifest::parse(
            "[plugin]\nid = \"acme.contacts\"\nname = \"C\"\nversion = \"1.0.0\"\nhost_api = \"1\"\n\n\
             [capabilities.esi]\ndata_source = [\"esi-corporations.read_contacts.v1\"]\n\
             owner_permissions = [\"manage_corporation_contacts\"]\n\n\
             [permissions]\nview = \"v\"\nmanage_corporation_contacts = \"m\"\nadd_other = \"a\"\n\n\
             [[pages]]\npath = \"\"\npermission = \"view\"\n",
        )
        .unwrap();
        assert_eq!(
            owner_permissions(&named),
            vec!["manage_corporation_contacts"]
        );
        let named_with = |held: &[&str]| may_offer(&named, |p| held.contains(&p));
        assert!(named_with(&[
            "plugin.acme.contacts.view",
            "plugin.acme.contacts.manage_corporation_contacts"
        ]));
        // Then add_ ones don't.
        assert!(!named_with(&[
            "plugin.acme.contacts.view",
            "plugin.acme.contacts.add_other"
        ]));
        // Nothing to offer to an app without data sources.
        let none = manifest("view = \"v\"\nmanage = \"m\"", false);
        assert!(!may_offer(&none, |_| true));
    }
}
