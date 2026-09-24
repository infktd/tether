//! Instance settings (`core.settings`), set through the first-run wizard.

use serde_json::Value;

/// EVE SSO application client id (PKCE; there is no client secret).
pub const SSO_CLIENT_ID: &str = "sso.client_id";

pub async fn get<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    key: &str,
) -> Result<Option<Value>, sqlx::Error> {
    sqlx::query_scalar!("SELECT value FROM core.settings WHERE key = $1", key)
        .fetch_optional(executor)
        .await
}

pub async fn get_string<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    key: &str,
) -> Result<Option<String>, sqlx::Error> {
    Ok(get(executor, key)
        .await?
        .and_then(|v| v.as_str().map(str::to_owned)))
}

pub async fn set<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    key: &str,
    value: Value,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        r#"
        INSERT INTO core.settings (key, value) VALUES ($1, $2)
        ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value, updated_at = now()
        "#,
        key,
        value,
    )
    .execute(executor)
    .await?;
    Ok(())
}
