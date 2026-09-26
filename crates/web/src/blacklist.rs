//! The Blacklist and the Pilot Log (AA's blacklist app). Blacklisting a
//! character, corporation or alliance puts every account whose main it
//! covers in the Blacklist state, above every other: no permissions, no
//! groups, no services. The owner can never be blacklisted.

use serde_json::json;
use tether_core::states::EntityKind;
use tether_db::accounts::AccountId;
use tether_db::audit::{self, Actor};
use tether_db::blacklist as db;

use crate::AppState;
use crate::admin::{esi_unavailable, names_unavailable};
use crate::error::AppError;

/// An entity named by ESI, never by the client.
pub struct Found {
    pub id: i64,
    pub kind: EntityKind,
    pub name: String,
}

/// Finds a character, corporation or alliance by exact name or by id.
pub async fn find(state: &AppState, text: &str) -> Result<Found, AppError> {
    let text = text.trim();
    if text.is_empty() || text.chars().count() > 100 {
        return Err(AppError::bad_request("Enter a name or an id."));
    }
    if let Ok(id) = text.parse::<i64>() {
        let named = tether_esi::names::resolve(
            &state.db,
            &state.esi,
            &[id],
            tether_esi::Priority::Interactive,
        )
        .await
        .map_err(names_unavailable)?
        .remove(&id)
        .ok_or_else(|| AppError::not_found("EVE doesn't know that id."))?;
        let kind = named
            .kind()
            .filter(|k| *k != EntityKind::Faction)
            .ok_or_else(|| AppError::bad_request("That isn't a pilot, corporation or alliance."))?;
        return Ok(Found {
            id: named.id,
            kind,
            name: named.name,
        });
    }
    let resolved = state
        .esi
        .resolve_names(&[text.to_owned()], tether_esi::Priority::Interactive)
        .await
        .map_err(esi_unavailable)?;
    let mut found: Vec<Found> = [
        (EntityKind::Character, resolved.characters),
        (EntityKind::Corporation, resolved.corporations),
        (EntityKind::Alliance, resolved.alliances),
    ]
    .into_iter()
    .flat_map(|(kind, list)| {
        list.into_iter().map(move |e| Found {
            id: e.id,
            kind,
            name: e.name,
        })
    })
    .collect();
    match found.len() {
        0 => Err(AppError::not_found(
            "No pilot, corporation or alliance has exactly that name.",
        )),
        1 => Ok(found.remove(0)),
        _ => Err(AppError::bad_request(
            "More than one has that name: enter the id instead.",
        )),
    }
}

async fn actor_name(state: &AppState, actor: AccountId) -> Result<String, AppError> {
    Ok(tether_db::accounts::get(&state.db, actor)
        .await?
        .and_then(|a| a.main)
        .map_or_else(|| format!("account {}", actor.0), |m| m.name))
}

fn text(label: &str, value: &str, max: usize) -> Result<String, AppError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(AppError::bad_request(format!("Write a {label}.")));
    }
    if value.chars().count() > max {
        return Err(AppError::bad_request(format!(
            "A {label} is at most {max} characters."
        )));
    }
    Ok(value.to_owned())
}

pub async fn add(
    state: &AppState,
    actor: AccountId,
    who: &str,
    reason: &str,
) -> Result<(), AppError> {
    let reason = text("reason", reason, 1000)?;
    let npc = |id: i64| (1_000_000..2_000_000).contains(&id);
    let refuse_npc = || {
        AppError::bad_request("NPC corporations can't be blacklisted: every new pilot is in one.")
    };
    // Before asking EVE, when the id says it already.
    if who.trim().parse::<i64>().is_ok_and(npc) {
        return Err(refuse_npc());
    }
    let found = find(state, who).await?;
    if found.kind == EntityKind::Corporation && npc(found.id) {
        return Err(refuse_npc());
    }
    let by = actor_name(state, actor).await?;
    let mut tx = state.db.begin().await?;
    // The same lock order as every evaluation: the states first.
    tether_db::states::lock_shared(&mut tx).await?;
    if db::covers_owner(&mut *tx, found.id).await? {
        return Err(AppError::bad_request(
            "That would blacklist the owner's main, which can't be.",
        ));
    }
    // As deactivating: never a way past what you hold.
    let covered = db::accounts_covered(&mut *tx, found.id).await?;
    let mine = tether_db::permissions::effective_in(&mut tx, actor).await?;
    for account in &covered {
        let theirs = tether_db::permissions::effective_in(&mut tx, *account).await?;
        // Blacklisting holds nothing, as deactivating: stripping someone
        // with sensitive powers needs a recent login too (sudo mode).
        if theirs
            .iter()
            .any(|p| tether_core::permissions::is_sensitive(p))
        {
            crate::sudo::check(crate::sudo::Action::AccountDeactivate)?;
        }
        if let Some(missing) = theirs.iter().find(|p| !mine.contains(*p)) {
            return Err(AppError::new(
                axum::http::StatusCode::FORBIDDEN,
                format!(
                    "That would blacklist an account holding {missing}, which you don't, so you can't."
                ),
            ));
        }
    }
    if !db::add(
        &mut *tx,
        db::NewListing {
            entity_id: found.id,
            kind: found.kind,
            name: &found.name,
            reason: &reason,
            added_by: actor,
            added_by_name: &by,
        },
    )
    .await?
    {
        return Err(AppError::new(
            axum::http::StatusCode::CONFLICT,
            "Already blacklisted.",
        ));
    }
    audit::record(
        &mut *tx,
        Actor::Account(actor),
        "blacklist.add",
        Some(&format!("{}:{}", found.kind.as_str(), found.id)),
        json!({ "name": found.name, "reason": reason, "accounts": covered.len() }),
    )
    .await?;
    // Everyone it covers moves now, in this transaction.
    reevaluate(&mut tx, &covered).await?;
    tx.commit().await?;
    Ok(())
}

/// Re-evaluates accounts inside the caller's transaction (which holds
/// the states' shared lock), and queues a full pass as a backstop.
async fn reevaluate(
    tx: &mut sqlx::PgTransaction<'_>,
    accounts: &[AccountId],
) -> Result<(), AppError> {
    let rules = tether_db::states::load_rules(&mut *tx).await?;
    for account in accounts {
        crate::states::evaluate_in(&mut *tx, &rules, *account, None).await?;
    }
    crate::states::enqueue_evaluate_all(&mut **tx).await?;
    Ok(())
}

pub async fn remove(state: &AppState, actor: AccountId, entity_id: i64) -> Result<(), AppError> {
    let mut tx = state.db.begin().await?;
    tether_db::states::lock_shared(&mut tx).await?;
    let covered = db::accounts_covered(&mut *tx, entity_id).await?;
    let (kind, name) = db::remove(&mut *tx, entity_id)
        .await?
        .ok_or_else(|| AppError::not_found("Not blacklisted."))?;
    audit::record(
        &mut *tx,
        Actor::Account(actor),
        "blacklist.remove",
        Some(&format!("{kind}:{entity_id}")),
        json!({ "name": name, "accounts": covered.len() }),
    )
    .await?;
    reevaluate(&mut tx, &covered).await?;
    tx.commit().await?;
    Ok(())
}

pub async fn add_note(
    state: &AppState,
    actor: AccountId,
    about: &str,
    note: &str,
) -> Result<(), AppError> {
    let note = text("note", note, 2000)?;
    let found = find(state, about).await?;
    let by = actor_name(state, actor).await?;
    let mut tx = state.db.begin().await?;
    let id = db::add_note(
        &mut *tx,
        db::NewNote {
            entity_id: found.id,
            kind: found.kind,
            name: &found.name,
            note: &note,
            added_by: actor,
            added_by_name: &by,
        },
    )
    .await?;
    audit::record(
        &mut *tx,
        Actor::Account(actor),
        "pilot_note.add",
        Some(&format!("pilot_note:{id}")),
        json!({ "about": found.name, "entity_id": found.id }),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Authors delete their own notes; `manage` deletes anyone's.
pub async fn delete_note(
    state: &AppState,
    actor: AccountId,
    manage: bool,
    id: i64,
) -> Result<(), AppError> {
    let mut tx = state.db.begin().await?;
    let note = db::note(&mut *tx, id)
        .await?
        .ok_or_else(|| AppError::not_found("No such note."))?;
    if !manage && note.added_by != Some(actor.0) {
        return Err(AppError::forbidden());
    }
    db::delete_note(&mut *tx, id).await?;
    audit::record(
        &mut *tx,
        Actor::Account(actor),
        "pilot_note.delete",
        Some(&format!("pilot_note:{id}")),
        // Not the text: the audit log isn't the Pilot Log.
        json!({ "about": note.name, "entity_id": note.entity_id, "author": note.added_by_name }),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}
