//! Secure Groups (aa-securegroups): which groups are smart, their filters,
//! the facts filters read, and grace periods.

use std::collections::{BTreeSet, HashMap};

use chrono::{DateTime, Utc};
use tether_core::smart::{Facts, Filter, Rule};

use crate::accounts::AccountId;
use crate::groups::GroupId;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Settings {
    /// Adds everyone who passes; otherwise they request.
    pub auto_join: bool,
    /// Days a member who stops passing keeps the group; 0 removes at once.
    pub grace_days: i32,
    pub notify: bool,
}

pub async fn settings<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    group: GroupId,
) -> Result<Option<Settings>, sqlx::Error> {
    sqlx::query_as!(
        Settings,
        "SELECT auto_join, grace_days, notify FROM core.smart_groups WHERE group_id = $1",
        group.0
    )
    .fetch_optional(executor)
    .await
}

/// Makes a group smart with these settings, or (`None`) ordinary again,
/// dropping its filters and grace periods.
pub async fn set_settings<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    group: GroupId,
    settings: Option<Settings>,
) -> Result<(), sqlx::Error> {
    match settings {
        Some(s) => {
            sqlx::query!(
                r#"
                INSERT INTO core.smart_groups (group_id, auto_join, grace_days, notify)
                VALUES ($1, $2, $3, $4)
                ON CONFLICT (group_id) DO UPDATE
                SET auto_join = $2, grace_days = $3, notify = $4
                "#,
                group.0,
                s.auto_join,
                s.grace_days,
                s.notify,
            )
            .execute(executor)
            .await?;
        }
        None => {
            sqlx::query!("DELETE FROM core.smart_groups WHERE group_id = $1", group.0)
                .execute(executor)
                .await?;
        }
    }
    Ok(())
}

/// Every smart group.
pub async fn all<'e>(
    executor: impl sqlx::PgExecutor<'e>,
) -> Result<Vec<(GroupId, Settings)>, sqlx::Error> {
    let rows = sqlx::query!(
        "SELECT group_id, auto_join, grace_days, notify FROM core.smart_groups ORDER BY group_id"
    )
    .fetch_all(executor)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            (
                GroupId(r.group_id),
                Settings {
                    auto_join: r.auto_join,
                    grace_days: r.grace_days,
                    notify: r.notify,
                },
            )
        })
        .collect())
}

/// A group's filters, and the ids of any that no longer read (an old kind
/// or a hand-edited row). Callers must fail closed on those: a broken
/// filter can't be allowed to widen who gets in.
pub async fn rules<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    group: GroupId,
) -> Result<(Vec<Rule>, Vec<i64>), sqlx::Error> {
    let rows = sqlx::query!(
        r#"
        SELECT id, kind, config, reversed FROM core.smart_filters
        WHERE group_id = $1 ORDER BY id
        "#,
        group.0
    )
    .fetch_all(executor)
    .await?;
    let mut rules = Vec::new();
    let mut broken = Vec::new();
    for r in rows {
        match serde_json::from_value::<Filter>(serde_json::json!({
            "kind": r.kind,
            "config": r.config,
        })) {
            Ok(filter) => rules.push(Rule {
                id: r.id,
                filter,
                reversed: r.reversed,
            }),
            Err(_) => broken.push(r.id),
        }
    }
    Ok((rules, broken))
}

pub async fn add_filter<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    group: GroupId,
    filter: &Filter,
    reversed: bool,
) -> Result<i64, sqlx::Error> {
    let stored = serde_json::to_value(filter).map_err(|e| sqlx::Error::Protocol(e.to_string()))?;
    sqlx::query_scalar!(
        r#"
        INSERT INTO core.smart_filters (group_id, kind, config, reversed)
        VALUES ($1, $2, $3, $4) RETURNING id
        "#,
        group.0,
        filter.kind(),
        stored
            .get("config")
            .cloned()
            .unwrap_or_else(|| serde_json::json!({})),
        reversed,
    )
    .fetch_one(executor)
    .await
}

/// `false` if the group has no such filter.
pub async fn delete_filter<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    group: GroupId,
    id: i64,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query!(
        "DELETE FROM core.smart_filters WHERE id = $1 AND group_id = $2",
        id,
        group.0
    )
    .execute(executor)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// What filters read, for every account that may be in a group (active,
/// with a main, not blacklisted), or only the ones given.
pub async fn facts<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    only: Option<&[AccountId]>,
) -> Result<HashMap<AccountId, Facts>, sqlx::Error> {
    let ids: Option<Vec<i64>> = only.map(|a| a.iter().map(|a| a.0).collect());
    let rows = sqlx::query!(
        r#"
        SELECT a.id, a.state_id, a.compliant,
               m.corporation_id AS main_corporation, m.alliance_id AS main_alliance,
               (EXTRACT(epoch FROM now() - m.birthday) / 86400)::bigint AS age_days,
               ARRAY(
                   SELECT DISTINCT x FROM core.characters c,
                        LATERAL unnest(ARRAY[c.corporation_id, c.alliance_id]) AS x
                   WHERE c.account_id = a.id AND x IS NOT NULL
               ) AS "affiliations!",
               ARRAY(SELECT gm.group_id FROM core.group_members gm WHERE gm.account_id = a.id)
                   AS "groups!"
        FROM core.accounts a
        JOIN core.characters m ON m.id = a.main_character_id
        WHERE a.active AND NOT core.blacklisted(a.id)
          AND ($1::bigint[] IS NULL OR a.id = ANY($1))
        "#,
        ids.as_deref() as Option<&[i64]>,
    )
    .fetch_all(executor)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            (
                AccountId(r.id),
                Facts {
                    state: r.state_id,
                    main_affiliation: [r.main_corporation, r.main_alliance]
                        .into_iter()
                        .flatten()
                        .collect(),
                    affiliations: r.affiliations.into_iter().collect::<BTreeSet<_>>(),
                    main_age_days: r.age_days,
                    groups: r.groups.into_iter().collect(),
                    compliant: r.compliant,
                },
            )
        })
        .collect())
}

/// Members in their grace period, and since when.
pub async fn grace<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    group: GroupId,
) -> Result<HashMap<AccountId, DateTime<Utc>>, sqlx::Error> {
    let rows = sqlx::query!(
        "SELECT account_id, since FROM core.smart_grace WHERE group_id = $1",
        group.0
    )
    .fetch_all(executor)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| (AccountId(r.account_id), r.since))
        .collect())
}

pub async fn start_grace<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    group: GroupId,
    account: AccountId,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "INSERT INTO core.smart_grace (group_id, account_id) VALUES ($1, $2) ON CONFLICT DO NOTHING",
        group.0,
        account.0
    )
    .execute(executor)
    .await?;
    Ok(())
}

/// `true` if the account was in its grace period.
pub async fn end_grace<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    group: GroupId,
    account: AccountId,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query!(
        "DELETE FROM core.smart_grace WHERE group_id = $1 AND account_id = $2",
        group.0,
        account.0
    )
    .execute(executor)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// Mains whose birthday isn't known yet, for the character age filter:
/// never asked first, then those asked longest ago.
pub async fn mains_without_birthday<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    limit: i64,
) -> Result<Vec<i64>, sqlx::Error> {
    sqlx::query_scalar!(
        r#"
        SELECT c.id FROM core.accounts a JOIN core.characters c ON c.id = a.main_character_id
        WHERE a.active AND c.birthday IS NULL
        ORDER BY c.birthday_checked_at NULLS FIRST, c.id LIMIT $1
        "#,
        limit
    )
    .fetch_all(executor)
    .await
}

/// Records that ESI was asked (and failed), so the character waits its
/// turn again.
pub async fn birthday_checked<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    character: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "UPDATE core.characters SET birthday_checked_at = now() WHERE id = $1",
        character
    )
    .execute(executor)
    .await?;
    Ok(())
}

/// A group's members' accounts.
pub async fn member_ids<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    group: GroupId,
) -> Result<Vec<AccountId>, sqlx::Error> {
    let ids = sqlx::query_scalar!(
        "SELECT account_id FROM core.group_members WHERE group_id = $1",
        group.0
    )
    .fetch_all(executor)
    .await?;
    Ok(ids.into_iter().map(AccountId).collect())
}

/// Adds an account the sweep found passing, checked again as it's added:
/// still active, with a main, and not blacklisted. `false` if not added.
pub async fn add_if_eligible<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    group: GroupId,
    account: AccountId,
) -> Result<bool, sqlx::Error> {
    let added = sqlx::query_scalar!(
        r#"
        INSERT INTO core.group_members (group_id, account_id)
        SELECT $1, a.id FROM core.accounts a
        WHERE a.id = $2 AND a.active AND a.main_character_id IS NOT NULL
          AND NOT core.blacklisted(a.id)
        ON CONFLICT DO NOTHING
        RETURNING true AS "added!"
        "#,
        group.0,
        account.0,
    )
    .fetch_optional(executor)
    .await?;
    Ok(added.is_some())
}

/// Grace rows of accounts no longer in the group.
pub async fn clear_stale_grace<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    group: GroupId,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        r#"
        DELETE FROM core.smart_grace g WHERE g.group_id = $1
          AND NOT EXISTS (SELECT 1 FROM core.group_members m
                          WHERE m.group_id = g.group_id AND m.account_id = g.account_id)
        "#,
        group.0
    )
    .execute(executor)
    .await?;
    Ok(())
}

pub async fn set_birthday<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    character: i64,
    birthday: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "UPDATE core.characters SET birthday = $2 WHERE id = $1",
        character,
        birthday
    )
    .execute(executor)
    .await?;
    Ok(())
}

/// Whether any smart group filters by character age (so birthdays are
/// worth fetching).
pub async fn uses_age<'e>(executor: impl sqlx::PgExecutor<'e>) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM core.smart_filters WHERE kind = 'character_age') AS "e!""#
    )
    .fetch_one(executor)
    .await
}
