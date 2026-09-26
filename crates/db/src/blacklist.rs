//! The Blacklist and the Pilot Log (AA's blacklist app): blacklisted
//! characters, corporations and alliances, and notes on any of them.

use chrono::{DateTime, Utc};
use tether_core::states::EntityKind;

use crate::accounts::AccountId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listed {
    pub entity_id: i64,
    pub kind: EntityKind,
    pub name: String,
    pub reason: String,
    pub added_by_name: String,
    pub added_at: DateTime<Utc>,
}

pub async fn list<'e>(executor: impl sqlx::PgExecutor<'e>) -> Result<Vec<Listed>, sqlx::Error> {
    let rows = sqlx::query!(
        r#"
        SELECT entity_id, entity_kind, name, reason, added_by_name, added_at
        FROM core.blacklist ORDER BY added_at DESC
        "#
    )
    .fetch_all(executor)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|r| {
            Some(Listed {
                entity_id: r.entity_id,
                kind: EntityKind::parse(&r.entity_kind)?,
                name: r.name,
                reason: r.reason,
                added_by_name: r.added_by_name,
                added_at: r.added_at,
            })
        })
        .collect())
}

/// Whether an account is blacklisted (any of its characters, or their
/// corporation or alliance, is listed; never the owner).
pub async fn is_blacklisted<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    account: AccountId,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar!(r#"SELECT core.blacklisted($1) AS "b!""#, account.0)
        .fetch_one(executor)
        .await
}

/// The accounts an entry covers (or would): any character of theirs is it,
/// or is in it. Never the owner.
pub async fn accounts_covered<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    entity_id: i64,
) -> Result<Vec<AccountId>, sqlx::Error> {
    let ids = sqlx::query_scalar!(
        r#"
        SELECT DISTINCT a.id FROM core.accounts a
        JOIN core.characters c ON c.account_id = a.id
        WHERE NOT a.is_owner AND $1 IN (c.id, c.corporation_id, c.alliance_id)
        "#,
        entity_id
    )
    .fetch_all(executor)
    .await?;
    Ok(ids.into_iter().map(AccountId).collect())
}

pub struct NewListing<'a> {
    pub entity_id: i64,
    pub kind: EntityKind,
    pub name: &'a str,
    pub reason: &'a str,
    pub added_by: AccountId,
    pub added_by_name: &'a str,
}

/// `false` if it was already blacklisted.
pub async fn add<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    listing: NewListing<'_>,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query!(
        r#"
        INSERT INTO core.blacklist (entity_id, entity_kind, name, reason, added_by, added_by_name)
        VALUES ($1, $2, $3, $4, $5, $6) ON CONFLICT DO NOTHING
        "#,
        listing.entity_id,
        listing.kind.as_str(),
        listing.name,
        listing.reason,
        listing.added_by.0,
        listing.added_by_name,
    )
    .execute(executor)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// Removes an entry; returns its kind and name.
pub async fn remove<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    entity_id: i64,
) -> Result<Option<(String, String)>, sqlx::Error> {
    let row = sqlx::query!(
        "DELETE FROM core.blacklist WHERE entity_id = $1 RETURNING entity_kind, name",
        entity_id
    )
    .fetch_optional(executor)
    .await?;
    Ok(row.map(|r| (r.entity_kind, r.name)))
}

/// Whether blacklisting this would cover the owner's main.
pub async fn covers_owner<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    entity_id: i64,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar!(
        r#"
        SELECT EXISTS (
            SELECT 1 FROM core.accounts a JOIN core.characters c ON c.id = a.main_character_id
            WHERE a.is_owner
              AND (c.id = $1 OR c.corporation_id = $1 OR c.alliance_id = $1)
        ) AS "covers!"
        "#,
        entity_id
    )
    .fetch_one(executor)
    .await
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note {
    pub id: i64,
    pub entity_id: i64,
    pub kind: EntityKind,
    pub name: String,
    pub note: String,
    pub added_by: Option<i64>,
    pub added_by_name: String,
    pub added_at: DateTime<Utc>,
}

/// The newest notes: on the given entities, or whose subject's name
/// contains `search`, or all.
pub async fn notes<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    about: Option<&[i64]>,
    search: Option<&str>,
    limit: i64,
) -> Result<Vec<Note>, sqlx::Error> {
    let pattern = search.map(|s| {
        let escaped: String = s
            .chars()
            .flat_map(|c| match c {
                '\\' | '%' | '_' => vec!['\\', c],
                c => vec![c],
            })
            .collect();
        format!("%{escaped}%")
    });
    let rows = sqlx::query!(
        r#"
        SELECT id, entity_id, entity_kind, name, note, added_by, added_by_name, added_at
        FROM core.pilot_notes
        WHERE ($1::bigint[] IS NULL OR entity_id = ANY($1))
          AND ($2::text IS NULL OR name ILIKE $2)
        ORDER BY added_at DESC, id DESC LIMIT $3
        "#,
        about.map(<[i64]>::to_vec) as Option<Vec<i64>>,
        pattern,
        limit,
    )
    .fetch_all(executor)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|r| {
            Some(Note {
                id: r.id,
                entity_id: r.entity_id,
                kind: EntityKind::parse(&r.entity_kind)?,
                name: r.name,
                note: r.note,
                added_by: r.added_by,
                added_by_name: r.added_by_name,
                added_at: r.added_at,
            })
        })
        .collect())
}

pub async fn note<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    id: i64,
) -> Result<Option<Note>, sqlx::Error> {
    let row = sqlx::query!(
        r#"
        SELECT id, entity_id, entity_kind, name, note, added_by, added_by_name, added_at
        FROM core.pilot_notes WHERE id = $1
        "#,
        id
    )
    .fetch_optional(executor)
    .await?;
    Ok(row.and_then(|r| {
        Some(Note {
            id: r.id,
            entity_id: r.entity_id,
            kind: EntityKind::parse(&r.entity_kind)?,
            name: r.name,
            note: r.note,
            added_by: r.added_by,
            added_by_name: r.added_by_name,
            added_at: r.added_at,
        })
    }))
}

pub struct NewNote<'a> {
    pub entity_id: i64,
    pub kind: EntityKind,
    pub name: &'a str,
    pub note: &'a str,
    pub added_by: AccountId,
    pub added_by_name: &'a str,
}

pub async fn add_note<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    note: NewNote<'_>,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar!(
        r#"
        INSERT INTO core.pilot_notes (entity_id, entity_kind, name, note, added_by, added_by_name)
        VALUES ($1, $2, $3, $4, $5, $6) RETURNING id
        "#,
        note.entity_id,
        note.kind.as_str(),
        note.name,
        note.note,
        note.added_by.0,
        note.added_by_name,
    )
    .fetch_one(executor)
    .await
}

/// Deletes a note; returns it.
pub async fn delete_note<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    id: i64,
) -> Result<Option<(i64, String)>, sqlx::Error> {
    let row = sqlx::query!(
        "DELETE FROM core.pilot_notes WHERE id = $1 RETURNING entity_id, note",
        id
    )
    .fetch_optional(executor)
    .await?;
    Ok(row.map(|r| (r.entity_id, r.note)))
}
