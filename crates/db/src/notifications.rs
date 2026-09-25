//! In-app notifications (AA's): only the recipient sees them, and each
//! account keeps its latest 50.

use chrono::{DateTime, Utc};

use crate::PgPool;
use crate::accounts::AccountId;

/// How many each account keeps; the oldest (read or not) go first.
pub const KEEP: i64 = 50;
pub const MAX_TITLE: usize = 254;
pub const MAX_MESSAGE: usize = 2000;
/// The Postgres channel a change is announced on, with the account id.
pub const CHANNEL: &str = "tether_notifications";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Danger,
    Warning,
    Info,
    Success,
}

impl Level {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Danger => "danger",
            Self::Warning => "warning",
            Self::Info => "info",
            Self::Success => "success",
        }
    }
}

fn clip(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

/// Sends a notification. The message defaults to the title (AA); both are
/// cut to their limits rather than refused, since callers build them from
/// names.
pub async fn notify(
    tx: &mut sqlx::PgConnection,
    account: AccountId,
    level: Level,
    title: &str,
    message: Option<&str>,
) -> Result<(), sqlx::Error> {
    send(tx, account, level, title, message, false).await
}

/// [`notify`], unless the same one is already waiting unread: for notices
/// others can trigger at will (requests), so they can't push everything
/// else out of the 50.
pub async fn notify_once(
    tx: &mut sqlx::PgConnection,
    account: AccountId,
    level: Level,
    title: &str,
    message: Option<&str>,
) -> Result<(), sqlx::Error> {
    send(tx, account, level, title, message, true).await
}

async fn send(
    tx: &mut sqlx::PgConnection,
    account: AccountId,
    level: Level,
    title: &str,
    message: Option<&str>,
    once: bool,
) -> Result<(), sqlx::Error> {
    let title = clip(title.trim(), MAX_TITLE);
    let message = clip(message.map_or(title.as_str(), str::trim), MAX_MESSAGE);
    let inserted = sqlx::query!(
        r#"
        INSERT INTO core.notifications (account_id, level, title, message)
        SELECT $1, $2, $3, $4
        WHERE NOT ($5 AND EXISTS (
            SELECT 1 FROM core.notifications
            WHERE account_id = $1 AND read_at IS NULL AND title = $3 AND message = $4
        ))
        "#,
        account.0,
        level.as_str(),
        title,
        message,
        once,
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if inserted == 0 {
        return Ok(());
    }
    sqlx::query!(
        r#"
        DELETE FROM core.notifications
        WHERE account_id = $1 AND id NOT IN (
            SELECT id FROM core.notifications WHERE account_id = $1
            ORDER BY id DESC LIMIT $2
        )
        "#,
        account.0,
        KEEP,
    )
    .execute(&mut *tx)
    .await?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notification {
    pub id: i64,
    pub level: String,
    pub title: String,
    pub message: String,
    pub created_at: DateTime<Utc>,
    pub read: bool,
}

/// The account's notifications, newest first.
pub async fn list(pool: &PgPool, account: AccountId) -> Result<Vec<Notification>, sqlx::Error> {
    sqlx::query_as!(
        Notification,
        r#"
        SELECT id, level, title, message, created_at, read_at IS NOT NULL AS "read!"
        FROM core.notifications WHERE account_id = $1
        ORDER BY id DESC
        "#,
        account.0
    )
    .fetch_all(pool)
    .await
}

/// Opens one of the account's notifications, marking it read.
pub async fn open(
    pool: &PgPool,
    account: AccountId,
    id: i64,
) -> Result<Option<Notification>, sqlx::Error> {
    sqlx::query_as!(
        Notification,
        r#"
        WITH marked AS (
            UPDATE core.notifications SET read_at = now()
            WHERE id = $1 AND account_id = $2 AND read_at IS NULL
        )
        SELECT id, level, title, message, created_at, true AS "read!"
        FROM core.notifications WHERE id = $1 AND account_id = $2
        "#,
        id,
        account.0
    )
    .fetch_optional(pool)
    .await
}

pub async fn delete(pool: &PgPool, account: AccountId, id: i64) -> Result<bool, sqlx::Error> {
    let result = sqlx::query!(
        "DELETE FROM core.notifications WHERE id = $1 AND account_id = $2",
        id,
        account.0
    )
    .execute(pool)
    .await?;
    Ok(result.rows_affected() == 1)
}

pub async fn mark_all_read(pool: &PgPool, account: AccountId) -> Result<u64, sqlx::Error> {
    let result = sqlx::query!(
        "UPDATE core.notifications SET read_at = now() WHERE account_id = $1 AND read_at IS NULL",
        account.0
    )
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

pub async fn delete_read(pool: &PgPool, account: AccountId) -> Result<u64, sqlx::Error> {
    let result = sqlx::query!(
        "DELETE FROM core.notifications WHERE account_id = $1 AND read_at IS NOT NULL",
        account.0
    )
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

pub async fn unread(pool: &PgPool, account: AccountId) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar!(
        r#"SELECT count(*) AS "count!" FROM core.notifications WHERE account_id = $1 AND read_at IS NULL"#,
        account.0
    )
    .fetch_one(pool)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts;

    async fn account(pool: &PgPool) -> AccountId {
        let login = accounts::Login {
            character_id: 7,
            character_name: "Pilot",
            owner_hash: "h",
        };
        accounts::sign_in(pool, login, false)
            .await
            .unwrap()
            .outcome
            .account()
            .unwrap()
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn keeps_the_latest_fifty(pool: PgPool) {
        let me = account(&pool).await;
        let mut conn = pool.acquire().await.unwrap();
        for i in 0..55 {
            notify(&mut conn, me, Level::Info, &format!("n{i}"), None)
                .await
                .unwrap();
        }
        let all = list(&pool, me).await.unwrap();
        assert_eq!(all.len(), 50);
        assert_eq!(all[0].title, "n54");
        assert_eq!(all[49].title, "n5");
        // The message defaults to the title.
        assert_eq!(all[0].message, "n54");
        assert_eq!(unread(&pool, me).await.unwrap(), 50);
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn only_the_recipient_reads_or_deletes(pool: PgPool) {
        let me = account(&pool).await;
        let other = AccountId(me.0 + 1000);
        let mut conn = pool.acquire().await.unwrap();
        notify(&mut conn, me, Level::Success, "Hello", Some("Body"))
            .await
            .unwrap();
        let id = list(&pool, me).await.unwrap()[0].id;
        assert!(open(&pool, other, id).await.unwrap().is_none());
        assert!(!delete(&pool, other, id).await.unwrap());
        let opened = open(&pool, me, id).await.unwrap().unwrap();
        assert_eq!(opened.message, "Body");
        assert_eq!(unread(&pool, me).await.unwrap(), 0);
        notify(&mut conn, me, Level::Danger, "Two", None)
            .await
            .unwrap();
        assert_eq!(delete_read(&pool, me).await.unwrap(), 1);
        assert_eq!(mark_all_read(&pool, me).await.unwrap(), 1);
        assert_eq!(list(&pool, me).await.unwrap().len(), 1);
    }
}
