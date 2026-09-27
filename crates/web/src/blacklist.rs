//! The Blacklist and the Pilot Log, as allianceauth-blacklist. Notes on
//! pilots, corporations and alliances carry a reason and three flags:
//! blacklisted, restricted and ultra restricted; comments on them carry
//! the same tiers. Its 16 permissions decide who sees and adds which.
//!
//! As AA, blacklisting goes by the main: an account whose main is, or is
//! in, a blacklisted pilot, corporation or alliance is in the Blacklist
//! state, above every other, and that state is all it changes. The
//! account holds what the Blacklist state is granted (never a sensitive
//! permission) plus its own grants and those of groups that don't exclude
//! the state, and keeps a service only while something grants it access.
//! To strip someone completely, deactivate them. A superuser can never be
//! blacklisted, nor an account holding permissions the blacklister lacks;
//! blacklisting (either way) needs what the Blacklist state is granted;
//! and a blacklisted account can't Change Main its way out.

use serde_json::json;
use tether_core::permissions as p;
use tether_core::states::{Builtin, EntityKind};
use tether_db::accounts::AccountId;
use tether_db::audit::{self, Actor};
use tether_db::blacklist::{self as db, Note, Reader};

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

/// The account's main's corporation, as last seen.
async fn main_corporation(state: &AppState, account: AccountId) -> Result<Option<i64>, AppError> {
    Ok(tether_db::states::main(&state.db, account)
        .await?
        .and_then(|m| m.affiliation)
        .map(|a| a.corporation_id))
}

/// Which notes and comments an account may see.
pub struct Access {
    pub reader: Reader,
    pub blacklist: bool,
    pub comments: bool,
    pub restricted_comments: bool,
    pub ultra_comments: bool,
    pub add_basic: bool,
    pub add: bool,
    pub add_to_blacklist: bool,
    pub add_restricted: bool,
    pub add_ultra: bool,
    pub comment: bool,
    pub comment_restricted: bool,
    pub comment_ultra: bool,
    /// A superuser: deletes notes and comments (AA's Django admin).
    pub owner: bool,
}

impl Access {
    /// Whether the Pilot Log shows anything to them.
    pub fn notes(&self) -> bool {
        self.reader.all || self.reader.corporation.is_some()
    }

    pub fn adds(&self) -> bool {
        self.add || self.add_basic
    }
}

pub async fn access(state: &AppState, account: AccountId) -> Result<Access, AppError> {
    let held = tether_db::permissions::effective(&state.db, account).await?;
    let has = |permission: &str| held.contains(permission);
    let corporation = if has(p::BLACKLIST_VIEW_BASIC_NOTES) || has(p::BLACKLIST_ADD_BASIC_NOTES) {
        main_corporation(state, account).await?
    } else {
        None
    };
    let owner = tether_db::accounts::get(&state.db, account)
        .await?
        .is_some_and(|a| a.is_owner)
        && tether_db::permissions::token_scope().is_none();
    Ok(Access {
        reader: Reader {
            all: has(p::BLACKLIST_VIEW_NOTES),
            corporation: corporation.filter(|_| has(p::BLACKLIST_VIEW_BASIC_NOTES)),
            restricted: has(p::BLACKLIST_VIEW_RESTRICTED),
            ultra_restricted: has(p::BLACKLIST_VIEW_ULTRA),
        },
        blacklist: has(p::BLACKLIST_VIEW_BLACKLIST),
        comments: has(p::BLACKLIST_VIEW_COMMENTS),
        restricted_comments: has(p::BLACKLIST_VIEW_RESTRICTED_COMMENTS),
        ultra_comments: has(p::BLACKLIST_VIEW_ULTRA_COMMENTS),
        add_basic: has(p::BLACKLIST_ADD_BASIC_NOTES),
        add: has(p::BLACKLIST_ADD_NOTES),
        add_to_blacklist: has(p::BLACKLIST_ADD_TO_BLACKLIST),
        add_restricted: has(p::BLACKLIST_ADD_RESTRICTED),
        add_ultra: has(p::BLACKLIST_ADD_ULTRA),
        comment: has(p::BLACKLIST_ADD_COMMENTS),
        comment_restricted: has(p::BLACKLIST_ADD_RESTRICTED_COMMENTS),
        comment_ultra: has(p::BLACKLIST_ADD_ULTRA_COMMENTS),
        owner,
    })
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

fn refused(what: &str) -> AppError {
    AppError::new(
        axum::http::StatusCode::FORBIDDEN,
        format!("You can't {what}."),
    )
}

/// The flags a note or comment asks for, checked against what the actor
/// may set.
#[derive(Debug, Clone, Copy, Default)]
pub struct Flags {
    pub blacklisted: bool,
    pub restricted: bool,
    pub ultra_restricted: bool,
}

/// A new note, as typed (AA's add note form).
#[derive(Debug, Clone, Default)]
pub struct NewNote {
    pub who: String,
    pub reason: String,
    pub flags: Flags,
    /// Also note every other character on the pilot's account (AA's "all
    /// linked characters"; pilots only).
    pub linked: bool,
}

/// Changing who is blacklisted is changing who is in the Blacklist state:
/// never a superuser, never past what the actor holds (as deactivating), and
/// only with everything the Blacklist state is granted (as `admin.states`).
async fn check_blacklisting(
    tx: &mut sqlx::PgConnection,
    actor: AccountId,
    entities: &[i64],
    adding: bool,
) -> Result<Vec<AccountId>, AppError> {
    if adding && db::covers_owner(&mut *tx, entities).await? {
        return Err(AppError::bad_request(
            "That would blacklist a superuser's main, which can't be.",
        ));
    }
    let covered = db::accounts_covered(&mut *tx, entities).await?;
    let mine = tether_db::permissions::effective_in(&mut *tx, actor).await?;
    if let Some(blacklist) = tether_db::states::builtin(&mut *tx, Builtin::Blacklist).await? {
        let granted = tether_db::states::granted_to(&mut *tx, &[blacklist.id]).await?;
        if let Some(missing) = granted.iter().find(|g| !mine.contains(*g)) {
            return Err(AppError::new(
                axum::http::StatusCode::FORBIDDEN,
                format!(
                    "{} grants {missing}, which you don't hold, so you can't change who is in it.",
                    blacklist.name
                ),
            ));
        }
    }
    if adding {
        for account in &covered {
            let theirs = tether_db::permissions::effective_in(&mut *tx, *account).await?;
            // Blacklisting takes their state's grants away, as deactivating
            // does: stripping someone with sensitive powers needs a recent
            // login too (sudo mode).
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
    }
    Ok(covered)
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

/// A pilot's current corporation and alliance, with their names.
type Affiliation = (Option<(i64, Option<String>)>, Option<(i64, Option<String>)>);

async fn affiliation_of(state: &AppState, found: &Found) -> Result<Affiliation, AppError> {
    match found.kind {
        EntityKind::Character => {
            let affiliation = state
                .esi
                .affiliations(&[found.id], tether_esi::Priority::Interactive)
                .await
                .map_err(esi_unavailable)?
                .into_iter()
                .find(|a| a.character_id == found.id)
                .ok_or_else(|| AppError::not_found("EVE doesn't know that pilot."))?;
            let ids: Vec<i64> = std::iter::once(affiliation.corporation_id)
                .chain(affiliation.alliance_id)
                .collect();
            let names = tether_esi::names::resolve(
                &state.db,
                &state.esi,
                &ids,
                tether_esi::Priority::Interactive,
            )
            .await
            .unwrap_or_default();
            let named = |id: i64| (id, names.get(&id).map(|n| n.name.clone()));
            Ok((
                Some(named(affiliation.corporation_id)),
                affiliation.alliance_id.map(named),
            ))
        }
        EntityKind::Corporation => Ok((Some((found.id, Some(found.name.clone()))), None)),
        EntityKind::Alliance => Ok((None, Some((found.id, Some(found.name.clone()))))),
        EntityKind::Faction => Err(AppError::bad_request(
            "That isn't a pilot, corporation or alliance.",
        )),
    }
}

/// Adds a note (AA's add note), and with `linked`, one on every other
/// character of the pilot's account. Returns the first note's id.
pub async fn add_note(state: &AppState, actor: AccountId, new: &NewNote) -> Result<i64, AppError> {
    let access = access(state, actor).await?;
    if !access.adds() {
        return Err(refused("add notes"));
    }
    let reason = text("reason", &new.reason, 2000)?;
    let flags = new.flags;
    if flags.blacklisted && !access.add_to_blacklist {
        return Err(refused("blacklist"));
    }
    if flags.restricted && !access.add_restricted {
        return Err(refused("add restricted notes"));
    }
    if flags.ultra_restricted && !access.add_ultra {
        return Err(refused("add ultra restricted notes"));
    }
    let found = find(state, &new.who).await?;
    let (corporation, alliance) = affiliation_of(state, &found).await?;
    // `add_basic_eve_notes`: pilots in your main's corporation only.
    if !access.add {
        let mine = main_corporation(state, actor).await?;
        if found.kind != EntityKind::Character
            || mine.is_none()
            || corporation.as_ref().map(|(id, _)| *id) != mine
        {
            return Err(AppError::new(
                axum::http::StatusCode::FORBIDDEN,
                "You can only add notes on pilots in your own corporation. Ask someone who can \
                 add any note.",
            ));
        }
    }
    let mut linked = if new.linked && found.kind == EntityKind::Character {
        db::linked_characters(&state.db, found.id).await?
    } else {
        Vec::new()
    };
    // Basic notes stay in the actor's own corporation, linked ones too.
    if !access.add {
        let mine = main_corporation(state, actor).await?;
        linked.retain(|(_, _, corporation, _)| corporation.is_some() && *corporation == mine);
    }
    let by = actor_name(state, actor).await?;
    let mut tx = state.db.begin().await?;
    // The same lock order as every evaluation: the states first.
    tether_db::states::lock_shared(&mut tx).await?;
    let entities: Vec<i64> = std::iter::once(found.id)
        .chain(linked.iter().map(|(id, ..)| *id))
        .collect();
    let covered = if flags.blacklisted {
        check_blacklisting(&mut tx, actor, &entities, true).await?
    } else {
        Vec::new()
    };
    let id = db::add_note(
        &mut *tx,
        db::NewNote {
            entity_id: found.id,
            kind: found.kind,
            name: &found.name,
            note: &reason,
            blacklisted: flags.blacklisted,
            restricted: flags.restricted,
            ultra_restricted: flags.ultra_restricted,
            corporation: corporation.as_ref().map(|(id, n)| (*id, n.as_deref())),
            alliance: alliance.as_ref().map(|(id, n)| (*id, n.as_deref())),
            added_by: actor,
            added_by_name: &by,
        },
    )
    .await?;
    let linked_reason = format!("Linked: {} - {reason}", found.name);
    let linked_reason: String = linked_reason.chars().take(2000).collect();
    for (character, name, corporation, alliance) in &linked {
        db::add_note(
            &mut *tx,
            db::NewNote {
                entity_id: *character,
                kind: EntityKind::Character,
                name,
                note: &linked_reason,
                blacklisted: flags.blacklisted,
                restricted: flags.restricted,
                ultra_restricted: flags.ultra_restricted,
                corporation: corporation.map(|c| (c, None)),
                alliance: alliance.map(|a| (a, None)),
                added_by: actor,
                added_by_name: &by,
            },
        )
        .await?;
    }
    audit::record(
        &mut *tx,
        Actor::Account(actor),
        if flags.blacklisted {
            "blacklist.add"
        } else {
            "pilot_note.add"
        },
        Some(&format!("{}:{}", found.kind.as_str(), found.id)),
        // Not the reason: the audit log isn't the Pilot Log, and a
        // restricted note stays restricted (not even named).
        json!({
            "note": id,
            "name": (!flags.restricted && !flags.ultra_restricted).then_some(&found.name),
            "blacklisted": flags.blacklisted,
            "restricted": flags.restricted,
            "ultra_restricted": flags.ultra_restricted,
            "linked": linked.len(),
            "accounts": covered.len(),
        }),
    )
    .await?;
    if flags.blacklisted {
        // Everyone it covers moves now, in this transaction.
        reevaluate(&mut tx, &covered).await?;
    }
    tx.commit().await?;
    Ok(id)
}

/// The note, if the actor may see it in the Pilot Log.
async fn visible_note(state: &AppState, access: &Access, id: i64) -> Result<Note, AppError> {
    db::note(&state.db, id)
        .await?
        .filter(|n| access.reader.sees(n))
        .ok_or_else(|| AppError::not_found("No such note."))
}

/// Edits a note (AA's edit note, for `add_new_eve_notes`): its reason, and
/// each flag the actor may set; the others stay as they are.
pub async fn edit_note(
    state: &AppState,
    actor: AccountId,
    id: i64,
    reason: &str,
    flags: Flags,
) -> Result<(), AppError> {
    let access = access(state, actor).await?;
    if !access.add {
        return Err(refused("edit notes"));
    }
    let reason = text("reason", reason, 2000)?;
    visible_note(state, &access, id).await?;
    let mut tx = state.db.begin().await?;
    tether_db::states::lock_shared(&mut tx).await?;
    // Read again under the note's lock: flags the actor can't set are kept
    // as they are now, not as they were before a concurrent change.
    let before = db::note_locked(&mut tx, id)
        .await?
        .filter(|n| access.reader.sees(n))
        .ok_or_else(|| AppError::not_found("No such note."))?;
    let blacklisted = if access.add_to_blacklist {
        flags.blacklisted
    } else {
        before.blacklisted
    };
    let restricted = if access.add_restricted {
        flags.restricted
    } else {
        before.restricted
    };
    let ultra_restricted = if access.add_ultra {
        flags.ultra_restricted
    } else {
        before.ultra_restricted
    };
    let changes_state = blacklisted != before.blacklisted;
    let covered = if changes_state {
        check_blacklisting(&mut tx, actor, &[before.entity_id], blacklisted).await?
    } else {
        Vec::new()
    };
    db::edit_note(
        &mut *tx,
        id,
        &reason,
        blacklisted,
        restricted,
        ultra_restricted,
    )
    .await?;
    audit::record(
        &mut *tx,
        Actor::Account(actor),
        match (changes_state, blacklisted) {
            (true, true) => "blacklist.add",
            (true, false) => "blacklist.remove",
            _ => "pilot_note.edit",
        },
        Some(&format!("{}:{}", before.kind.as_str(), before.entity_id)),
        json!({
            "note": id,
            "name": (!restricted && !ultra_restricted).then_some(&before.name),
            "blacklisted": blacklisted,
            "restricted": restricted,
            "ultra_restricted": ultra_restricted,
            "accounts": covered.len(),
        }),
    )
    .await?;
    if changes_state {
        reevaluate(&mut tx, &covered).await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Takes an entity off the Blacklist: every blacklisted note on it stays,
/// no longer blacklisted (AA: untick Blacklist on the note).
pub async fn unblacklist(
    state: &AppState,
    actor: AccountId,
    entity_id: i64,
) -> Result<(), AppError> {
    let access = access(state, actor).await?;
    if !access.add || !access.add_to_blacklist {
        return Err(refused("take anyone off the Blacklist"));
    }
    let mut tx = state.db.begin().await?;
    tether_db::states::lock_shared(&mut tx).await?;
    if !db::entity_blacklisted(&mut *tx, entity_id).await? {
        return Err(AppError::not_found("Not blacklisted."));
    }
    // Only blacklistings the actor may read: a restricted one's tier
    // decides who undoes it, as editing the note would.
    let listed = db::notes(
        &mut *tx,
        &access.reader,
        db::Filter {
            about: Some(&[entity_id]),
            blacklist: true,
            ..db::Filter::default()
        },
        1000,
    )
    .await?;
    if listed.iter().any(|n| !access.reader.reads_reason(n)) {
        return Err(AppError::new(
            axum::http::StatusCode::FORBIDDEN,
            "A restricted note blacklists them: only someone who can read it can take them off.",
        ));
    }
    let covered = check_blacklisting(&mut tx, actor, &[entity_id], false).await?;
    let (kind, name, notes) = db::unblacklist(&mut *tx, entity_id)
        .await?
        .ok_or_else(|| AppError::not_found("Not blacklisted."))?;
    audit::record(
        &mut *tx,
        Actor::Account(actor),
        "blacklist.remove",
        Some(&format!("{kind}:{entity_id}")),
        json!({ "name": name, "notes": notes, "accounts": covered.len() }),
    )
    .await?;
    reevaluate(&mut tx, &covered).await?;
    tx.commit().await?;
    Ok(())
}

/// Deletes a note and its comments: superusers only (AA: the Django
/// admin).
pub async fn delete_note(state: &AppState, actor: AccountId, id: i64) -> Result<(), AppError> {
    let access = access(state, actor).await?;
    if !access.owner {
        return Err(refused("delete notes: only a superuser can"));
    }
    let mut tx = state.db.begin().await?;
    tether_db::states::lock_shared(&mut tx).await?;
    let note = db::note(&mut *tx, id)
        .await?
        .ok_or_else(|| AppError::not_found("No such note."))?;
    let covered = if note.blacklisted {
        check_blacklisting(&mut tx, actor, &[note.entity_id], false).await?
    } else {
        Vec::new()
    };
    db::delete_note(&mut *tx, id).await?;
    audit::record(
        &mut *tx,
        Actor::Account(actor),
        "pilot_note.delete",
        Some(&format!("pilot_note:{id}")),
        // Not the text: the audit log isn't the Pilot Log.
        json!({
            "about": (!note.restricted && !note.ultra_restricted).then_some(&note.name),
            "entity_id": note.entity_id,
            "author": note.added_by_name,
            "blacklisted": note.blacklisted,
        }),
    )
    .await?;
    if note.blacklisted {
        reevaluate(&mut tx, &covered).await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Comments on a note the actor may see (AA's add comment).
pub async fn add_comment(
    state: &AppState,
    actor: AccountId,
    note: i64,
    comment: &str,
    flags: Flags,
) -> Result<(), AppError> {
    let access = access(state, actor).await?;
    if !access.comment {
        return Err(refused("comment on notes"));
    }
    if flags.restricted && !access.comment_restricted {
        return Err(refused("add restricted comments"));
    }
    if flags.ultra_restricted && !access.comment_ultra {
        return Err(refused("add ultra restricted comments"));
    }
    let comment = text("comment", comment, 2000)?;
    let found = visible_note(state, &access, note).await?;
    let by = actor_name(state, actor).await?;
    let mut tx = state.db.begin().await?;
    let id = db::add_comment(
        &mut *tx,
        db::NewComment {
            note_id: found.id,
            comment: &comment,
            restricted: flags.restricted,
            ultra_restricted: flags.ultra_restricted,
            added_by: actor,
            added_by_name: &by,
        },
    )
    .await?;
    audit::record(
        &mut *tx,
        Actor::Account(actor),
        "pilot_note.comment",
        Some(&format!("pilot_note:{}", found.id)),
        json!({
            "comment": id,
            "about": (!found.restricted && !found.ultra_restricted).then_some(&found.name),
            "restricted": flags.restricted,
            "ultra_restricted": flags.ultra_restricted,
        }),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}
