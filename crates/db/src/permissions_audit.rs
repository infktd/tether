//! Permissions Audit (AA's permissions tool): for every permission, who
//! holds it and through what, computed the way `permissions::effective`
//! grants it (active accounts only, never blacklisted ones; groups only
//! while the account has a main; the owner holds everything).

use crate::PgPool;

/// How widely a permission is held.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Counts {
    pub states: i64,
    pub groups: i64,
    pub accounts: i64,
}

pub async fn counts(pool: &PgPool, permission: &str) -> Result<Counts, sqlx::Error> {
    sqlx::query_as!(
        Counts,
        r#"
        WITH g AS (SELECT state_id, group_id FROM core.permission_grants WHERE permission = $1)
        SELECT (SELECT count(*) FROM g WHERE state_id IS NOT NULL) AS "states!",
               (SELECT count(*) FROM g WHERE group_id IS NOT NULL) AS "groups!",
               (SELECT count(*) FROM core.accounts a
                WHERE a.active
                  AND NOT core.blacklisted(a.id)
                  AND (
                    a.is_owner
                    OR a.state_id IN (SELECT state_id FROM g WHERE state_id IS NOT NULL)
                    OR (a.main_character_id IS NOT NULL AND EXISTS (
                        SELECT 1 FROM core.group_members m
                        WHERE m.account_id = a.id
                          AND m.group_id IN (SELECT group_id FROM g WHERE group_id IS NOT NULL)))
                )) AS "accounts!"
        "#,
        permission
    )
    .fetch_one(pool)
    .await
}

/// One account holding a permission, and through what.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Holder {
    pub account_id: i64,
    pub main_id: i64,
    pub main_name: String,
    pub state: String,
    pub state_style: String,
    pub owner: bool,
    /// Its state grants it.
    pub via_state: bool,
    /// Groups it's in that grant it.
    pub via_groups: Vec<String>,
}

pub async fn holders(pool: &PgPool, permission: &str) -> Result<Vec<Holder>, sqlx::Error> {
    sqlx::query_as!(
        Holder,
        r#"
        WITH g AS (SELECT state_id, group_id FROM core.permission_grants WHERE permission = $1)
        SELECT a.id AS account_id, COALESCE(main.id, 0) AS "main_id!",
               COALESCE(main.name, '(no main)') AS "main_name!",
               s.name AS state, COALESCE(s.builtin, 'custom') AS "state_style!",
               a.is_owner AS owner,
               a.state_id IN (SELECT state_id FROM g WHERE state_id IS NOT NULL) AS "via_state!",
               COALESCE((
                   SELECT array_agg(gr.name ORDER BY gr.name)
                   FROM core.group_members m JOIN core.groups gr ON gr.id = m.group_id
                   WHERE m.account_id = a.id AND a.main_character_id IS NOT NULL
                     AND m.group_id IN (SELECT group_id FROM g WHERE group_id IS NOT NULL)
               ), '{}') AS "via_groups!"
        FROM core.accounts a
        JOIN core.states s ON s.id = a.state_id
        LEFT JOIN core.characters main ON main.id = a.main_character_id
        WHERE a.active
                  AND NOT core.blacklisted(a.id)
                  AND (
            a.is_owner
            OR a.state_id IN (SELECT state_id FROM g WHERE state_id IS NOT NULL)
            OR (a.main_character_id IS NOT NULL AND EXISTS (
                SELECT 1 FROM core.group_members m
                WHERE m.account_id = a.id
                  AND m.group_id IN (SELECT group_id FROM g WHERE group_id IS NOT NULL)))
        )
        -- No limit: an audit that leaves holders out is worse than none,
        -- and holders are bounded by accounts.
        ORDER BY main.name NULLS LAST
        "#,
        permission
    )
    .fetch_all(pool)
    .await
}
