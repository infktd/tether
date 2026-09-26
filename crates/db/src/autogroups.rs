//! Auto Groups (AA's): configs that keep a group per corporation and
//! alliance of the mains in their states.

use tether_core::states::StateId;

use crate::accounts::AccountId;
use crate::groups::GroupId;

/// Where a group's name comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Name,
    Ticker,
}

impl Source {
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "name" => Some(Self::Name),
            "ticker" => Some(Self::Ticker),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Ticker => "ticker",
        }
    }
}

/// What a group is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    Corporation,
    Alliance,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Corporation => "corporation",
            Self::Alliance => "alliance",
        }
    }

    fn parse(text: &str) -> Self {
        if text == "alliance" {
            Self::Alliance
        } else {
            Self::Corporation
        }
    }
}

/// A config's settings, as saved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub corp_groups: bool,
    pub corp_prefix: String,
    pub corp_source: Source,
    pub alliance_groups: bool,
    pub alliance_prefix: String,
    pub alliance_source: Source,
    pub replace_spaces: bool,
    pub replace_with: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub id: i64,
    pub settings: Settings,
    pub states: Vec<StateId>,
}

pub async fn configs<'e>(executor: impl sqlx::PgExecutor<'e>) -> Result<Vec<Config>, sqlx::Error> {
    let rows = sqlx::query!(
        r#"
        SELECT c.id, c.corp_groups, c.corp_prefix, c.corp_source, c.alliance_groups,
               c.alliance_prefix, c.alliance_source, c.replace_spaces, c.replace_with,
               COALESCE((SELECT array_agg(s.state_id) FROM core.autogroup_config_states s
                         WHERE s.config_id = c.id), '{}') AS "states!"
        FROM core.autogroup_configs c ORDER BY c.id
        "#
    )
    .fetch_all(executor)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| Config {
            id: r.id,
            settings: Settings {
                corp_groups: r.corp_groups,
                corp_prefix: r.corp_prefix,
                corp_source: Source::parse(&r.corp_source).unwrap_or(Source::Name),
                alliance_groups: r.alliance_groups,
                alliance_prefix: r.alliance_prefix,
                alliance_source: Source::parse(&r.alliance_source).unwrap_or(Source::Name),
                replace_spaces: r.replace_spaces,
                replace_with: r.replace_with,
            },
            states: r.states.into_iter().map(StateId).collect(),
        })
        .collect())
}

pub async fn create(
    tx: &mut sqlx::PgConnection,
    settings: &Settings,
    states: &[StateId],
) -> Result<i64, sqlx::Error> {
    let id = sqlx::query_scalar!(
        r#"
        INSERT INTO core.autogroup_configs
            (corp_groups, corp_prefix, corp_source, alliance_groups, alliance_prefix,
             alliance_source, replace_spaces, replace_with)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING id
        "#,
        settings.corp_groups,
        settings.corp_prefix,
        settings.corp_source.as_str(),
        settings.alliance_groups,
        settings.alliance_prefix,
        settings.alliance_source.as_str(),
        settings.replace_spaces,
        settings.replace_with,
    )
    .fetch_one(&mut *tx)
    .await?;
    set_states(tx, id, states).await?;
    Ok(id)
}

/// False if there's no such config.
pub async fn update(
    tx: &mut sqlx::PgConnection,
    id: i64,
    settings: &Settings,
    states: &[StateId],
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query!(
        r#"
        UPDATE core.autogroup_configs SET corp_groups = $2, corp_prefix = $3, corp_source = $4,
            alliance_groups = $5, alliance_prefix = $6, alliance_source = $7,
            replace_spaces = $8, replace_with = $9
        WHERE id = $1
        "#,
        id,
        settings.corp_groups,
        settings.corp_prefix,
        settings.corp_source.as_str(),
        settings.alliance_groups,
        settings.alliance_prefix,
        settings.alliance_source.as_str(),
        settings.replace_spaces,
        settings.replace_with,
    )
    .execute(&mut *tx)
    .await?;
    if result.rows_affected() == 0 {
        return Ok(false);
    }
    set_states(tx, id, states).await?;
    Ok(true)
}

async fn set_states(
    tx: &mut sqlx::PgConnection,
    id: i64,
    states: &[StateId],
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "DELETE FROM core.autogroup_config_states WHERE config_id = $1",
        id
    )
    .execute(&mut *tx)
    .await?;
    let ids: Vec<i64> = states.iter().map(|s| s.0).collect();
    sqlx::query!(
        r#"
        INSERT INTO core.autogroup_config_states (config_id, state_id)
        SELECT $1, unnest($2::bigint[]) ON CONFLICT DO NOTHING
        "#,
        id,
        &ids
    )
    .execute(&mut *tx)
    .await?;
    Ok(())
}

/// Deletes a config; its groups go with it (a trigger).
pub async fn delete(tx: &mut sqlx::PgConnection, id: i64) -> Result<bool, sqlx::Error> {
    // Its groups first, so each row's trigger deletes its group.
    sqlx::query!("DELETE FROM core.autogroup_groups WHERE config_id = $1", id)
        .execute(&mut *tx)
        .await?;
    let result = sqlx::query!("DELETE FROM core.autogroup_configs WHERE id = $1", id)
        .execute(&mut *tx)
        .await?;
    Ok(result.rows_affected() == 1)
}

/// A config's group for a corporation or alliance, if it has one yet.
pub async fn group_for<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    config: i64,
    kind: Kind,
    entity: i64,
) -> Result<Option<GroupId>, sqlx::Error> {
    let id = sqlx::query_scalar!(
        "SELECT group_id FROM core.autogroup_groups WHERE config_id = $1 AND kind = $2 AND entity_id = $3",
        config,
        kind.as_str(),
        entity
    )
    .fetch_optional(executor)
    .await?;
    Ok(id.map(GroupId))
}

pub async fn insert_group<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    group: GroupId,
    config: i64,
    kind: Kind,
    entity: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "INSERT INTO core.autogroup_groups (group_id, config_id, kind, entity_id) VALUES ($1, $2, $3, $4)",
        group.0,
        config,
        kind.as_str(),
        entity
    )
    .execute(executor)
    .await?;
    Ok(())
}

/// An Auto Group the account is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Membership {
    pub group: GroupId,
    pub config: i64,
    pub kind: Kind,
    pub entity: i64,
}

pub async fn memberships<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    account: AccountId,
) -> Result<Vec<Membership>, sqlx::Error> {
    let rows = sqlx::query!(
        r#"
        SELECT a.group_id, a.config_id, a.kind, a.entity_id
        FROM core.autogroup_groups a
        JOIN core.group_members m ON m.group_id = a.group_id
        WHERE m.account_id = $1
        "#,
        account.0
    )
    .fetch_all(executor)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| Membership {
            group: GroupId(r.group_id),
            config: r.config_id,
            kind: Kind::parse(&r.kind),
            entity: r.entity_id,
        })
        .collect())
}

/// Whether Tether keeps this group's members as an Auto Group.
pub async fn is_auto<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    group: GroupId,
) -> Result<bool, sqlx::Error> {
    let found = sqlx::query_scalar!(
        r#"SELECT true AS "found!" FROM core.autogroup_groups WHERE group_id = $1"#,
        group.0
    )
    .fetch_optional(executor)
    .await?;
    Ok(found.is_some())
}

/// A config's groups: `(group, kind, entity, current name)`.
pub async fn groups_of<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    config: i64,
) -> Result<Vec<(GroupId, Kind, i64, String)>, sqlx::Error> {
    let rows = sqlx::query!(
        r#"
        SELECT a.group_id, a.kind, a.entity_id, g.name
        FROM core.autogroup_groups a JOIN core.groups g ON g.id = a.group_id
        WHERE a.config_id = $1 ORDER BY g.name
        "#,
        config
    )
    .fetch_all(executor)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            (
                GroupId(r.group_id),
                Kind::parse(&r.kind),
                r.entity_id,
                r.name,
            )
        })
        .collect())
}

/// Auto Groups nobody is in any more and nobody configured (no grants,
/// Discord roles or leaders): removed, their groups with them. Returns
/// `(group, name)` for each, for the audit log.
pub async fn remove_empty<'e>(
    executor: impl sqlx::PgExecutor<'e>,
) -> Result<Vec<(GroupId, String)>, sqlx::Error> {
    let rows = sqlx::query!(
        r#"
        WITH gone AS (
            DELETE FROM core.autogroup_groups a
            WHERE NOT EXISTS (SELECT 1 FROM core.group_members m WHERE m.group_id = a.group_id)
              AND NOT EXISTS (SELECT 1 FROM core.permission_grants p WHERE p.group_id = a.group_id)
              AND NOT EXISTS (SELECT 1 FROM core.discord_role_mappings d WHERE d.group_id = a.group_id)
              AND NOT EXISTS (SELECT 1 FROM core.group_leaders l WHERE l.group_id = a.group_id)
              AND NOT EXISTS (
                  SELECT 1 FROM core.group_leader_groups lg
                  WHERE lg.group_id = a.group_id OR lg.leader_group_id = a.group_id
              )
            RETURNING a.group_id
        )
        SELECT g.id, g.name FROM gone JOIN core.groups g ON g.id = gone.group_id
        "#
    )
    .fetch_all(executor)
    .await?;
    Ok(rows.into_iter().map(|r| (GroupId(r.id), r.name)).collect())
}

/// Renames a group (Auto Groups follow corporation and alliance names).
pub async fn rename_group<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    group: GroupId,
    name: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "UPDATE core.groups SET name = $2 WHERE id = $1",
        group.0,
        name
    )
    .execute(executor)
    .await?;
    Ok(())
}

/// Queues the Auto Groups sync unless one is already waiting.
pub async fn queue_sync<'e>(executor: impl sqlx::PgExecutor<'e>) -> Result<(), sqlx::Error> {
    sqlx::query!(
        r#"
        INSERT INTO core.jobs (kind, payload, max_attempts)
        SELECT 'autogroups.sync', '{}', 5
        WHERE NOT EXISTS (SELECT 1 FROM core.jobs WHERE kind = 'autogroups.sync' AND state = 'queued')
        "#
    )
    .execute(executor)
    .await?;
    Ok(())
}
