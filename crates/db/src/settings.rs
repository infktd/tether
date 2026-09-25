//! Instance settings (`core.settings`), set through the first-run wizard.

use serde_json::Value;

/// EVE SSO application client id (PKCE; there is no client secret).
pub const SSO_CLIENT_ID: &str = "sso.client_id";
/// `{"at": <rfc3339>}` of the last successful SSO token exchange; proves the
/// client id and registered callback URL work.
pub const SSO_LAST_SUCCESS: &str = "sso.last_success";
/// `{"at": <rfc3339>, "error": <message>}` of the last failed exchange.
pub const SSO_LAST_ERROR: &str = "sso.last_error";
/// Discord application id (also the OAuth2 client id), as a string.
pub const DISCORD_APPLICATION_ID: &str = "discord.application_id";
/// The Discord server members join, as a string.
pub const DISCORD_GUILD_ID: &str = "discord.guild_id";
/// Nickname template such as `[{corp}] {name}`; unset means Tether leaves
/// nicknames alone.
pub const DISCORD_NICKNAME_TEMPLATE: &str = "discord.nickname_template";

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

pub async fn delete<'e>(executor: impl sqlx::PgExecutor<'e>, key: &str) -> Result<(), sqlx::Error> {
    sqlx::query!("DELETE FROM core.settings WHERE key = $1", key)
        .execute(executor)
        .await?;
    Ok(())
}
