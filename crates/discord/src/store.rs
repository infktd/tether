//! The Discord configuration at rest: ids in `core.settings`, the bot token
//! and client secret sealed in `core.secrets`.

use tether_core::Secret;
use tether_core::crypto::{CryptoError, EncryptionKey};
use tether_db::{PgPool, secrets, settings};

use crate::DiscordConfig;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("a stored Discord secret can't be decrypted (was ENCRYPTION_KEY changed?)")]
    Crypto(#[from] CryptoError),
    #[error("stored Discord setting {0} is invalid")]
    Invalid(&'static str),
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

/// What is saved, possibly incomplete; the secrets opened.
#[derive(Debug, Default)]
pub struct Stored {
    pub application_id: Option<u64>,
    pub guild_id: Option<u64>,
    pub client_secret: Option<Secret<String>>,
    pub bot_token: Option<Secret<String>>,
}

impl Stored {
    /// Complete configurations only.
    pub fn config(self) -> Option<DiscordConfig> {
        Some(DiscordConfig {
            application_id: self.application_id?,
            client_secret: self.client_secret?,
            bot_token: self.bot_token?,
            guild_id: self.guild_id?,
        })
    }
}

pub async fn stored(db: &PgPool, key: &EncryptionKey) -> Result<Stored, StoreError> {
    Ok(Stored {
        application_id: id(db, settings::DISCORD_APPLICATION_ID).await?,
        guild_id: id(db, settings::DISCORD_GUILD_ID).await?,
        client_secret: secret(db, key, secrets::DISCORD_CLIENT_SECRET).await?,
        bot_token: secret(db, key, secrets::DISCORD_BOT_TOKEN).await?,
    })
}

/// The configuration, if it is complete.
pub async fn load(db: &PgPool, key: &EncryptionKey) -> Result<Option<DiscordConfig>, StoreError> {
    Ok(stored(db, key).await?.config())
}

pub async fn save(
    tx: &mut sqlx::PgTransaction<'_>,
    key: &EncryptionKey,
    config: &DiscordConfig,
) -> Result<(), StoreError> {
    settings::set(
        &mut **tx,
        settings::DISCORD_APPLICATION_ID,
        config.application_id.to_string().into(),
    )
    .await?;
    settings::set(
        &mut **tx,
        settings::DISCORD_GUILD_ID,
        config.guild_id.to_string().into(),
    )
    .await?;
    for (name, value) in [
        (secrets::DISCORD_CLIENT_SECRET, &config.client_secret),
        (secrets::DISCORD_BOT_TOKEN, &config.bot_token),
    ] {
        let sealed = key.seal(value, &secrets::context(name))?;
        secrets::put(&mut **tx, name, &sealed).await?;
    }
    Ok(())
}

async fn id(db: &PgPool, name: &'static str) -> Result<Option<u64>, StoreError> {
    match settings::get_string(db, name).await? {
        Some(value) => value
            .parse()
            .map(Some)
            .map_err(|_| StoreError::Invalid(name)),
        None => Ok(None),
    }
}

async fn secret(
    db: &PgPool,
    key: &EncryptionKey,
    name: &str,
) -> Result<Option<Secret<String>>, StoreError> {
    match secrets::get(db, name).await? {
        Some(sealed) => Ok(Some(key.open(&sealed, &secrets::context(name))?)),
        None => Ok(None),
    }
}
