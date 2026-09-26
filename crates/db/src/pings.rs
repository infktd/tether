//! Fleet ping channels and the ping history.

use chrono::{DateTime, Utc};

use crate::PgPool;
use crate::accounts::AccountId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PingChannel {
    pub channel_id: i64,
    pub name: String,
}

/// The ping channels on the configured server (`guild_id`).
pub async fn channels<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    guild_id: i64,
) -> Result<Vec<PingChannel>, sqlx::Error> {
    sqlx::query_as!(
        PingChannel,
        r#"
        SELECT channel_id, name FROM core.discord_ping_channels
        WHERE guild_id = $1 ORDER BY name, channel_id
        "#,
        guild_id
    )
    .fetch_all(executor)
    .await
}

/// Whether the channel is (still) a ping channel on that server.
pub async fn is_channel<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    channel_id: i64,
    guild_id: i64,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar!(
        r#"
        SELECT EXISTS (
            SELECT 1 FROM core.discord_ping_channels WHERE channel_id = $1 AND guild_id = $2
        ) AS "exists!"
        "#,
        channel_id,
        guild_id,
    )
    .fetch_one(executor)
    .await
}

/// `false` if it was already a ping channel.
pub async fn add_channel<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    channel_id: i64,
    guild_id: i64,
    name: &str,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query!(
        r#"
        INSERT INTO core.discord_ping_channels (channel_id, guild_id, name) VALUES ($1, $2, $3)
        ON CONFLICT DO NOTHING
        "#,
        channel_id,
        guild_id,
        name,
    )
    .execute(executor)
    .await?;
    Ok(result.rows_affected() == 1)
}

pub async fn remove_channel<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    channel_id: i64,
) -> Result<Option<PingChannel>, sqlx::Error> {
    sqlx::query_as!(
        PingChannel,
        "DELETE FROM core.discord_ping_channels WHERE channel_id = $1 RETURNING channel_id, name",
        channel_id
    )
    .fetch_optional(executor)
    .await
}

/// Who a ping is for, as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    None,
    Here,
    Everyone,
    Role { id: i64, name: String },
}

impl Target {
    fn kind(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Here => "here",
            Self::Everyone => "everyone",
            Self::Role { .. } => "role",
        }
    }

    fn from_columns(kind: &str, role_id: Option<i64>, role_name: Option<String>) -> Self {
        match (kind, role_id) {
            ("here", _) => Self::Here,
            ("everyone", _) => Self::Everyone,
            ("role", Some(id)) => Self::Role {
                id,
                name: role_name.unwrap_or_default(),
            },
            _ => Self::None,
        }
    }
}

/// A ping's fleet details (aa-fleetpings' fields). All optional: a plain
/// message is still a ping.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Details {
    pub pre_ping: bool,
    pub fleet_type: Option<String>,
    /// The fleet type's embed colour when sent, `#rrggbb`.
    pub embed_color: Option<String>,
    pub fc_name: Option<String>,
    pub fleet_name: Option<String>,
    pub formup_location: Option<String>,
    /// `None` with `formup_now` false: not said.
    pub formup_time: Option<DateTime<Utc>>,
    pub formup_now: bool,
    pub comms: Option<String>,
    pub doctrine: Option<String>,
    pub doctrine_link: Option<String>,
    /// `None`: not said.
    pub srp: Option<bool>,
}

impl Details {
    /// Whether any detail was given.
    pub fn is_empty(&self) -> bool {
        *self
            == Self {
                pre_ping: self.pre_ping,
                ..Self::default()
            }
    }
}

#[derive(Debug)]
pub struct NewPing<'a> {
    pub account: AccountId,
    pub sender_name: &'a str,
    pub channel: &'a PingChannel,
    pub target: &'a Target,
    pub message: &'a str,
    pub details: &'a Details,
    pub nonce: &'a str,
}

pub async fn insert<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    ping: NewPing<'_>,
) -> Result<i64, sqlx::Error> {
    let (role_id, role_name) = match ping.target {
        Target::Role { id, name } => (Some(*id), Some(name.as_str())),
        _ => (None, None),
    };
    let d = ping.details;
    sqlx::query_scalar!(
        r#"
        INSERT INTO core.fleet_pings
            (account_id, sender_name, channel_id, channel_name, target, role_id, role_name,
             message, nonce, pre_ping, fleet_type, embed_color, fc_name, fleet_name,
             formup_location, formup_time, formup_now, comms, doctrine, doctrine_link, srp)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17,
                $18, $19, $20, $21)
        RETURNING id
        "#,
        ping.account.0,
        ping.sender_name,
        ping.channel.channel_id,
        ping.channel.name,
        ping.target.kind(),
        role_id,
        role_name,
        ping.message,
        ping.nonce,
        d.pre_ping,
        d.fleet_type,
        d.embed_color,
        d.fc_name,
        d.fleet_name,
        d.formup_location,
        d.formup_time,
        d.formup_now,
        d.comms,
        d.doctrine,
        d.doctrine_link,
        d.srp,
    )
    .fetch_one(executor)
    .await
}

#[derive(Debug, Clone)]
pub struct Ping {
    pub id: i64,
    pub sender_name: String,
    pub channel_id: i64,
    pub channel_name: String,
    pub target: Target,
    pub message: String,
    pub details: Details,
    pub nonce: String,
    pub created_at: DateTime<Utc>,
    /// Older than `stale_after` when read (by the database's clock).
    pub stale: bool,
    pub sent_at: Option<DateTime<Utc>>,
    pub failed_at: Option<DateTime<Utc>>,
    pub error: Option<String>,
}

struct PingRow {
    id: i64,
    sender_name: String,
    channel_id: i64,
    channel_name: String,
    target: String,
    role_id: Option<i64>,
    role_name: Option<String>,
    message: String,
    nonce: String,
    created_at: DateTime<Utc>,
    sent_at: Option<DateTime<Utc>>,
    failed_at: Option<DateTime<Utc>>,
    error: Option<String>,
    stale: bool,
    pre_ping: bool,
    fleet_type: Option<String>,
    embed_color: Option<String>,
    fc_name: Option<String>,
    fleet_name: Option<String>,
    formup_location: Option<String>,
    formup_time: Option<DateTime<Utc>>,
    formup_now: bool,
    comms: Option<String>,
    doctrine: Option<String>,
    doctrine_link: Option<String>,
    srp: Option<bool>,
}

impl From<PingRow> for Ping {
    fn from(r: PingRow) -> Self {
        Self {
            id: r.id,
            sender_name: r.sender_name,
            channel_id: r.channel_id,
            channel_name: r.channel_name,
            target: Target::from_columns(&r.target, r.role_id, r.role_name),
            message: r.message,
            details: Details {
                pre_ping: r.pre_ping,
                fleet_type: r.fleet_type,
                embed_color: r.embed_color,
                fc_name: r.fc_name,
                fleet_name: r.fleet_name,
                formup_location: r.formup_location,
                formup_time: r.formup_time,
                formup_now: r.formup_now,
                comms: r.comms,
                doctrine: r.doctrine,
                doctrine_link: r.doctrine_link,
                srp: r.srp,
            },
            nonce: r.nonce,
            created_at: r.created_at,
            stale: r.stale,
            sent_at: r.sent_at,
            failed_at: r.failed_at,
            error: r.error,
        }
    }
}

pub async fn get<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    id: i64,
    stale_after: std::time::Duration,
) -> Result<Option<Ping>, sqlx::Error> {
    let row = sqlx::query_as!(
        PingRow,
        r#"
        SELECT id, sender_name, channel_id, channel_name, target, role_id, role_name,
               message, nonce, created_at, sent_at, failed_at, error,
               created_at < now() - make_interval(secs => $2) AS "stale!",
               pre_ping, fleet_type, embed_color, fc_name, fleet_name, formup_location,
               formup_time, formup_now, comms, doctrine, doctrine_link, srp
        FROM core.fleet_pings WHERE id = $1
        "#,
        id,
        stale_after.as_secs_f64(),
    )
    .fetch_optional(executor)
    .await?;
    Ok(row.map(Ping::from))
}

pub async fn recent(
    pool: &PgPool,
    limit: i64,
    stale_after: std::time::Duration,
) -> Result<Vec<Ping>, sqlx::Error> {
    let rows = sqlx::query_as!(
        PingRow,
        r#"
        SELECT id, sender_name, channel_id, channel_name, target, role_id, role_name,
               message, nonce, created_at, sent_at, failed_at, error,
               created_at < now() - make_interval(secs => $2) AS "stale!",
               pre_ping, fleet_type, embed_color, fc_name, fleet_name, formup_location,
               formup_time, formup_now, comms, doctrine, doctrine_link, srp
        FROM core.fleet_pings ORDER BY created_at DESC, id DESC LIMIT $1
        "#,
        limit,
        stale_after.as_secs_f64(),
    )
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(Ping::from).collect())
}

/// Holds the account's row until the transaction ends, so concurrent
/// sends count each other for the rate limit.
pub async fn lock_sender(
    tx: &mut sqlx::PgTransaction<'_>,
    account: AccountId,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "SELECT 1 AS one FROM core.accounts WHERE id = $1 FOR NO KEY UPDATE",
        account.0
    )
    .fetch_optional(&mut **tx)
    .await?;
    Ok(())
}

/// How many pings the account sent in the last `secs` seconds.
pub async fn sent_since<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    account: AccountId,
    secs: f64,
) -> Result<i64, sqlx::Error> {
    let count = sqlx::query_scalar!(
        r#"
        SELECT count(*) AS "count!" FROM core.fleet_pings
        WHERE account_id = $1 AND created_at > now() - make_interval(secs => $2)
        "#,
        account.0,
        secs,
    )
    .fetch_one(executor)
    .await?;
    Ok(count)
}

pub async fn mark_sent<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    id: i64,
    discord_message_id: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        r#"
        UPDATE core.fleet_pings
        SET sent_at = now(), discord_message_id = $2, error = NULL, failed_at = NULL
        WHERE id = $1
        "#,
        id,
        discord_message_id,
    )
    .execute(executor)
    .await?;
    Ok(())
}

/// Records a failure; `failed` marks it final (no more retries).
pub async fn mark_error<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    id: i64,
    error: &str,
    failed: bool,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        r#"
        UPDATE core.fleet_pings
        SET error = $2, failed_at = CASE WHEN $3 THEN now() ELSE failed_at END
        WHERE id = $1
        "#,
        id,
        error,
        failed,
    )
    .execute(executor)
    .await?;
    Ok(())
}
