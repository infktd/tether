//! The append-only audit log.

use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::PgPool;
use crate::accounts::AccountId;

/// Who did it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Actor {
    Account(AccountId),
    /// Scheduled or automatic work (state sync, jobs).
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
    // Changes made through an access token say which one.
    let mut details = details;
    if let (Actor::Account(a), Some(token)) = (actor, crate::permissions::token_scope())
        && token.account == a
        && let Some(object) = details.as_object_mut()
    {
        object.insert("via_token".to_owned(), token.token_id.into());
    }
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
    find(pool, &Filter::default(), limit, before).await
}

/// Who did it, as the log is filtered by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Who {
    Account(AccountId),
    System,
    Cli,
}

/// What the log is filtered by; the default is everything.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Filter {
    pub who: Option<Who>,
    /// Any of these actions (one, or a family's).
    pub actions: Option<Vec<String>>,
    /// The app an entry is about (`core.audit_log_app`: its target, one of
    /// its schedules, or its details' `app`).
    pub app: Option<String>,
    /// From this moment, and before that one.
    pub from: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
    /// Words each found, whatever their case, in who, the action, the
    /// target or the details.
    pub words: Vec<String>,
}

/// `%word%` for ILIKE, with its own `%`, `_` and `\` taken literally.
fn pattern(word: &str) -> String {
    let mut out = String::with_capacity(word.len() + 2);
    out.push('%');
    for c in word.chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('%');
    out
}

/// The entries `filter` keeps, newest first (by when, then id); pass the
/// last id seen as `before` to page back. Each filter reads in that order
/// from its own index, within the dates asked for (migration 0065).
pub async fn find(
    pool: &PgPool,
    filter: &Filter,
    limit: i64,
    before: Option<i64>,
) -> Result<Vec<Entry>, sqlx::Error> {
    let before_at = match before {
        None => None,
        Some(id) => {
            let at = sqlx::query_scalar!("SELECT at FROM core.audit_log WHERE id = $1", id)
                .fetch_optional(pool)
                .await?;
            // Before an entry that isn't there: nothing.
            let Some(at) = at else {
                return Ok(Vec::new());
            };
            Some(at)
        }
    };
    let (account, fixed) = match filter.who {
        None => (None, None),
        Some(Who::Account(a)) => (Some(a.0), None),
        Some(Who::System) => (None, Some("system")),
        Some(Who::Cli) => (None, Some("cli")),
    };
    let words: Option<Vec<String>> =
        (!filter.words.is_empty()).then(|| filter.words.iter().map(|w| pattern(w)).collect());
    sqlx::query_as!(
        Entry,
        r#"
        SELECT id, at, actor_account_id, actor_name, action, target, details
        FROM core.audit_log l
        WHERE ($10::timestamptz IS NULL OR (at, id) < ($10, $2::bigint))
          AND ($3::bigint IS NULL OR actor_account_id = $3)
          AND ($4::text IS NULL OR (actor_account_id IS NULL AND CASE WHEN $4 = 'cli'
                   THEN actor_name = 'cli' ELSE actor_name IS DISTINCT FROM 'cli' END))
          AND ($5::text[] IS NULL OR action = ANY($5))
          AND ($6::text IS NULL OR core.audit_log_app(target, details) = $6)
          AND ($7::timestamptz IS NULL OR at >= $7)
          AND ($8::timestamptz IS NULL OR at < $8)
          AND ($9::text[] IS NULL OR NOT EXISTS (
                SELECT 1 FROM unnest($9::text[]) AS w(p)
                WHERE concat_ws(' ', l.actor_name, l.action, l.target, l.details::text) NOT ILIKE w.p))
        ORDER BY at DESC, id DESC
        LIMIT $1
        "#,
        limit,
        before,
        account,
        fixed,
        filter.actions.as_deref(),
        filter.app,
        filter.from,
        filter.until,
        words.as_deref(),
        before_at,
    )
    .fetch_all(pool)
    .await
}

/// Every action the log holds, in order: read from the action index one
/// value at a time, so a long log costs no more than a short one.
pub async fn actions(pool: &PgPool) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar!(
        r#"
        WITH RECURSIVE a(action) AS (
            (SELECT action FROM core.audit_log ORDER BY action LIMIT 1)
            UNION ALL
            SELECT (SELECT l.action FROM core.audit_log l
                    WHERE l.action > a.action ORDER BY l.action LIMIT 1)
            FROM a WHERE a.action IS NOT NULL
        )
        SELECT action AS "action!" FROM a WHERE action IS NOT NULL
        "#
    )
    .fetch_all(pool)
    .await
}

/// Every app the log has entries about, in order (as [`actions`]).
pub async fn apps(pool: &PgPool) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar!(
        r#"
        WITH RECURSIVE a(app) AS (
            (SELECT core.audit_log_app(target, details) FROM core.audit_log
             WHERE core.audit_log_app(target, details) IS NOT NULL
             ORDER BY core.audit_log_app(target, details) LIMIT 1)
            UNION ALL
            SELECT (SELECT core.audit_log_app(l.target, l.details) FROM core.audit_log l
                    WHERE core.audit_log_app(l.target, l.details) IS NOT NULL
                      AND core.audit_log_app(l.target, l.details) > a.app
                    ORDER BY core.audit_log_app(l.target, l.details) LIMIT 1)
            FROM a WHERE a.app IS NOT NULL
        )
        SELECT app AS "app!" FROM a WHERE app IS NOT NULL
        "#
    )
    .fetch_all(pool)
    .await
}

/// The accounts behind the newest entries (of the last [`RECENT`]), most
/// recent first, with the name each last had: at most `limit`.
pub async fn recent_actors(pool: &PgPool, limit: i64) -> Result<Vec<(i64, String)>, sqlx::Error> {
    let rows = sqlx::query!(
        r#"
        SELECT actor_account_id AS "id!",
               (array_agg(actor_name ORDER BY id DESC))[1] AS name
        FROM (SELECT id, actor_account_id, actor_name FROM core.audit_log
              ORDER BY id DESC LIMIT $2) r
        WHERE actor_account_id IS NOT NULL
        GROUP BY actor_account_id
        ORDER BY max(id) DESC
        LIMIT $1
        "#,
        limit,
        RECENT,
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            let name = r.name.unwrap_or_else(|| format!("Account {}", r.id));
            (r.id, name)
        })
        .collect())
}

/// Entries [`recent_actors`] reads.
const RECENT: i64 = 2_000;

/// The name an account last acted under in the log.
pub async fn actor_name(pool: &PgPool, account: AccountId) -> Result<Option<String>, sqlx::Error> {
    Ok(sqlx::query_scalar!(
        "SELECT actor_name FROM core.audit_log WHERE actor_account_id = $1 ORDER BY at DESC, id DESC LIMIT 1",
        account.0,
    )
    .fetch_optional(pool)
    .await?
    .flatten())
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
        let account = crate::accounts::sign_in(&pool, login, false)
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
