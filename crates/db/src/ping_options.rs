//! What the Fleet Pings form offers (aa-fleetpings' fleet types, doctrines,
//! formup locations and comms), and who may use which channel, target,
//! fleet type and doctrine.

use std::collections::HashSet;

use crate::accounts::AccountId;

/// A list the ping form offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    FleetType,
    Doctrine,
    Formup,
    Comms,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::FleetType => "fleet_type",
            Self::Doctrine => "doctrine",
            Self::Formup => "formup",
            Self::Comms => "comms",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "fleet_type" => Some(Self::FleetType),
            "doctrine" => Some(Self::Doctrine),
            "formup" => Some(Self::Formup),
            "comms" => Some(Self::Comms),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PingOption {
    pub id: i64,
    pub kind: Kind,
    pub name: String,
    pub link: Option<String>,
    pub color: Option<String>,
}

impl PingOption {
    /// Its key in `core.ping_restrictions`.
    pub fn item(&self) -> String {
        format!("option:{}", self.id)
    }
}

/// Every option, by kind and name.
pub async fn list<'e>(executor: impl sqlx::PgExecutor<'e>) -> Result<Vec<PingOption>, sqlx::Error> {
    let rows = sqlx::query!(
        "SELECT id, kind, name, link, color FROM core.ping_options ORDER BY kind, lower(name), id"
    )
    .fetch_all(executor)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|r| {
            Some(PingOption {
                id: r.id,
                kind: Kind::parse(&r.kind)?,
                name: r.name,
                link: r.link,
                color: r.color,
            })
        })
        .collect())
}

/// `None` if one of that kind already has the name.
pub async fn add<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    kind: Kind,
    name: &str,
    link: Option<&str>,
    color: Option<&str>,
) -> Result<Option<i64>, sqlx::Error> {
    sqlx::query_scalar!(
        r#"
        INSERT INTO core.ping_options (kind, name, link, color) VALUES ($1, $2, $3, $4)
        ON CONFLICT (kind, name) DO NOTHING
        RETURNING id
        "#,
        kind.as_str(),
        name,
        link,
        color,
    )
    .fetch_optional(executor)
    .await
}

/// A limit that was removed, for the audit log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Removed {
    pub item: String,
    pub state_id: Option<i64>,
    pub group_id: Option<i64>,
}

/// Deletes an option and who it was limited to. Returns its name and the
/// limits removed.
pub async fn delete(
    tx: &mut sqlx::PgConnection,
    id: i64,
) -> Result<Option<(String, Vec<Removed>)>, sqlx::Error> {
    let limits = clear(&mut *tx, &format!("option:{id}")).await?;
    let name = sqlx::query_scalar!(
        "DELETE FROM core.ping_options WHERE id = $1 RETURNING name",
        id
    )
    .fetch_optional(&mut *tx)
    .await?;
    Ok(name.map(|name| (name, limits)))
}

/// A limit on an item: to a state or a group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Restriction {
    pub id: i64,
    pub item: String,
    pub state_id: Option<i64>,
    pub group_id: Option<i64>,
    /// The state's or group's name.
    pub name: String,
}

pub async fn restrictions<'e>(
    executor: impl sqlx::PgExecutor<'e>,
) -> Result<Vec<Restriction>, sqlx::Error> {
    sqlx::query_as!(
        Restriction,
        r#"
        SELECT r.id, r.item, r.state_id, r.group_id,
               COALESCE(s.name, g.name) AS "name!"
        FROM core.ping_restrictions r
        LEFT JOIN core.states s ON s.id = r.state_id
        LEFT JOIN core.groups g ON g.id = r.group_id
        ORDER BY r.item, 5
        "#
    )
    .fetch_all(executor)
    .await
}

/// `false` if it was already limited to that state or group.
pub async fn restrict<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    item: &str,
    state_id: Option<i64>,
    group_id: Option<i64>,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query!(
        r#"
        INSERT INTO core.ping_restrictions (item, state_id, group_id) VALUES ($1, $2, $3)
        ON CONFLICT DO NOTHING
        "#,
        item,
        state_id,
        group_id,
    )
    .execute(executor)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// Removes a limit; returns what it was.
pub async fn unrestrict<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    id: i64,
) -> Result<Option<Removed>, sqlx::Error> {
    sqlx::query_as!(
        Removed,
        "DELETE FROM core.ping_restrictions WHERE id = $1 RETURNING item, state_id, group_id",
        id
    )
    .fetch_optional(executor)
    .await
}

/// Drops the limits on an item that's gone (a channel no longer used);
/// returns them.
pub async fn clear<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    item: &str,
) -> Result<Vec<Removed>, sqlx::Error> {
    sqlx::query_as!(
        Removed,
        "DELETE FROM core.ping_restrictions WHERE item = $1 RETURNING item, state_id, group_id",
        item
    )
    .fetch_all(executor)
    .await
}

/// Which items an account may use: everything without limits, plus the
/// limited items its state or one of its groups is listed on (groups
/// only while it has a main, as for permissions). Returns
/// `(limited, allowed)`: an item is usable if it isn't in `limited` or is
/// in `allowed`.
pub async fn access<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    account: AccountId,
) -> Result<Access, sqlx::Error> {
    let rows = sqlx::query!(
        r#"
        SELECT r.item,
               -- COALESCE: a comparison with a NULL column is NULL, and
               -- bool_or skips NULLs.
               COALESCE(bool_or(COALESCE(r.state_id = a.state_id, false)
                       OR (a.main_character_id IS NOT NULL AND COALESCE(r.group_id IN (
                           SELECT group_id FROM core.group_members WHERE account_id = a.id), false))),
                   false) AS "allowed!"
        FROM core.ping_restrictions r
        JOIN core.accounts a ON a.id = $1
        GROUP BY r.item
        "#,
        account.0
    )
    .fetch_all(executor)
    .await?;
    Ok(Access {
        limited: rows.iter().map(|r| r.item.clone()).collect(),
        allowed: rows
            .into_iter()
            .filter(|r| r.allowed)
            .map(|r| r.item)
            .collect(),
    })
}

#[derive(Debug, Clone, Default)]
pub struct Access {
    limited: HashSet<String>,
    allowed: HashSet<String>,
}

impl Access {
    /// Whether the account may use the item.
    pub fn may_use(&self, item: &str) -> bool {
        !self.limited.contains(item) || self.allowed.contains(item)
    }
}
