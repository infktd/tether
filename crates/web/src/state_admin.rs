//! Changing the states (F4): create, rename, delete, reorder, and what each
//! covers. Every change can be previewed (which accounts would move where)
//! and is audited; applying one queues a re-evaluation of every account.
//! The web pages, the API and the CLI all come through here.

use std::collections::BTreeSet;

use serde::Serialize;
use serde_json::json;
use tether_core::states::{self as core, EntityKind, State, StateId, StateRules};
use tether_db::PgPool;
use tether_db::audit::{self, Actor};
use tether_db::states as db;
use tether_esi::Esi;

use crate::admin::names_unavailable;
use crate::error::AppError;

/// Most entities one state can list; far more than any real alliance needs.
pub const MAX_COVERED: usize = 500;
/// Highest priority a state can have.
pub const MAX_PRIORITY: i32 = 1_000_000;
/// Most states, Member, Blue and Guest included.
pub const MAX_STATES: i64 = 50;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    Create {
        name: String,
    },
    Rename {
        state: StateId,
        name: String,
    },
    Delete {
        state: StateId,
    },
    /// Swap with the neighbour above (`up`) or below. `past` pins which
    /// neighbour, so a confirmed preview can't turn into a different swap.
    Move {
        state: StateId,
        up: bool,
        past: Option<StateId>,
    },
    Add {
        state: StateId,
        entity_id: i64,
    },
    Remove {
        state: StateId,
        entity_id: i64,
    },
    /// Set a state's priority (higher wins), as AA's editable number.
    SetPriority {
        state: StateId,
        priority: i32,
    },
    /// Require a scope on every character of the state's accounts.
    AddScope {
        state: StateId,
        scope: String,
    },
    RemoveScope {
        state: StateId,
        scope: String,
    },
}

/// Accounts moving between two states.
#[derive(Debug, Clone, Serialize, PartialEq, Eq, utoipa::ToSchema)]
pub struct Move {
    pub from: String,
    pub to: String,
    pub accounts: usize,
}

/// What a change would do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preview {
    /// One line, e.g. "Add Pandemic Horde to Blue".
    pub summary: String,
    pub moves: Vec<Move>,
    /// Something the admin must read before applying, even if nobody
    /// moves yet.
    pub warning: Option<String>,
}

impl Preview {
    pub fn moves_anyone(&self) -> bool {
        !self.moves.is_empty()
    }

    /// Whether to show the confirmation step.
    pub fn needs_confirming(&self) -> bool {
        self.moves_anyone() || self.warning.is_some()
    }
}

/// A resolved alliance, corporation, character or faction, named by ESI (through
/// the names cache), never by the client.
struct Entity {
    id: i64,
    kind: EntityKind,
    name: String,
}

/// NPC corporations (the starter corporations every new character is
/// in): anyone can join one in minutes, so no state may cover them.
fn is_npc_corporation(id: i64) -> bool {
    (1_000_000..2_000_000).contains(&id)
}

async fn entity(db: &PgPool, esi: &Esi, entity_id: i64) -> Result<Entity, AppError> {
    if is_npc_corporation(entity_id) {
        return Err(AppError::bad_request(
            "NPC corporations can't be covered: anyone can join them.",
        ));
    }
    let named =
        tether_esi::names::resolve(db, esi, &[entity_id], tether_esi::Priority::Interactive)
            .await
            .map_err(names_unavailable)?
            .remove(&entity_id)
            .ok_or_else(|| AppError::not_found("EVE doesn't know that id."))?;
    let kind = named.kind().ok_or_else(|| {
        AppError::bad_request("That isn't an alliance, corporation, character or faction.")
    })?;
    Ok(Entity {
        id: named.id,
        kind,
        name: named.name,
    })
}

fn find(states: &[State], id: StateId) -> Result<&State, AppError> {
    states
        .iter()
        .find(|s| s.id == id)
        .ok_or_else(|| AppError::not_found("No such state."))
}

/// As AA: every state can be renamed or deleted, except Guest.
fn check_editable(s: &State, what: &str) -> Result<(), AppError> {
    if s.is_guest() {
        return Err(AppError::bad_request(format!(
            "{} is everyone no other state covers: it can't be {what}.",
            s.name
        )));
    }
    Ok(())
}

fn check_covers(s: &State) -> Result<(), AppError> {
    if s.is_guest() {
        return Err(AppError::bad_request(
            "Guest covers everyone no other state does; it lists nobody.",
        ));
    }
    Ok(())
}

/// Only character scopes from ESI's list, and never for Guest: a
/// corporation scope needs in-game roles most members don't have.
fn check_scope(s: &State, scope: &str) -> Result<(), AppError> {
    if s.is_guest() {
        return Err(AppError::bad_request(
            "Guest is identity only: it requires no scopes.",
        ));
    }
    if tether_core::scopes::is_write(scope) {
        return Err(AppError::bad_request(
            "That scope acts as the character (mail, contacts, fleets...): Tether never requires it.",
        ));
    }
    match tether_core::scopes::info(scope) {
        Some(info) if info.kind == tether_core::scopes::ScopeKind::Character => Ok(()),
        Some(_) => Err(AppError::bad_request(
            "That's a corporation scope: it needs in-game roles most members don't have.",
        )),
        None => Err(AppError::bad_request("That isn't an ESI scope.")),
    }
}

fn check_priority(s: &State, priority: i32, states: &[State]) -> Result<(), AppError> {
    if s.is_guest() {
        return Err(AppError::bad_request(
            "Guest is always 0, below every state.",
        ));
    }
    if !(1..=MAX_PRIORITY).contains(&priority) {
        return Err(AppError::bad_request(
            "Priorities are 1 to 1,000,000 (Guest is 0).",
        ));
    }
    if states
        .iter()
        .any(|o| o.id != s.id && o.priority == priority)
    {
        return Err(priority_taken());
    }
    Ok(())
}

fn priority_taken() -> AppError {
    AppError::new(
        axum::http::StatusCode::CONFLICT,
        "Another state has that priority: each needs its own.",
    )
}

/// The neighbour a state would swap with, if it can move that way.
fn neighbour(states: &[State], id: StateId, up: bool) -> Result<&State, AppError> {
    let at = states
        .iter()
        .position(|s| s.id == id)
        .ok_or_else(|| AppError::not_found("No such state."))?;
    if states[at].is_guest() {
        return Err(AppError::bad_request("Guest is always last."));
    }
    let other = if up {
        at.checked_sub(1).and_then(|i| states.get(i))
    } else {
        states.get(at + 1)
    };
    other
        .filter(|s| !s.is_guest())
        .ok_or_else(|| AppError::bad_request("It can't move further that way."))
}

/// The neighbour for a move, checked against the one the admin saw.
fn pinned_neighbour(
    states: &[State],
    id: StateId,
    up: bool,
    past: Option<StateId>,
) -> Result<&State, AppError> {
    let other = neighbour(states, id, up)?;
    if past.is_some_and(|p| p != other.id) {
        return Err(AppError::new(
            axum::http::StatusCode::CONFLICT,
            "The order changed since you looked. Check the states and try again.",
        ));
    }
    Ok(other)
}

/// The states whose membership a change affects: changing who is in a
/// state hands out (or takes away) everything granted to it.
fn affected(states: &[State], change: &Change) -> Result<Vec<StateId>, AppError> {
    Ok(match change {
        Change::Create { .. } | Change::Rename { .. } => Vec::new(),
        Change::Delete { state }
        | Change::Add { state, .. }
        | Change::Remove { state, .. }
        | Change::AddScope { state, .. }
        | Change::RemoveScope { state, .. } => vec![*state],
        Change::Move { state, up, past } => {
            vec![*state, pinned_neighbour(states, *state, *up, *past)?.id]
        }
        // It changes who is in it and in every state it passes.
        Change::SetPriority { state, priority } => {
            let s = find(states, *state)?;
            let (low, high) = if *priority > s.priority {
                (s.priority, *priority)
            } else {
                (*priority, s.priority)
            };
            states
                .iter()
                .filter(|o| {
                    o.id == *state || (!o.is_guest() && o.priority >= low && o.priority <= high)
                })
                .map(|o| o.id)
                .collect()
        }
    })
}

/// An admin may only change who is in a state if they already hold
/// everything granted to it. Otherwise `admin.states` would be a way to any
/// permission: put your own main in a state that has it. The owner holds
/// everything; the CLI (shell access) and the system are trusted.
async fn check_reach(
    tx: &mut sqlx::PgConnection,
    actor: Actor,
    states: &[State],
    change: &Change,
) -> Result<(), AppError> {
    let Actor::Account(account) = actor else {
        return Ok(());
    };
    let targets = affected(states, change)?;
    if targets.is_empty() {
        return Ok(());
    }
    let mut granted = db::granted_to(&mut *tx, &targets).await?;
    // Who is in a state (and what it requires) also decides who is in the
    // Compliant group, so its grants count too.
    granted.extend(db::granted_to_compliance_groups(&mut *tx).await?);
    granted.sort();
    granted.dedup();
    if granted.is_empty() {
        return Ok(());
    }
    let held: BTreeSet<String> = tether_db::permissions::effective_in(&mut *tx, account).await?;
    let missing: Vec<&String> = granted.iter().filter(|p| !held.contains(*p)).collect();
    if missing.is_empty() {
        return Ok(());
    }
    let names: Vec<&str> = targets
        .iter()
        .filter_map(|id| states.iter().find(|s| s.id == *id))
        .map(|s| s.name.as_str())
        .collect();
    Err(AppError::new(
        axum::http::StatusCode::FORBIDDEN,
        format!(
            "{} grants {}, which you don't hold. Only someone who holds it (or the owner) can change who is in it.",
            names.join(" and "),
            missing
                .iter()
                .map(|p| p.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    ))
}

/// What the change would do, without doing it.
pub async fn preview(db: &PgPool, esi: &Esi, change: &Change) -> Result<Preview, AppError> {
    let mut conn = db.acquire().await?;
    let states = db::list(&mut *conn).await?;
    let before = db::load_rules(&mut conn).await?;
    let mut after = before.clone();
    let mut warning = None;
    let summary = match change {
        Change::Create { name } => {
            let name = core::check_name(name).map_err(AppError::bad_request)?;
            format!("Create {name}")
        }
        Change::Rename { state: id, name } => {
            let s = find(&states, *id)?;
            check_editable(s, "renamed")?;
            let name = core::check_name(name).map_err(AppError::bad_request)?;
            format!("Rename {} to {name}", s.name)
        }
        Change::Delete { state: id } => {
            let s = find(&states, *id)?;
            check_editable(s, "deleted")?;
            after.remove_state(*id);
            format!("Delete {}", s.name)
        }
        Change::Move {
            state: id,
            up,
            past,
        } => {
            let s = find(&states, *id)?;
            let other = pinned_neighbour(&states, *id, *up, *past)?;
            after.set_priority(s.id, other.priority);
            after.set_priority(other.id, s.priority);
            format!(
                "Move {} {} {}",
                s.name,
                if *up { "above" } else { "below" },
                other.name
            )
        }
        Change::SetPriority {
            state: id,
            priority,
        } => {
            let s = find(&states, *id)?;
            check_priority(s, *priority, &states)?;
            after.set_priority(s.id, *priority);
            format!("Set {}'s priority to {priority}", s.name)
        }
        Change::Add {
            state: id,
            entity_id,
        } => {
            let s = find(&states, *id)?;
            check_covers(s)?;
            let e = entity(db, esi, *entity_id).await?;
            after.add(s.id, e.kind, e.id);
            // As AA's Member Factions, but never silently: militias are
            // open to anyone with the standings, and who is enlisted shows
            // only after the next affiliation check.
            if e.kind == EntityKind::Faction {
                warning = Some(format!(
                    "Anyone who enlists in the {} militia joins {}: enlisting takes minutes, and \
                     tens of thousands of pilots already have. Who moves shows after the next \
                     affiliation check (hourly), so the list below may be short.",
                    e.name, s.name
                ));
            }
            format!("Add {} to {}", e.name, s.name)
        }
        Change::Remove {
            state: id,
            entity_id,
        } => {
            let s = find(&states, *id)?;
            let covered = db::covered(&mut *conn).await?;
            let c = covered
                .iter()
                .find(|c| c.state == *id && c.entity_id == *entity_id)
                .ok_or_else(|| AppError::not_found("That state doesn't list it."))?;
            after.remove(s.id, c.kind, c.entity_id);
            format!("Remove {} from {}", c.name, s.name)
        }
        Change::AddScope { state: id, scope } => {
            let s = find(&states, *id)?;
            check_scope(s, scope)?;
            format!("Require {scope} for {}", s.name)
        }
        Change::RemoveScope { state: id, scope } => {
            let s = find(&states, *id)?;
            format!("Stop requiring {scope} for {}", s.name)
        }
    };
    let mains = db::all_mains(db).await?;
    let mut moved = moves(&mains, &states, &before, &after);
    // A new requirement flags everyone who lacks it (and takes them out of
    // the Compliant group) until they register again.
    if let Change::AddScope { state: id, scope } = change {
        let short = tether_db::compliance::accounts_lacking(db, *id, scope).await?;
        if short > 0 {
            moved.push(Move {
                from: format!("{}: compliant", find(&states, *id)?.name),
                to: "Not compliant (keeps its state)".to_owned(),
                accounts: usize::try_from(short).unwrap_or(usize::MAX),
            });
        }
    }
    Ok(Preview {
        summary,
        moves: moved,
        warning,
    })
}

fn moves(
    mains: &[Option<core::Main>],
    states: &[State],
    before: &StateRules,
    after: &StateRules,
) -> Vec<Move> {
    let name = |id: StateId| {
        states
            .iter()
            .find(|s| s.id == id)
            .map_or_else(|| "?".to_owned(), |s| s.name.clone())
    };
    let mut moves: Vec<Move> = core::impact(mains, before, after)
        .into_iter()
        .map(|((from, to), accounts)| Move {
            from: name(from),
            to: name(to),
            accounts,
        })
        .collect();
    moves.sort_by(|a, b| b.accounts.cmp(&a.accounts).then(a.from.cmp(&b.from)));
    moves
}

/// Applies a change, audits it and queues a re-evaluation of every
/// account. Returns the new state's id for `Create`.
pub async fn apply(
    db: &PgPool,
    esi: &Esi,
    actor: Actor,
    change: &Change,
) -> Result<Option<StateId>, AppError> {
    // Resolve outside the transaction: it may call ESI.
    let resolved = match change {
        Change::Add { entity_id, .. } => Some(entity(db, esi, *entity_id).await?),
        _ => None,
    };
    let mut tx = db.begin().await?;
    db::lock(&mut tx).await?;
    let states = db::list(&mut *tx).await?;
    check_reach(&mut tx, actor, &states, change).await?;
    let mut created = None;
    let (action, target, details) = match change {
        Change::Create { name } => {
            let name = core::check_name(name).map_err(AppError::bad_request)?;
            if db::count(&mut *tx).await? >= MAX_STATES {
                return Err(AppError::bad_request(format!(
                    "There can be at most {MAX_STATES} states."
                )));
            }
            let id = db::create(&mut tx, name).await?.ok_or_else(taken)?;
            created = Some(id);
            ("state.create", id, json!({ "name": name }))
        }
        Change::Rename { state: id, name } => {
            let name = core::check_name(name).map_err(AppError::bad_request)?;
            check_editable(find(&states, *id)?, "renamed")?;
            match db::rename(&mut tx, *id, name).await? {
                db::Renamed::Done { from } => {
                    ("state.rename", *id, json!({ "from": from, "to": name }))
                }
                db::Renamed::NotFound => return Err(AppError::not_found("No such state.")),
                db::Renamed::Builtin => {
                    return Err(AppError::bad_request("Guest can't be renamed."));
                }
                db::Renamed::Taken => return Err(taken()),
            }
        }
        Change::Delete { state: id } => {
            check_editable(find(&states, *id)?, "deleted")?;
            match db::delete(&mut tx, *id).await? {
                db::Deleted::Done {
                    name,
                    accounts,
                    grants,
                    roles,
                } => {
                    // Its accounts go straight to where they belong now,
                    // not through Guest (no role flapping), each audited.
                    let rules = db::load_rules(&mut tx).await?;
                    for account in &accounts {
                        crate::states::evaluate_in(&mut tx, &rules, *account, Some(&name)).await?;
                    }
                    (
                        "state.delete",
                        *id,
                        json!({
                            "name": name,
                            "accounts": accounts.len(),
                            "grants_removed": grants,
                            "roles_removed": roles
                                .iter()
                                .map(|(id, name)| json!({ "role_id": id.to_string(), "role": name }))
                                .collect::<Vec<_>>(),
                        }),
                    )
                }
                db::Deleted::NotFound => return Err(AppError::not_found("No such state.")),
                db::Deleted::Builtin => {
                    return Err(AppError::bad_request("Guest can't be deleted."));
                }
            }
        }
        Change::Move {
            state: id,
            up,
            past,
        } => {
            let s = find(&states, *id)?;
            let expected = pinned_neighbour(&states, *id, *up, *past)?.id;
            let other = db::swap(&mut tx, *id, *up)
                .await?
                .filter(|o| o.id == expected)
                .ok_or_else(|| AppError::bad_request("It can't move further that way."))?;
            (
                "state.move",
                *id,
                json!({
                    "name": s.name,
                    "direction": if *up { "up" } else { "down" },
                    "past": other.name,
                }),
            )
        }
        Change::SetPriority {
            state: id,
            priority,
        } => {
            let s = find(&states, *id)?;
            check_priority(s, *priority, &states)?;
            match db::set_priority(&mut tx, *id, *priority).await? {
                db::Reprioritized::Done { from } => (
                    "state.priority",
                    *id,
                    json!({ "name": s.name, "from": from, "to": priority }),
                ),
                db::Reprioritized::NotFound => {
                    return Err(AppError::not_found("No such state."));
                }
                db::Reprioritized::Guest => {
                    return Err(AppError::bad_request("Guest is always 0."));
                }
                db::Reprioritized::Taken => return Err(priority_taken()),
            }
        }
        Change::Add { state: id, .. } => {
            let s = find(&states, *id)?;
            check_covers(s)?;
            let e = resolved.ok_or_else(|| AppError::internal("entity not resolved"))?;
            let count = db::covered(&mut *tx)
                .await?
                .iter()
                .filter(|c| c.state == *id)
                .count();
            if count >= MAX_COVERED {
                return Err(AppError::bad_request(format!(
                    "A state can list at most {MAX_COVERED} alliances, corporations, characters and factions."
                )));
            }
            if !db::add_entity(&mut *tx, *id, e.kind, e.id, &e.name).await? {
                return Err(AppError::new(
                    axum::http::StatusCode::CONFLICT,
                    format!("{} already lists {}.", s.name, e.name),
                ));
            }
            (
                "state.add",
                *id,
                json!({ "state": s.name, "entity_id": e.id, "kind": e.kind.as_str(), "name": e.name }),
            )
        }
        Change::Remove {
            state: id,
            entity_id,
        } => {
            let s = find(&states, *id)?;
            let (kind, name) = db::remove_entity(&mut *tx, *id, *entity_id)
                .await?
                .ok_or_else(|| AppError::not_found("That state doesn't list it."))?;
            (
                "state.remove",
                *id,
                json!({ "state": s.name, "entity_id": entity_id, "kind": kind.as_str(), "name": name }),
            )
        }
        Change::AddScope { state: id, scope } => {
            let s = find(&states, *id)?;
            check_scope(s, scope)?;
            let by = match actor {
                Actor::Account(account) => Some(account),
                _ => None,
            };
            if !tether_db::compliance::add_scope(&mut *tx, *id, scope, by).await? {
                return Err(AppError::new(
                    axum::http::StatusCode::CONFLICT,
                    format!("{} already requires {scope}.", s.name),
                ));
            }
            (
                "state.scope_add",
                *id,
                json!({ "state": s.name, "scope": scope }),
            )
        }
        Change::RemoveScope { state: id, scope } => {
            let s = find(&states, *id)?;
            if !tether_db::compliance::remove_scope(&mut *tx, *id, scope).await? {
                return Err(AppError::new(
                    axum::http::StatusCode::NOT_FOUND,
                    format!("{} doesn't require {scope}.", s.name),
                ));
            }
            (
                "state.scope_remove",
                *id,
                json!({ "state": s.name, "scope": scope }),
            )
        }
    };
    audit::record(
        &mut *tx,
        actor,
        action,
        Some(&format!("state:{}", target.0)),
        details,
    )
    .await?;
    crate::states::enqueue_evaluate_all(&mut *tx).await?;
    tx.commit().await?;
    Ok(created)
}

fn taken() -> AppError {
    AppError::new(
        axum::http::StatusCode::CONFLICT,
        "Another state already has that name.",
    )
}

#[cfg(test)]
mod tests {
    use super::is_npc_corporation;

    #[test]
    fn npc_corporations_are_recognised() {
        assert!(is_npc_corporation(1_000_167));
        assert!(!is_npc_corporation(98_133_756));
        assert!(!is_npc_corporation(99_000_001));
        assert!(!is_npc_corporation(2_112_000_000));
    }
}
