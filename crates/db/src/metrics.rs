//! Counts for the optional `/metrics` endpoint: nothing about any one
//! account, and no names.

use crate::PgPool;

/// Active accounts in one state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateCount {
    pub state_id: i64,
    /// `member`, `blue`, `guest` or `blacklist` for a built-in state.
    pub builtin: Option<String>,
    pub accounts: i64,
}

/// Every state with its active accounts (zero included), by id.
pub async fn accounts_by_state(pool: &PgPool) -> Result<Vec<StateCount>, sqlx::Error> {
    sqlx::query_as!(
        StateCount,
        r#"
        SELECT s.id AS state_id, s.builtin, count(a.id) AS "accounts!"
        FROM core.states s
        LEFT JOIN core.accounts a ON a.state_id = s.id AND a.active
        GROUP BY s.id, s.builtin
        ORDER BY s.id
        "#
    )
    .fetch_all(pool)
    .await
}

/// Deactivated accounts.
pub async fn deactivated_accounts(pool: &PgPool) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar!(r#"SELECT count(*) AS "n!" FROM core.accounts WHERE NOT active"#)
        .fetch_one(pool)
        .await
}
