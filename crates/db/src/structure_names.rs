//! Structure names any member could read (`core.structure_names`): see
//! `tether_web_core::structure_names`.

use crate::PgPool;

/// A structure's name, system and type, as ESI gave them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Named {
    pub structure_id: i64,
    pub name: String,
    pub solar_system_id: i64,
    pub type_id: Option<i64>,
}

/// The name kept for `structure_id`, read less than `max_age_secs` ago.
pub async fn get(
    pool: &PgPool,
    structure_id: i64,
    max_age_secs: f64,
) -> Result<Option<Named>, sqlx::Error> {
    sqlx::query_as!(
        Named,
        r#"
        SELECT structure_id, name, solar_system_id, type_id FROM core.structure_names
        WHERE structure_id = $1 AND read_at > now() - make_interval(secs => $2)
        "#,
        structure_id,
        max_age_secs,
    )
    .fetch_optional(pool)
    .await
}

pub async fn store(pool: &PgPool, named: &Named) -> Result<(), sqlx::Error> {
    sqlx::query!(
        r#"
        INSERT INTO core.structure_names (structure_id, name, solar_system_id, type_id)
        VALUES ($1, $2, $3, $4)
        ON CONFLICT (structure_id) DO UPDATE SET name = EXCLUDED.name,
            solar_system_id = EXCLUDED.solar_system_id, type_id = EXCLUDED.type_id,
            read_at = now()
        "#,
        named.structure_id,
        named.name,
        named.solar_system_id,
        named.type_id,
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Up to `limit` characters to ask for `structure_id`'s name: on active
/// accounts in Member or a state an admin put above it (never Blue, Guest
/// or the Blacklist, whatever their order, nor an account blacklisted but
/// not moved yet), with a valid token carrying `scope`, not refused it in
/// the last `miss_secs`, the most recently refreshed tokens first.
pub async fn candidates(
    pool: &PgPool,
    structure_id: i64,
    scope: &str,
    miss_secs: f64,
    limit: i64,
) -> Result<Vec<i64>, sqlx::Error> {
    sqlx::query_scalar!(
        r#"
        SELECT c.id FROM core.characters c
        JOIN core.character_tokens t ON t.character_id = c.id
        JOIN core.accounts a ON a.id = c.account_id
        JOIN core.states s ON s.id = a.state_id
        WHERE a.active AND t.state = 'valid' AND $2 = ANY(t.scopes)
          AND s.priority >= (SELECT priority FROM core.states WHERE builtin = 'member')
          AND (s.builtin IS NULL OR s.builtin = 'member')
          AND NOT core.blacklisted(a.id)
          AND NOT EXISTS (
              SELECT 1 FROM core.structure_name_misses m
              WHERE m.structure_id = $1 AND m.character_id = c.id
                AND m.at > now() - make_interval(secs => $3))
        ORDER BY t.last_refreshed_at DESC NULLS LAST, c.id
        LIMIT $4
        "#,
        structure_id,
        scope,
        miss_secs,
        limit,
    )
    .fetch_all(pool)
    .await
}

/// How many characters were refused `structure_id` in the last `secs`.
pub async fn recent_misses(
    pool: &PgPool,
    structure_id: i64,
    secs: f64,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar!(
        r#"SELECT count(*) AS "count!" FROM core.structure_name_misses
           WHERE structure_id = $1 AND at > now() - make_interval(secs => $2)"#,
        structure_id,
        secs,
    )
    .fetch_one(pool)
    .await
}

pub async fn miss(pool: &PgPool, structure_id: i64, character_id: i64) -> Result<(), sqlx::Error> {
    sqlx::query!(
        r#"
        INSERT INTO core.structure_name_misses (structure_id, character_id) VALUES ($1, $2)
        ON CONFLICT (structure_id, character_id) DO UPDATE SET at = now()
        "#,
        structure_id,
        character_id,
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Holds `structure_id`'s lookup for the transaction, so concurrent ones
/// wait and then find the name, or the hour's refusals, instead of asking
/// the same characters again.
pub async fn lock(tx: &mut sqlx::PgConnection, structure_id: i64) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "SELECT pg_advisory_xact_lock(hashtext('structure_names'), ($1::bigint % 2147483647)::int)",
        structure_id
    )
    .execute(tx)
    .await?;
    Ok(())
}

/// Forgets names and refusals older than `secs` (the hourly prune).
pub async fn prune(pool: &PgPool, secs: f64) -> Result<u64, sqlx::Error> {
    let names = sqlx::query!(
        "DELETE FROM core.structure_names WHERE read_at < now() - make_interval(secs => $1)",
        secs
    )
    .execute(pool)
    .await?
    .rows_affected();
    let misses = sqlx::query!(
        "DELETE FROM core.structure_name_misses WHERE at < now() - make_interval(secs => $1)",
        secs
    )
    .execute(pool)
    .await?
    .rows_affected();
    Ok(names + misses)
}
