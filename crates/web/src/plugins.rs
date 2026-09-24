//! Plugins: trusting package signers (F15).
//!
//! A package's publisher key is pinned on first install. After that, only
//! a rotation the pinned key signed moves the pin, or an admin re-pinning
//! it by hand after typing the plugin's id to confirm (for a publisher who
//! lost their key). Every change is audited in the same transaction.

use serde_json::json;
use sqlx::PgConnection;
use tether_db::PgPool;
use tether_db::audit::{self, Actor};
use tether_db::plugin_keys::{self, PinnedBy};
use tether_plugins::manifest;
use tether_plugins::package::{Trust, Verified};

use crate::error::AppError;

const CHANGED_MEANWHILE: &str =
    "This plugin's publisher key changed while the package was being checked. Upload it again.";

/// Records the key a verified package was trusted with, in the install's
/// transaction. Check the package against the pin read by
/// [`plugin_keys::get_locked`] for [`Unverified::id`] in that same
/// transaction; each case here also re-checks the pin atomically, so a
/// stale check fails rather than overwrites.
///
/// [`Unverified::id`]: tether_plugins::package::Unverified::id
pub async fn record_trust(
    tx: &mut PgConnection,
    actor: Actor,
    verified: &Verified,
) -> Result<(), AppError> {
    let plugin = verified.package().manifest.plugin.id.as_str();
    let key = verified.key();
    match verified.trust() {
        Trust::Pinned => {
            if plugin_keys::get_locked(tx, plugin).await?.as_deref() != Some(key) {
                return Err(AppError::bad_request(CHANGED_MEANWHILE));
            }
        }
        Trust::FirstInstall => {
            if !plugin_keys::pin_first(&mut *tx, plugin, key).await? {
                return Err(AppError::bad_request(CHANGED_MEANWHILE));
            }
            audit::record(
                &mut *tx,
                actor,
                "plugin.key_pinned",
                Some(plugin),
                json!({ "key": key }),
            )
            .await?;
        }
        Trust::Rotated { from } => {
            if !plugin_keys::replace(&mut *tx, plugin, from, key, PinnedBy::Rotation).await? {
                return Err(AppError::bad_request(CHANGED_MEANWHILE));
            }
            audit::record(
                &mut *tx,
                actor,
                "plugin.key_rotated",
                Some(plugin),
                json!({ "old": from, "new": key }),
            )
            .await?;
        }
    }
    Ok(())
}

/// Replaces a plugin's pinned key by hand, for when a publisher lost the
/// old key and can't sign a rotation. `confirmation` must be the plugin's
/// id, typed by the admin, and `expected_old` the key the admin was shown,
/// so a pin that changed in the meantime isn't overwritten unseen.
pub async fn repin_key(
    db: &PgPool,
    actor: Actor,
    plugin_id: &str,
    expected_old: &str,
    new_key: &str,
    confirmation: &str,
) -> Result<(), AppError> {
    if confirmation.trim() != plugin_id {
        return Err(AppError::bad_request(
            "Type the plugin's id exactly to confirm replacing its key.",
        ));
    }
    let new_key = new_key.trim();
    manifest::check_key(new_key)
        .map_err(|_| AppError::bad_request("That isn't a minisign public key."))?;
    if new_key == expected_old {
        return Err(AppError::bad_request("That key is already pinned."));
    }
    let mut tx = db.begin().await?;
    match plugin_keys::get_locked(&mut tx, plugin_id).await? {
        None => return Err(AppError::not_found("No key is pinned for that plugin.")),
        Some(current) if current != expected_old => {
            return Err(AppError::bad_request(
                "The pinned key changed since you looked. Check it again.",
            ));
        }
        Some(_) => {}
    }
    if !plugin_keys::replace(&mut *tx, plugin_id, expected_old, new_key, PinnedBy::Repin).await? {
        return Err(AppError::bad_request(
            "The pinned key changed since you looked. Check it again.",
        ));
    }
    audit::record(
        &mut *tx,
        actor,
        "plugin.key_repinned",
        Some(plugin_id),
        json!({ "old": expected_old, "new": new_key }),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}
