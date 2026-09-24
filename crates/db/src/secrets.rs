//! Instance secrets (`core.secrets`), stored sealed. Callers seal and open
//! with the instance key and [`context`], so this module never sees a
//! plaintext.

pub const DISCORD_BOT_TOKEN: &str = "discord.bot_token";
pub const DISCORD_CLIENT_SECRET: &str = "discord.client_secret";

/// Associated data for sealing `name`: a sealed value only opens under the
/// name it was stored as.
pub fn context(name: &str) -> String {
    format!("secret:{name}")
}

pub async fn put<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    name: &str,
    sealed: &[u8],
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        r#"
        INSERT INTO core.secrets (name, sealed) VALUES ($1, $2)
        ON CONFLICT (name) DO UPDATE SET sealed = EXCLUDED.sealed, updated_at = now()
        "#,
        name,
        sealed,
    )
    .execute(executor)
    .await?;
    Ok(())
}

pub async fn get<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    name: &str,
) -> Result<Option<Vec<u8>>, sqlx::Error> {
    sqlx::query_scalar!("SELECT sealed FROM core.secrets WHERE name = $1", name)
        .fetch_optional(executor)
        .await
}
