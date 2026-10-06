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
/// the Name Formatter). Off unless set, as AA's (instances from before
/// this default had it saved as on).
pub const DISCORD_SYNC_NAMES: &str = "discord.sync_names";
/// Removes every role Tether doesn't map to a member, except Discord's
/// own (integration) roles and reserved group names. Off unless set.
pub const DISCORD_STRIP_UNMAPPED: &str = "discord.strip_unmapped";

/// The site's own name (an alliance's, say), shown in browser tabs and on
/// the sign-in page beside Tether's. None unless set.
pub const SITE_NAME: &str = "site.name";

/// The accent colour (DESIGN.md), `#rrggbb`. Signal orange unless set.
pub const THEME_ACCENT: &str = "theme.accent";

/// aa-fleetpings: whether pings may target @here and @everyone. On unless
/// set.
pub const PINGS_MASS_MENTIONS: &str = "pings.mass_mentions";
/// aa-fleetpings' `use_default_fleet_types`: whether the form offers
/// Roaming, Home Defense, StratOP and CTA beside the configured fleet
/// types. On unless set.
pub const PINGS_DEFAULT_FLEET_TYPES: &str = "pings.use_default_fleet_types";
/// aa-fleetpings' `default_embed_color`: the ping card's colour when its
/// fleet type has none (`#rrggbb`). `#faa61a` unless set.
pub const PINGS_DEFAULT_EMBED_COLOR: &str = "pings.default_embed_color";
/// aa-fleetpings' `use_doctrines_from_fittings_module`: the form offers the
/// doctrines apps share (Fittings') that the pilot may see, instead of the
/// ones configured here. Off unless set.
pub const PINGS_DOCTRINES_FROM_APPS: &str = "pings.use_doctrines_from_fittings";

/// AA's `GROUPMANAGEMENT_AUTO_LEAVE`: `true` lets members leave
/// requestable groups without approval. Off unless set.
pub const GROUPS_AUTO_LEAVE: &str = "groups.auto_leave";
/// AA's `GROUPMANAGEMENT_REQUESTS_NOTIFICATION`: `true` tells a group's
/// leaders about new requests. Off unless set.
pub const GROUPS_NOTIFY_REQUESTS: &str = "groups.notify_requests";

/// AA's `NOTIFICATIONS_MAX_PER_USER`: how many notifications each account
/// keeps (the oldest go first). [`NOTIFICATIONS_MAX_DEFAULT`] unless set.
pub const NOTIFICATIONS_MAX_PER_USER: &str = "notifications.max_per_user";
/// AA's default.
pub const NOTIFICATIONS_MAX_DEFAULT: i64 = 50;
/// The range admins may set: at least one, and a bound on the table.
pub const NOTIFICATIONS_MAX_RANGE: std::ops::RangeInclusive<i64> = 1..=1000;

/// The notification cap, [`NOTIFICATIONS_MAX_DEFAULT`] when unset or out of
/// range.
pub async fn notifications_max<'e>(
    executor: impl sqlx::PgExecutor<'e>,
) -> Result<i64, sqlx::Error> {
    Ok(get(executor, NOTIFICATIONS_MAX_PER_USER)
        .await?
        .and_then(|v| v.as_i64())
        .filter(|n| NOTIFICATIONS_MAX_RANGE.contains(n))
        .unwrap_or(NOTIFICATIONS_MAX_DEFAULT))
}

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
