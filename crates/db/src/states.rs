//! Access states, what they cover, character affiliations and account
//! states.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use tether_core::states::{Affiliation, Builtin, EntityKind, Main, State, StateId, StateRules};

use crate::PgPool;
use crate::accounts::AccountId;

fn state(id: i64, name: String, builtin: Option<String>, priority: i32) -> State {
    State {
        id: StateId(id),
        name,
        builtin: builtin.as_deref().and_then(Builtin::parse),
        priority,
    }
}

/// Every state, highest priority first (Guest last).
pub async fn list<'e>(executor: impl sqlx::PgExecutor<'e>) -> Result<Vec<State>, sqlx::Error> {
    let rows =
        sqlx::query!("SELECT id, name, builtin, priority FROM core.states ORDER BY priority DESC")
            .fetch_all(executor)
            .await?;
    Ok(rows
        .into_iter()
        .map(|r| state(r.id, r.name, r.builtin, r.priority))
        .collect())
}

pub async fn get<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    id: StateId,
) -> Result<Option<State>, sqlx::Error> {
    let row = sqlx::query!(
        "SELECT id, name, builtin, priority FROM core.states WHERE id = $1",
        id.0
    )
    .fetch_optional(executor)
    .await?;
    Ok(row.map(|r| state(r.id, r.name, r.builtin, r.priority)))
}

/// A state by name, ignoring case.
pub async fn by_name<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    name: &str,
) -> Result<Option<State>, sqlx::Error> {
    let row = sqlx::query!(
        "SELECT id, name, builtin, priority FROM core.states WHERE lower(name) = lower($1)",
        name
    )
    .fetch_optional(executor)
    .await?;
    Ok(row.map(|r| state(r.id, r.name, r.builtin, r.priority)))
}

/// A built-in state. Member and Blue can be deleted (as in AA), so they
/// may be gone; Guest never is.
pub async fn builtin<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    which: Builtin,
) -> Result<Option<State>, sqlx::Error> {
    let r = sqlx::query!(
        "SELECT id, name, builtin, priority FROM core.states WHERE builtin = $1",
        which.as_str()
    )
    .fetch_optional(executor)
    .await?;
    Ok(r.map(|r| state(r.id, r.name, r.builtin, r.priority)))
}

/// An alliance, corporation or character a state covers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Covered {
    pub state: StateId,
    pub entity_id: i64,
    pub kind: EntityKind,
    pub name: String,
    pub added_at: DateTime<Utc>,
}

/// What every state covers, by state, then kind, then name.
pub async fn covered<'e>(executor: impl sqlx::PgExecutor<'e>) -> Result<Vec<Covered>, sqlx::Error> {
    let rows = sqlx::query!(
        r#"
        SELECT state_id, entity_id, entity_kind, name, added_at
        FROM core.state_entities
        ORDER BY state_id, entity_kind, lower(name)
        "#
    )
    .fetch_all(executor)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|r| {
            Some(Covered {
                state: StateId(r.state_id),
                entity_id: r.entity_id,
                kind: EntityKind::parse(&r.entity_kind)?,
                name: r.name,
                added_at: r.added_at,
            })
        })
        .collect())
}

pub async fn load_rules(conn: &mut sqlx::PgConnection) -> Result<StateRules, sqlx::Error> {
    let states = list(&mut *conn).await?;
    let guest = states
        .iter()
        .find(|s| s.is_guest())
        .map(|s| s.id)
        .ok_or(sqlx::Error::RowNotFound)?;
    let mut rules = StateRules::new(guest);
    for s in &states {
        rules.add_state(s.id, s.priority);
    }
    for c in covered(&mut *conn).await? {
        rules.add(c.state, c.kind, c.entity_id);
    }
    Ok(rules)
}

/// Locks every state row until the transaction ends, so changes to the
/// order and to what states cover happen one at a time.
pub async fn lock(tx: &mut sqlx::PgConnection) -> Result<(), sqlx::Error> {
    sqlx::query!("SELECT id FROM core.states FOR UPDATE")
        .fetch_all(tx)
        .await?;
    Ok(())
}

/// Holds the states steady (shared) until the transaction ends, so an
/// evaluation always sees committed rules and can't interleave with a
/// change.
pub async fn lock_shared(tx: &mut sqlx::PgConnection) -> Result<(), sqlx::Error> {
    sqlx::query!("SELECT id FROM core.states FOR SHARE")
        .fetch_all(tx)
        .await?;
    Ok(())
}

/// The permissions granted to any of these states.
pub async fn granted_to<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    states: &[StateId],
) -> Result<Vec<String>, sqlx::Error> {
    let ids: Vec<i64> = states.iter().map(|s| s.0).collect();
    sqlx::query_scalar!(
        r#"SELECT DISTINCT permission AS "permission!" FROM core.permission_grants WHERE state_id = ANY($1) ORDER BY 1"#,
        &ids
    )
    .fetch_all(executor)
    .await
}

/// The permissions granted to compliance groups (Tether keeps their
/// members).
pub async fn granted_to_compliance_groups<'e>(
    executor: impl sqlx::PgExecutor<'e>,
) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar!(
        r#"
        SELECT DISTINCT g.permission AS "permission!"
        FROM core.permission_grants g
        JOIN core.groups gr ON gr.id = g.group_id
        WHERE gr.compliance
        "#
    )
    .fetch_all(executor)
    .await
}

/// How many states there are.
pub async fn count<'e>(executor: impl sqlx::PgExecutor<'e>) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar!(r#"SELECT count(*) AS "count!" FROM core.states"#)
        .fetch_one(executor)
        .await
}

/// Creates a state just above Guest, so it only covers pilots nothing
/// else does until an admin moves it up. `None` if the name is taken.
pub async fn create(
    tx: &mut sqlx::PgConnection,
    name: &str,
) -> Result<Option<StateId>, sqlx::Error> {
    lock(&mut *tx).await?;
    let taken = sqlx::query_scalar!(
        r#"SELECT true AS "taken!" FROM core.states WHERE lower(name) = lower($1)"#,
        name
    )
    .fetch_optional(&mut *tx)
    .await?;
    if taken.is_some() {
        return Ok(None);
    }
    // Halfway between Guest and the lowest state; only when there's no
    // room left (the lowest is 1) does everything move up one.
    let lowest = sqlx::query_scalar!("SELECT min(priority) FROM core.states WHERE priority > 0")
        .fetch_one(&mut *tx)
        .await?;
    let priority = match lowest {
        None => 100,
        Some(l) if l > 1 => l / 2,
        Some(_) => {
            sqlx::query!("UPDATE core.states SET priority = priority + 1 WHERE priority > 0")
                .execute(&mut *tx)
                .await?;
            1
        }
    };
    let id = sqlx::query_scalar!(
        "INSERT INTO core.states (name, priority) VALUES ($1, $2) RETURNING id",
        name,
        priority
    )
    .fetch_one(&mut *tx)
    .await?;
    Ok(Some(StateId(id)))
}

pub enum Renamed {
    Done { from: String },
    NotFound,
    Builtin,
    Taken,
}

pub async fn rename(
    tx: &mut sqlx::PgConnection,
    id: StateId,
    name: &str,
) -> Result<Renamed, sqlx::Error> {
    lock(&mut *tx).await?;
    let Some(current) = get(&mut *tx, id).await? else {
        return Ok(Renamed::NotFound);
    };
    if current.is_guest() {
        return Ok(Renamed::Builtin);
    }
    let taken = sqlx::query_scalar!(
        r#"SELECT true AS "taken!" FROM core.states WHERE lower(name) = lower($1) AND id <> $2"#,
        name,
        id.0
    )
    .fetch_optional(&mut *tx)
    .await?;
    if taken.is_some() {
        return Ok(Renamed::Taken);
    }
    sqlx::query!("UPDATE core.states SET name = $2 WHERE id = $1", id.0, name)
        .execute(&mut *tx)
        .await?;
    Ok(Renamed::Done { from: current.name })
}

pub enum Deleted {
    Done {
        name: String,
        /// Accounts that were in it, now Guest until the caller
        /// re-evaluates them.
        accounts: Vec<AccountId>,
        /// Permissions that were granted to it.
        grants: Vec<String>,
        /// Discord roles that were mapped to it: `(role_id, role_name)`.
        roles: Vec<(i64, String)>,
    },
    NotFound,
    Builtin,
}

/// Deletes a state (any but Guest), with its grants and role mappings
/// (returned for the audit log). Its accounts become Guest; the caller
/// re-evaluates them in the same transaction.
pub async fn delete(tx: &mut sqlx::PgConnection, id: StateId) -> Result<Deleted, sqlx::Error> {
    lock(&mut *tx).await?;
    let Some(current) = get(&mut *tx, id).await? else {
        return Ok(Deleted::NotFound);
    };
    if current.is_guest() {
        return Ok(Deleted::Builtin);
    }
    let accounts = sqlx::query_scalar!(
        "UPDATE core.accounts SET state_id = core.guest_state() WHERE state_id = $1 RETURNING id",
        id.0
    )
    .fetch_all(&mut *tx)
    .await?;
    let grants = sqlx::query_scalar!(
        "DELETE FROM core.permission_grants WHERE state_id = $1 RETURNING permission",
        id.0
    )
    .fetch_all(&mut *tx)
    .await?;
    let roles = sqlx::query!(
        "DELETE FROM core.discord_role_mappings WHERE state_id = $1 RETURNING role_id, role_name",
        id.0
    )
    .fetch_all(&mut *tx)
    .await?;
    sqlx::query!("DELETE FROM core.states WHERE id = $1", id.0)
        .execute(&mut *tx)
        .await?;
    Ok(Deleted::Done {
        name: current.name,
        accounts: accounts.into_iter().map(AccountId).collect(),
        grants,
        roles: roles
            .into_iter()
            .map(|r| (r.role_id, r.role_name))
            .collect(),
    })
}

pub enum Reprioritized {
    Done {
        from: i32,
    },
    NotFound,
    Guest,
    /// Another state has it.
    Taken,
}

/// Sets a state's priority (any positive number no other state has).
pub async fn set_priority(
    tx: &mut sqlx::PgConnection,
    id: StateId,
    priority: i32,
) -> Result<Reprioritized, sqlx::Error> {
    lock(&mut *tx).await?;
    let Some(current) = get(&mut *tx, id).await? else {
        return Ok(Reprioritized::NotFound);
    };
    if current.is_guest() {
        return Ok(Reprioritized::Guest);
    }
    let taken = sqlx::query_scalar!(
        r#"SELECT true AS "taken!" FROM core.states WHERE priority = $1 AND id <> $2"#,
        priority,
        id.0
    )
    .fetch_optional(&mut *tx)
    .await?;
    if taken.is_some() {
        return Ok(Reprioritized::Taken);
    }
    sqlx::query!(
        "UPDATE core.states SET priority = $2 WHERE id = $1",
        id.0,
        priority
    )
    .execute(&mut *tx)
    .await?;
    Ok(Reprioritized::Done {
        from: current.priority,
    })
}

/// Swaps a state with its neighbour above (`up`) or below. Guest never
/// moves and nothing moves below it. Returns the neighbour, if any.
pub async fn swap(
    tx: &mut sqlx::PgConnection,
    id: StateId,
    up: bool,
) -> Result<Option<State>, sqlx::Error> {
    lock(&mut *tx).await?;
    let states = list(&mut *tx).await?;
    let Some(at) = states.iter().position(|s| s.id == id && !s.is_guest()) else {
        return Ok(None);
    };
    // `states` is highest first, so "up" is the previous entry.
    let other = if up {
        at.checked_sub(1).and_then(|i| states.get(i))
    } else {
        states.get(at + 1)
    };
    let Some(other) = other.filter(|s| !s.is_guest()) else {
        return Ok(None);
    };
    let this = &states[at];
    sqlx::query!(
        r#"
        UPDATE core.states SET priority = CASE id WHEN $1 THEN $4::integer WHEN $3 THEN $2::integer END
        WHERE id IN ($1, $3)
        "#,
        this.id.0,
        this.priority,
        other.id.0,
        other.priority,
    )
    .execute(&mut *tx)
    .await?;
    Ok(Some(other.clone()))
}

/// Adds an entity to a state. False if it's already there.
pub async fn add_entity<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    state: StateId,
    kind: EntityKind,
    entity_id: i64,
    name: &str,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query!(
        r#"
        INSERT INTO core.state_entities (state_id, entity_id, entity_kind, name)
        VALUES ($1, $2, $3, $4)
        ON CONFLICT DO NOTHING
        "#,
        state.0,
        entity_id,
        kind.as_str(),
        name,
    )
    .execute(executor)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// Removes an entity from a state, returning it.
pub async fn remove_entity<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    state: StateId,
    entity_id: i64,
) -> Result<Option<(EntityKind, String)>, sqlx::Error> {
    let row = sqlx::query!(
        r#"
        DELETE FROM core.state_entities WHERE state_id = $1 AND entity_id = $2
        RETURNING entity_kind, name
        "#,
        state.0,
        entity_id,
    )
    .fetch_optional(executor)
    .await?;
    Ok(row.and_then(|r| Some((EntityKind::parse(&r.entity_kind)?, r.name))))
}

/// How many accounts are in each state right now.
pub async fn counts(pool: &PgPool) -> Result<HashMap<StateId, i64>, sqlx::Error> {
    let rows = sqlx::query!(
        r#"SELECT state_id, count(*) AS "count!" FROM core.accounts GROUP BY state_id"#
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| (StateId(r.state_id), r.count))
        .collect())
}

/// Every account's main, for previewing a change.
pub async fn all_mains(pool: &PgPool) -> Result<Vec<Option<Main>>, sqlx::Error> {
    let rows = sqlx::query!(
        r#"
        SELECT c.id AS "character_id?", c.corporation_id, c.alliance_id, c.faction_id
        FROM core.accounts a
        LEFT JOIN core.characters c ON c.id = a.main_character_id
        "#
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            main_from(
                r.character_id,
                r.corporation_id,
                r.alliance_id,
                r.faction_id,
            )
        })
        .collect())
}

fn main_from(
    character_id: Option<i64>,
    corporation_id: Option<i64>,
    alliance_id: Option<i64>,
    faction_id: Option<i64>,
) -> Option<Main> {
    Some(Main {
        character_id: character_id?,
        affiliation: corporation_id.map(|corporation_id| Affiliation {
            corporation_id,
            alliance_id,
            faction_id,
        }),
    })
}

/// Every linked character, for the affiliation sync.
pub async fn all_character_ids(pool: &PgPool) -> Result<Vec<i64>, sqlx::Error> {
    sqlx::query_scalar!("SELECT id FROM core.characters ORDER BY id")
        .fetch_all(pool)
        .await
}

pub async fn character_ids(pool: &PgPool, account: AccountId) -> Result<Vec<i64>, sqlx::Error> {
    sqlx::query_scalar!(
        "SELECT id FROM core.characters WHERE account_id = $1 ORDER BY id",
        account.0
    )
    .fetch_all(pool)
    .await
}

/// Stores fresh affiliations. Ids not in `core.characters` are ignored.
pub async fn update_affiliations(
    pool: &PgPool,
    affiliations: &[(i64, Affiliation)],
) -> Result<(), sqlx::Error> {
    let ids: Vec<i64> = affiliations.iter().map(|(id, _)| *id).collect();
    let corps: Vec<i64> = affiliations.iter().map(|(_, a)| a.corporation_id).collect();
    let alliances: Vec<Option<i64>> = affiliations.iter().map(|(_, a)| a.alliance_id).collect();
    let factions: Vec<Option<i64>> = affiliations.iter().map(|(_, a)| a.faction_id).collect();
    sqlx::query!(
        r#"
        UPDATE core.characters c
        SET corporation_id = u.corporation_id,
            alliance_id = u.alliance_id,
            faction_id = u.faction_id,
            affiliation_checked_at = now()
        FROM UNNEST($1::bigint[], $2::bigint[], $3::bigint[], $4::bigint[])
            AS u(id, corporation_id, alliance_id, faction_id)
        WHERE c.id = u.id
        "#,
        &ids,
        &corps,
        &alliances as &[Option<i64>],
        &factions as &[Option<i64>],
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// The account's main, with its last known affiliation.
pub async fn main<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    account: AccountId,
) -> Result<Option<Main>, sqlx::Error> {
    let row = sqlx::query!(
        r#"
        SELECT c.id, c.corporation_id, c.alliance_id, c.faction_id
        FROM core.accounts a
        JOIN core.characters c ON c.id = a.main_character_id
        WHERE a.id = $1
        "#,
        account.0,
    )
    .fetch_optional(executor)
    .await?;
    Ok(row.and_then(|r| main_from(Some(r.id), r.corporation_id, r.alliance_id, r.faction_id)))
}

/// Records the account's state and whether it's compliant; returns the
/// previous `(state, compliant)`.
pub async fn set_account_state<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    account: AccountId,
    state: StateId,
    compliant: bool,
) -> Result<Option<(StateId, bool)>, sqlx::Error> {
    let previous = sqlx::query!(
        r#"
        UPDATE core.accounts new
        SET state_id = $2, compliant = $3, state_evaluated_at = now()
        FROM core.accounts old
        WHERE new.id = $1 AND old.id = new.id
        RETURNING old.state_id, old.compliant
        "#,
        account.0,
        state.0,
        compliant,
    )
    .fetch_optional(executor)
    .await?;
    Ok(previous.map(|r| (StateId(r.state_id), r.compliant)))
}

/// The account's current state.
pub async fn account_state<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    account: AccountId,
) -> Result<Option<State>, sqlx::Error> {
    let row = sqlx::query!(
        r#"
        SELECT s.id, s.name, s.builtin, s.priority
        FROM core.accounts a JOIN core.states s ON s.id = a.state_id
        WHERE a.id = $1
        "#,
        account.0
    )
    .fetch_optional(executor)
    .await?;
    Ok(row.map(|r| state(r.id, r.name, r.builtin, r.priority)))
}
