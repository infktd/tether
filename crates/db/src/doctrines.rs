//! Doctrines apps share (`core.shared_doctrines`): one app's list replaces
//! its last, and each is offered to whoever may see it.

use sqlx::PgPool;

/// One doctrine as an app publishes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewDoctrine {
    pub key: String,
    pub name: String,
    pub link: String,
    /// `None`: everyone; else members of any of these groups.
    pub groups: Option<Vec<i64>>,
}

/// Replaces `plugin_id`'s doctrines with `doctrines`, in their order.
/// Holders of `plugin.<plugin_id>.<see_all>` see every one.
pub async fn replace(
    pool: &PgPool,
    plugin_id: &str,
    doctrines: &[NewDoctrine],
    see_all: Option<&str>,
) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    // One publish at a time per app (two at once would clash on the keys
    // and leave the older list), as uninstalling locks the row.
    sqlx::query!(
        "SELECT 1 AS one FROM core.plugins WHERE id = $1 FOR UPDATE",
        plugin_id
    )
    .fetch_optional(&mut *tx)
    .await?;
    sqlx::query!(
        "DELETE FROM core.shared_doctrines WHERE plugin_id = $1",
        plugin_id
    )
    .execute(&mut *tx)
    .await?;
    for (position, d) in (0..).zip(doctrines) {
        sqlx::query!(
            r#"
            INSERT INTO core.shared_doctrines
                (plugin_id, key, name, link, groups, see_all, position)
            VALUES ($1, $2, $3, $4, $5, $6, $7)
            "#,
            plugin_id,
            d.key,
            d.name,
            d.link,
            d.groups.as_deref(),
            see_all,
            position,
        )
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await
}

/// A shared doctrine someone may see.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeenDoctrine {
    pub plugin_id: String,
    pub plugin_name: String,
    pub name: String,
    pub link: String,
}

/// The doctrines an account in `groups`, holding `permissions`, may see:
/// those for everyone, those limited to one of its groups, and every one
/// of a publisher whose see-all permission it holds. By name.
pub async fn seen(
    pool: &PgPool,
    groups: &[i64],
    permissions: &[String],
) -> Result<Vec<SeenDoctrine>, sqlx::Error> {
    sqlx::query_as!(
        SeenDoctrine,
        r#"
        SELECT d.plugin_id, p.name AS plugin_name, d.name, d.link
        FROM core.shared_doctrines d JOIN core.plugins p ON p.id = d.plugin_id
        WHERE d.groups IS NULL
           OR d.groups && $1
           OR (d.see_all IS NOT NULL
               AND ('plugin.' || d.plugin_id || '.' || d.see_all) = ANY($2))
        ORDER BY lower(d.name), d.plugin_id, d.position
        "#,
        groups,
        permissions,
    )
    .fetch_all(pool)
    .await
}

/// Names other apps publish, lowercased: an app can't publish a name
/// another already does (and so take over its link).
pub async fn names_of_others(
    pool: &PgPool,
    plugin_id: &str,
) -> Result<Vec<(String, String)>, sqlx::Error> {
    let rows = sqlx::query!(
        r#"SELECT plugin_id, lower(name) AS "name!" FROM core.shared_doctrines WHERE plugin_id <> $1"#,
        plugin_id
    )
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(|r| (r.plugin_id, r.name)).collect())
}

/// Every name these apps share, seen or not: typed in by hand, one the
/// account can't see is refused, as a configured one is.
pub async fn names(pool: &PgPool, plugin_ids: &[String]) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar!(
        "SELECT DISTINCT name FROM core.shared_doctrines WHERE plugin_id = ANY($1) ORDER BY name",
        plugin_ids
    )
    .fetch_all(pool)
    .await
}
