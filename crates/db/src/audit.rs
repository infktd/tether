//! The append-only audit log.

use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::PgPool;
use crate::accounts::AccountId;

/// Who did it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Actor {
    Account(AccountId),
    /// Scheduled or automatic work (tier sync, jobs).
    System,
    /// The `tether` admin CLI, run by whoever has shell access to the host.
    Cli,
}

/// Records one entry. Call it in the same transaction as the change it
/// describes, so neither exists without the other.
pub async fn record<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    actor: Actor,
    action: &str,
    target: Option<&str>,
    details: Value,
) -> Result<(), sqlx::Error> {
    let (actor_id, fixed_name) = match actor {
        Actor::Account(a) => (Some(a.0), None),
        Actor::System => (None, None),
        Actor::Cli => (None, Some("cli")),
    };
    sqlx::query!(
        r#"
        INSERT INTO core.audit_log (actor_account_id, actor_name, action, target, details)
        VALUES (
            $1,
            COALESCE($5, (SELECT c.name FROM core.accounts a
                          JOIN core.characters c ON c.id = a.main_character_id
                          WHERE a.id = $1)),
            $2, $3, $4
        )
        "#,
        actor_id,
        action,
        target,
        details,
        fixed_name,
    )
    .execute(executor)
    .await?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub id: i64,
    pub at: DateTime<Utc>,
    pub actor_account_id: Option<i64>,
    pub actor_name: Option<String>,
    pub action: String,
    pub target: Option<String>,
    pub details: Value,
}

/// Newest first; pass the last id seen as `before` to page back.
pub async fn list(
    pool: &PgPool,
    limit: i64,
    before: Option<i64>,
) -> Result<Vec<Entry>, sqlx::Error> {
    sqlx::query_as!(
        Entry,
        r#"
        SELECT id, at, actor_account_id, actor_name, action, target, details
        FROM core.audit_log
        WHERE $2::bigint IS NULL OR id < $2
        ORDER BY id DESC
        LIMIT $1
        "#,
        limit,
        before,
    )
    .fetch_all(pool)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn entries_are_append_only(pool: PgPool) {
        record(
            &pool,
            Actor::System,
            "test.action",
            Some("thing:1"),
            json!({"a": 1}),
        )
        .await
        .unwrap();

        let update = sqlx::query("UPDATE core.audit_log SET action = 'forged'")
            .execute(&pool)
            .await;
        let delete = sqlx::query("DELETE FROM core.audit_log")
            .execute(&pool)
            .await;

        assert!(update.unwrap_err().to_string().contains("append-only"));
        assert!(delete.unwrap_err().to_string().contains("append-only"));
        let entries = list(&pool, 10, None).await.unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].action, "test.action");
        assert_eq!(entries[0].actor_account_id, None);
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn actor_name_is_the_mains_name(pool: PgPool) {
        let login = crate::accounts::Login {
            character_id: 7,
            character_name: "Admin Pilot",
            owner_hash: "h",
        };
        let account = crate::accounts::sign_in(&pool, login, None, false)
            .await
            .unwrap()
            .outcome
            .account()
            .unwrap();
        record(&pool, Actor::Account(account), "x", None, json!({}))
            .await
            .unwrap();
        let entry = &list(&pool, 1, None).await.unwrap()[0];
        assert_eq!(entry.actor_name.as_deref(), Some("Admin Pilot"));
    }
}
