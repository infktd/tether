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
/// AA's `DISCORD_SYNC_NAMES`: whether Tether sets members' nicknames (by
/// the Name Formatter). On unless set.
pub const DISCORD_SYNC_NAMES: &str = "discord.sync_names";
/// Removes every role Tether doesn't map to a member, except Discord's
/// own (integration) roles and reserved group names. Off unless set.
pub const DISCORD_STRIP_UNMAPPED: &str = "discord.strip_unmapped";

/// The accent colour (DESIGN.md), `#rrggbb`. Amber unless set.
pub const THEME_ACCENT: &str = "theme.accent";

/// aa-fleetpings: whether pings may target @here and @everyone. On unless
/// set.
pub const PINGS_MASS_MENTIONS: &str = "pings.mass_mentions";

/// AA's `GROUPMANAGEMENT_AUTO_LEAVE`: `true` lets members leave
/// requestable groups without approval. Off unless set.
pub const GROUPS_AUTO_LEAVE: &str = "groups.auto_leave";
/// AA's `GROUPMANAGEMENT_REQUESTS_NOTIFICATION`: `true` tells a group's
/// leaders about new requests. Off unless set.
pub const GROUPS_NOTIFY_REQUESTS: &str = "groups.notify_requests";

/// A boolean setting; unset (or not a boolean) is `false`.
pub async fn get_bool<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    key: &str,
) -> Result<bool, sqlx::Error> {
    get_bool_or(executor, key, false).await
}

/// A boolean setting, `default` when unset.
pub async fn get_bool_or<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    key: &str,
    default: bool,
) -> Result<bool, sqlx::Error> {
    Ok(get(executor, key)
        .await?
        .and_then(|v| v.as_bool())
        .unwrap_or(default))
}

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
