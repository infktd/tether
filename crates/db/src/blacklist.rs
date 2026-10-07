//! The Blacklist and the Pilot Log (allianceauth-blacklist): notes on
//! pilots, corporations and alliances (AA's EveNote), each of which may be
//! blacklisted, restricted or ultra restricted, and comments on notes. The
//! Blacklist is the blacklisted notes.

use chrono::{DateTime, Utc};
use tether_core::states::EntityKind;

use crate::accounts::AccountId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note {
    pub id: i64,
    pub entity_id: i64,
    pub kind: EntityKind,
    pub name: String,
    /// AA's reason.
    pub note: String,
    pub blacklisted: bool,
    pub restricted: bool,
    pub ultra_restricted: bool,
    /// A pilot's corporation and alliance when the note was added (a
    /// corporation's note: itself).
    pub corporation_id: Option<i64>,
    pub corporation_name: Option<String>,
    pub alliance_id: Option<i64>,
    pub alliance_name: Option<String>,
    pub added_by: Option<i64>,
    pub added_by_name: String,
    pub added_at: DateTime<Utc>,
    pub edited_at: Option<DateTime<Utc>>,
}

/// Which notes a reader sees, as allianceauth-blacklist's permissions.
#[derive(Debug, Clone, Default)]
pub struct Reader {
    /// Every note (`view_eve_notes`), or only those on this corporation's
    /// pilots (`view_basic_eve_notes`: the main's corporation). Neither:
    /// none.
    pub all: bool,
    pub corporation: Option<i64>,
    pub restricted: bool,
    pub ultra_restricted: bool,
}

/// What to list.
#[derive(Debug, Clone, Copy, Default)]
pub struct Filter<'a> {
    pub about: Option<&'a [i64]>,
    /// Whose subject's name contains this.
    pub search: Option<&'a str>,
    /// About a pilot, corporation, alliance or faction (`EntityKind`'s
    /// word).
    pub kind: Option<&'a str>,
    /// Only blacklisted notes, whatever the reader's tiers (the Blacklist:
    /// a restricted note's reason is hidden, not the entry).
    pub blacklist: bool,
}

struct Row {
    id: i64,
    entity_id: i64,
    entity_kind: String,
    name: String,
    note: String,
    blacklisted: bool,
    restricted: bool,
    ultra_restricted: bool,
    corporation_id: Option<i64>,
    corporation_name: Option<String>,
    alliance_id: Option<i64>,
    alliance_name: Option<String>,
    added_by: Option<i64>,
    added_by_name: String,
    added_at: DateTime<Utc>,
    edited_at: Option<DateTime<Utc>>,
}

impl Row {
    fn into_note(self) -> Option<Note> {
        Some(Note {
            id: self.id,
            entity_id: self.entity_id,
            kind: EntityKind::parse(&self.entity_kind)?,
            name: self.name,
            note: self.note,
            blacklisted: self.blacklisted,
            restricted: self.restricted,
            ultra_restricted: self.ultra_restricted,
            corporation_id: self.corporation_id,
            corporation_name: self.corporation_name,
            alliance_id: self.alliance_id,
            alliance_name: self.alliance_name,
            added_by: self.added_by,
            added_by_name: self.added_by_name,
            added_at: self.added_at,
            edited_at: self.edited_at,
        })
    }
}

fn like(search: &str) -> String {
    let escaped: String = search
        .chars()
        .flat_map(|c| match c {
            '\\' | '%' | '_' => vec!['\\', c],
            c => vec![c],
        })
        .collect();
    format!("%{escaped}%")
}

/// The newest notes the reader may see (for the Blacklist, every
/// blacklisted note), filtered.
pub async fn notes<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    reader: &Reader,
    filter: Filter<'_>,
    limit: i64,
) -> Result<Vec<Note>, sqlx::Error> {
    let rows = sqlx::query_as!(
        Row,
        r#"
        SELECT id, entity_id, entity_kind, name, note, blacklisted, restricted,
               ultra_restricted, corporation_id, corporation_name, alliance_id,
               alliance_name, added_by, added_by_name, added_at, edited_at
        FROM core.pilot_notes
        WHERE ($1::bigint[] IS NULL OR entity_id = ANY($1))
          AND ($2::text IS NULL OR name ILIKE $2)
          AND ($9::text IS NULL OR entity_kind = $9)
          AND (CASE WHEN $3 THEN blacklisted
                    ELSE ($4 OR ($5::bigint IS NOT NULL AND corporation_id = $5))
                         AND ($6 OR NOT restricted) AND ($7 OR NOT ultra_restricted)
               END)
        ORDER BY added_at DESC, id DESC LIMIT $8
        "#,
        filter.about.map(<[i64]>::to_vec) as Option<Vec<i64>>,
        filter.search.map(like),
        filter.blacklist,
        reader.all,
        reader.corporation,
        reader.restricted,
        reader.ultra_restricted,
        limit,
        filter.kind,
    )
    .fetch_all(executor)
    .await?;
    Ok(rows.into_iter().filter_map(Row::into_note).collect())
}

pub async fn note<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    id: i64,
) -> Result<Option<Note>, sqlx::Error> {
    let row = sqlx::query_as!(
        Row,
        r#"
        SELECT id, entity_id, entity_kind, name, note, blacklisted, restricted,
               ultra_restricted, corporation_id, corporation_name, alliance_id,
               alliance_name, added_by, added_by_name, added_at, edited_at
        FROM core.pilot_notes WHERE id = $1
        "#,
        id
    )
    .fetch_optional(executor)
    .await?;
    Ok(row.and_then(Row::into_note))
}

/// A note, locked until the transaction ends.
pub async fn note_locked(
    tx: &mut sqlx::PgConnection,
    id: i64,
) -> Result<Option<Note>, sqlx::Error> {
    sqlx::query!(
        "SELECT id FROM core.pilot_notes WHERE id = $1 FOR UPDATE",
        id
    )
    .fetch_optional(&mut *tx)
    .await?;
    note(&mut *tx, id).await
}

impl Reader {
    /// Whether the reader may see this note in the Pilot Log.
    pub fn sees(&self, note: &Note) -> bool {
        (self.all || (self.corporation.is_some() && note.corporation_id == self.corporation))
            && (self.restricted || !note.restricted)
            && (self.ultra_restricted || !note.ultra_restricted)
    }

    /// Whether the reader may read this note's reason (on the Blacklist,
    /// a restricted one's reason says who to ask instead, as AA's).
    pub fn reads_reason(&self, note: &Note) -> bool {
        (self.restricted || !note.restricted) && (self.ultra_restricted || !note.ultra_restricted)
    }
}

/// Whether an account is blacklisted: its main is, or is in a
/// corporation or alliance that is (never a superuser).
pub async fn is_blacklisted<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    account: AccountId,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar!(r#"SELECT core.blacklisted($1) AS "b!""#, account.0)
        .fetch_one(executor)
        .await
}

/// The accounts blacklisting these would cover (or does): their main is
/// one of them, or is in one. Never a superuser.
pub async fn accounts_covered<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    entities: &[i64],
) -> Result<Vec<AccountId>, sqlx::Error> {
    let ids = sqlx::query_scalar!(
        r#"
        SELECT DISTINCT a.id FROM core.accounts a
        JOIN core.characters c ON c.id = a.main_character_id
        WHERE NOT a.is_owner
          AND (c.id = ANY($1) OR c.corporation_id = ANY($1) OR c.alliance_id = ANY($1))
        "#,
        entities
    )
    .fetch_all(executor)
    .await?;
    Ok(ids.into_iter().map(AccountId).collect())
}

/// Whether blacklisting any of these would cover a superuser's main.
pub async fn covers_owner<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    entities: &[i64],
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar!(
        r#"
        SELECT EXISTS (
            SELECT 1 FROM core.accounts a JOIN core.characters c ON c.id = a.main_character_id
            WHERE a.is_owner
              AND (c.id = ANY($1) OR c.corporation_id = ANY($1) OR c.alliance_id = ANY($1))
        ) AS "covers!"
        "#,
        entities
    )
    .fetch_one(executor)
    .await
}

/// Whether any note on this entity is blacklisted.
pub async fn entity_blacklisted<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    entity_id: i64,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM core.pilot_notes WHERE entity_id = $1 AND blacklisted) AS "b!""#,
        entity_id
    )
    .fetch_one(executor)
    .await
}

/// A character Tether knows, with the other characters on its account
/// (AA's "all linked characters"): `(id, name, corporation, alliance)`.
pub async fn linked_characters<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    character: i64,
) -> Result<Vec<(i64, String, Option<i64>, Option<i64>)>, sqlx::Error> {
    let rows = sqlx::query!(
        r#"
        SELECT c.id, c.name, c.corporation_id, c.alliance_id FROM core.characters c
        WHERE c.account_id = (SELECT account_id FROM core.characters WHERE id = $1)
          AND c.id <> $1
        ORDER BY c.name
        "#,
        character
    )
    .fetch_all(executor)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| (r.id, r.name, r.corporation_id, r.alliance_id))
        .collect())
}

pub struct NewNote<'a> {
    pub entity_id: i64,
    pub kind: EntityKind,
    pub name: &'a str,
    pub note: &'a str,
    pub blacklisted: bool,
    pub restricted: bool,
    pub ultra_restricted: bool,
    pub corporation: Option<(i64, Option<&'a str>)>,
    pub alliance: Option<(i64, Option<&'a str>)>,
    pub added_by: AccountId,
    pub added_by_name: &'a str,
}

pub async fn add_note<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    note: NewNote<'_>,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar!(
        r#"
        INSERT INTO core.pilot_notes
            (entity_id, entity_kind, name, note, blacklisted, restricted, ultra_restricted,
             corporation_id, corporation_name, alliance_id, alliance_name, added_by, added_by_name)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13) RETURNING id
        "#,
        note.entity_id,
        note.kind.as_str(),
        note.name,
        note.note,
        note.blacklisted,
        note.restricted,
        note.ultra_restricted,
        note.corporation.map(|(id, _)| id),
        note.corporation.and_then(|(_, name)| name),
        note.alliance.map(|(id, _)| id),
        note.alliance.and_then(|(_, name)| name),
        note.added_by.0,
        note.added_by_name,
    )
    .fetch_one(executor)
    .await
}

/// Changes a note's reason and flags.
pub async fn edit_note<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    id: i64,
    note: &str,
    blacklisted: bool,
    restricted: bool,
    ultra_restricted: bool,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        r#"
        UPDATE core.pilot_notes
        SET note = $2, blacklisted = $3, restricted = $4, ultra_restricted = $5, edited_at = now()
        WHERE id = $1
        "#,
        id,
        note,
        blacklisted,
        restricted,
        ultra_restricted,
    )
    .execute(executor)
    .await?;
    Ok(())
}

/// Takes an entity off the Blacklist (every note on it stays, no longer
/// blacklisted); returns how many notes changed and the entity's kind and
/// name.
pub async fn unblacklist<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    entity_id: i64,
) -> Result<Option<(String, String, u64)>, sqlx::Error> {
    let rows = sqlx::query!(
        r#"
        UPDATE core.pilot_notes SET blacklisted = false, edited_at = now()
        WHERE entity_id = $1 AND blacklisted
        RETURNING entity_kind, name
        "#,
        entity_id
    )
    .fetch_all(executor)
    .await?;
    let count = rows.len() as u64;
    Ok(rows
        .into_iter()
        .next()
        .map(|r| (r.entity_kind, r.name, count)))
}

/// Deletes a note and its comments; returns it.
pub async fn delete_note<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    id: i64,
) -> Result<Option<(i64, bool)>, sqlx::Error> {
    let row = sqlx::query!(
        "DELETE FROM core.pilot_notes WHERE id = $1 RETURNING entity_id, blacklisted",
        id
    )
    .fetch_optional(executor)
    .await?;
    Ok(row.map(|r| (r.entity_id, r.blacklisted)))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Comment {
    pub id: i64,
    pub note_id: i64,
    pub comment: String,
    pub restricted: bool,
    pub ultra_restricted: bool,
    pub added_by_name: String,
    pub added_at: DateTime<Utc>,
}

/// The comments on these notes the reader may see, oldest first.
pub async fn comments<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    notes: &[i64],
    restricted: bool,
    ultra_restricted: bool,
) -> Result<Vec<Comment>, sqlx::Error> {
    sqlx::query_as!(
        Comment,
        r#"
        SELECT id, note_id, comment, restricted, ultra_restricted, added_by_name, added_at
        FROM core.pilot_note_comments
        WHERE note_id = ANY($1) AND ($2 OR NOT restricted) AND ($3 OR NOT ultra_restricted)
        ORDER BY added_at, id
        "#,
        notes,
        restricted,
        ultra_restricted,
    )
    .fetch_all(executor)
    .await
}

pub struct NewComment<'a> {
    pub note_id: i64,
    pub comment: &'a str,
    pub restricted: bool,
    pub ultra_restricted: bool,
    pub added_by: AccountId,
    pub added_by_name: &'a str,
}

pub async fn add_comment<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    comment: NewComment<'_>,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar!(
        r#"
        INSERT INTO core.pilot_note_comments
            (note_id, comment, restricted, ultra_restricted, added_by, added_by_name)
        VALUES ($1, $2, $3, $4, $5, $6) RETURNING id
        "#,
        comment.note_id,
        comment.comment,
        comment.restricted,
        comment.ultra_restricted,
        comment.added_by.0,
        comment.added_by_name,
    )
    .fetch_one(executor)
    .await
}
