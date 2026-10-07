//! What's new: the newest changelog release each account has seen, and the
//! apps updated since (`crates/web-core/src/whats_new.rs`).

use crate::PgPool;
use crate::accounts::AccountId;
use chrono::{DateTime, Utc};

/// What an account has seen: the newest release's number and when.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Seen {
    pub release: i32,
    pub at: DateTime<Utc>,
}

/// The account's [`Seen`]; an account that never had one (new since What's
/// new came) starts at `latest`, now: a new pilot doesn't get the history.
pub async fn seen(pool: &PgPool, account: AccountId, latest: i32) -> Result<Seen, sqlx::Error> {
    let row = sqlx::query!(
        "SELECT whats_new_seen, whats_new_seen_at FROM core.accounts WHERE id = $1",
        account.0,
    )
    .fetch_optional(pool)
    .await?;
    if let Some(row) = row
        && let (Some(release), Some(at)) = (row.whats_new_seen, row.whats_new_seen_at)
    {
        return Ok(Seen { release, at });
    }
    let row = sqlx::query!(
        r#"
        UPDATE core.accounts
        SET whats_new_seen = COALESCE(whats_new_seen, $2),
            whats_new_seen_at = COALESCE(whats_new_seen_at, now())
        WHERE id = $1
        RETURNING whats_new_seen AS "release!", whats_new_seen_at AS "at!"
        "#,
        account.0,
        latest,
    )
    .fetch_one(pool)
    .await?;
    Ok(Seen {
        release: row.release,
        at: row.at,
    })
}

/// Marks everything up to `release` seen, now.
pub async fn mark_seen(pool: &PgPool, account: AccountId, release: i32) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "UPDATE core.accounts SET whats_new_seen = GREATEST(COALESCE(whats_new_seen, 0), $2), \
         whats_new_seen_at = now() WHERE id = $1",
        account.0,
        release,
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// An app moved to another version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppUpdate {
    pub name: String,
    pub from: String,
    pub to: String,
}

/// Apps upgraded since `since` and still installed, each once (its first
/// version then and its version now), by name; a rebuild under the same
/// number isn't one. At most 30.
pub async fn app_updates_since(
    pool: &PgPool,
    since: DateTime<Utc>,
) -> Result<Vec<AppUpdate>, sqlx::Error> {
    let rows = sqlx::query!(
        r#"
        SELECT p.name AS "name!", first.from_version AS "from!", p.version AS "to!"
        FROM core.plugins p
        JOIN LATERAL (
            SELECT l.details->>'from' AS from_version
            FROM core.audit_log l
            WHERE l.target = 'plugin:' || p.id AND l.action = 'plugin.upgraded' AND l.at > $1
              AND l.details->>'from' IS NOT NULL
            ORDER BY l.id
            LIMIT 1
        ) first ON true
        WHERE first.from_version IS DISTINCT FROM p.version
        ORDER BY p.name
        LIMIT 30
        "#,
        since,
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| AppUpdate {
            name: r.name,
            from: r.from,
            to: r.to,
        })
        .collect())
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

    /// A new account starts having seen everything so far; reading marks
    /// what's newest, never going back.
    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn a_new_pilot_starts_up_to_date(pool: PgPool) {
        let me = account(&pool).await;
        assert_eq!(seen(&pool, me, 4).await.unwrap().release, 4);
        // Later releases don't move it on their own.
        assert_eq!(seen(&pool, me, 6).await.unwrap().release, 4);
        mark_seen(&pool, me, 6).await.unwrap();
        assert_eq!(seen(&pool, me, 6).await.unwrap().release, 6);
        mark_seen(&pool, me, 5).await.unwrap();
        assert_eq!(seen(&pool, me, 6).await.unwrap().release, 6);
    }

    /// Apps upgraded since: each once, from its version then to its
    /// version now; a rebuild under the same number, an older upgrade or
    /// an app no longer installed isn't one.
    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn apps_updated_since(pool: PgPool) {
        sqlx::raw_sql(
            "INSERT INTO core.plugins (id, name, version, package, signature, package_sha256)
               VALUES ('a', 'Alpha', '1.2.0', '\\x00', '', '\\x00'),
                      ('b', 'Beta', '2.0.0', '\\x00', '', '\\x00');
             INSERT INTO core.audit_log (at, action, target, details) VALUES
               (now() - interval '2 days', 'plugin.upgraded', 'plugin:a', '{\"from\": \"1.0.0\", \"to\": \"1.1.0\"}'),
               (now(), 'plugin.upgraded', 'plugin:a', '{\"from\": \"1.1.0\", \"to\": \"1.1.5\"}'),
               (now(), 'plugin.upgraded', 'plugin:a', '{\"from\": \"1.1.5\", \"to\": \"1.2.0\"}'),
               (now(), 'plugin.upgraded', 'plugin:b', '{\"from\": \"2.0.0\", \"to\": \"2.0.0\"}'),
               (now(), 'plugin.upgraded', 'plugin:gone', '{\"from\": \"0.1.0\", \"to\": \"0.2.0\"}');",
        )
        .execute(&pool)
        .await
        .unwrap();
        let since = Utc::now() - chrono::Duration::days(1);
        assert_eq!(
            app_updates_since(&pool, since).await.unwrap(),
            vec![AppUpdate {
                name: "Alpha".to_owned(),
                from: "1.1.0".to_owned(),
                to: "1.2.0".to_owned(),
            }]
        );
    }
}
