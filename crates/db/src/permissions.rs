//! Permission grants and effective permissions.

use std::collections::BTreeSet;

use tether_core::permissions::CORE_PERMISSIONS;
use tether_core::tiers::Tier;

use crate::PgPool;
use crate::accounts::AccountId;
use crate::groups::GroupId;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Grantee {
    Tier(Tier),
    Group(GroupId),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grant {
    pub id: i64,
    pub permission: String,
    pub grantee: Grantee,
}

/// Returns the grant id, or `None` if the same grant already exists.
pub async fn grant<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    permission: &str,
    grantee: Grantee,
) -> Result<Option<i64>, sqlx::Error> {
    let (tier, group) = split(grantee);
    sqlx::query_scalar!(
        r#"
        INSERT INTO core.permission_grants (permission, tier, group_id)
        VALUES ($1, $2, $3)
        ON CONFLICT DO NOTHING
        RETURNING id
        "#,
        permission,
        tier,
        group,
    )
    .fetch_optional(executor)
    .await
}

/// Removes a grant, returning what it was.
pub async fn revoke<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    grant_id: i64,
) -> Result<Option<Grant>, sqlx::Error> {
    let row = sqlx::query!(
        "DELETE FROM core.permission_grants WHERE id = $1 RETURNING id, permission, tier, group_id",
        grant_id
    )
    .fetch_optional(executor)
    .await?;
    Ok(row.and_then(|r| to_grant(r.id, r.permission, r.tier, r.group_id)))
}

pub async fn list(pool: &PgPool) -> Result<Vec<Grant>, sqlx::Error> {
    let rows = sqlx::query!(
        "SELECT id, permission, tier, group_id FROM core.permission_grants ORDER BY permission, id"
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|r| to_grant(r.id, r.permission, r.tier, r.group_id))
        .collect())
}

/// Permissions granted to a group.
pub async fn of_group<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    group: GroupId,
) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar!(
        "SELECT permission FROM core.permission_grants WHERE group_id = $1",
        group.0
    )
    .fetch_all(executor)
    .await
}

/// What the account may do: everything for the owner; otherwise the grants
/// to its tier plus the grants to its groups.
pub async fn effective(pool: &PgPool, account: AccountId) -> Result<BTreeSet<String>, sqlx::Error> {
    let owner = sqlx::query_scalar!(
        "SELECT is_owner FROM core.accounts WHERE id = $1",
        account.0
    )
    .fetch_optional(pool)
    .await?;
    match owner {
        None => return Ok(BTreeSet::new()),
        Some(true) => {
            return Ok(available(pool)
                .await?
                .into_iter()
                .map(|(name, _)| name)
                .collect());
        }
        Some(false) => {}
    }
    let permissions = sqlx::query_scalar!(
        r#"
        SELECT DISTINCT g.permission AS "permission!"
        FROM core.permission_grants g
        JOIN core.accounts a ON a.id = $1
        WHERE g.tier = a.tier
           OR g.group_id IN (SELECT group_id FROM core.group_members WHERE account_id = $1)
        "#,
        account.0,
    )
    .fetch_all(pool)
    .await?;
    Ok(permissions.into_iter().collect())
}

/// Every permission that can be granted: core's, then installed plugins',
/// with their descriptions.
pub async fn available(pool: &PgPool) -> Result<Vec<(String, String)>, sqlx::Error> {
    let mut all: Vec<(String, String)> = CORE_PERMISSIONS
        .iter()
        .map(|(name, description)| ((*name).to_owned(), (*description).to_owned()))
        .collect();
    let plugins = sqlx::query!(
        "SELECT permission, description FROM core.plugin_permissions ORDER BY permission"
    )
    .fetch_all(pool)
    .await?;
    all.extend(plugins.into_iter().map(|r| (r.permission, r.description)));
    Ok(all)
}

/// Whether a permission exists: core's, or an installed plugin's. A
/// plugin's is locked (shared) until the transaction ends, so an uninstall
/// can't remove it between this check and a grant.
pub async fn is_known<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    permission: &str,
) -> Result<bool, sqlx::Error> {
    if tether_core::permissions::is_known(permission) {
        return Ok(true);
    }
    let found = sqlx::query_scalar!(
        r#"SELECT true AS "found!" FROM core.plugin_permissions WHERE permission = $1 FOR SHARE"#,
        permission
    )
    .fetch_optional(executor)
    .await?;
    Ok(found.is_some())
}

/// Records a plugin's permissions, in its install's transaction. Any
/// grant of these names left from before (it shouldn't exist) is removed
/// first: a new install starts with nobody holding them.
pub async fn add_plugin_permissions(
    tx: &mut sqlx::PgConnection,
    plugin_id: &str,
    permissions: &[(String, String)],
) -> Result<(), sqlx::Error> {
    let names: Vec<String> = permissions.iter().map(|(name, _)| name.clone()).collect();
    sqlx::query!(
        "DELETE FROM core.permission_grants WHERE permission = ANY($1)",
        &names
    )
    .execute(&mut *tx)
    .await?;
    for (permission, description) in permissions {
        sqlx::query!(
            "INSERT INTO core.plugin_permissions (plugin_id, permission, description) VALUES ($1, $2, $3)",
            plugin_id,
            permission,
            description,
        )
        .execute(&mut *tx)
        .await?;
    }
    Ok(())
}

/// Removes every grant of a plugin's permissions (its permissions go with
/// the plugin's row), after locking them so no grant can slip in. Returns
/// what was removed, for the audit log.
pub async fn remove_plugin_grants(
    tx: &mut sqlx::PgConnection,
    plugin_id: &str,
) -> Result<Vec<Grant>, sqlx::Error> {
    sqlx::query_scalar!(
        "SELECT permission FROM core.plugin_permissions WHERE plugin_id = $1 FOR UPDATE",
        plugin_id
    )
    .fetch_all(&mut *tx)
    .await?;
    let rows = sqlx::query!(
        r#"
        DELETE FROM core.permission_grants
        WHERE permission IN (SELECT permission FROM core.plugin_permissions WHERE plugin_id = $1)
        RETURNING id, permission, tier, group_id
        "#,
        plugin_id
    )
    .fetch_all(&mut *tx)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|r| to_grant(r.id, r.permission, r.tier, r.group_id))
        .collect())
}

pub(crate) fn split(grantee: Grantee) -> (Option<&'static str>, Option<i64>) {
    match grantee {
        Grantee::Tier(tier) => (Some(tier.as_str()), None),
        Grantee::Group(group) => (None, Some(group.0)),
    }
}

/// Rebuilds a grantee from its `(tier, group_id)` columns.
pub(crate) fn grantee_from(tier: Option<String>, group: Option<i64>) -> Option<Grantee> {
    match (tier, group) {
        (Some(tier), None) => Some(Grantee::Tier(Tier::parse(&tier)?)),
        (None, Some(group)) => Some(Grantee::Group(GroupId(group))),
        _ => None,
    }
}

fn to_grant(
    id: i64,
    permission: String,
    tier: Option<String>,
    group: Option<i64>,
) -> Option<Grant> {
    let grantee = grantee_from(tier, group)?;
    Some(Grant {
        id,
        permission,
        grantee,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{accounts, groups, tiers};
    use tether_core::permissions::{ADMIN_AUDIT, ADMIN_GROUPS, JoinPolicy};

    /// Character 1 claims ownership; others are ordinary accounts.
    async fn account(pool: &PgPool, id: i64) -> AccountId {
        let login = accounts::Login {
            character_id: id,
            character_name: "Pilot",
            owner_hash: "h",
        };
        accounts::sign_in(pool, login, None, id == 1)
            .await
            .unwrap()
            .outcome
            .account()
            .unwrap()
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn owner_has_everything(pool: PgPool) {
        let owner = account(&pool, 1).await;
        assert_eq!(
            effective(&pool, owner).await.unwrap().len(),
            CORE_PERMISSIONS.len()
        );
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn grants_come_from_tier_and_groups(pool: PgPool) {
        account(&pool, 1).await; // owner
        let pilot = account(&pool, 2).await;
        tiers::set_account_tier(&pool, pilot, Tier::Member)
            .await
            .unwrap();
        let officers = groups::create(&pool, "Officers", "", JoinPolicy::Assigned)
            .await
            .unwrap();
        grant(&pool, ADMIN_AUDIT, Grantee::Tier(Tier::Member))
            .await
            .unwrap();
        let group_grant = grant(&pool, ADMIN_GROUPS, Grantee::Group(officers))
            .await
            .unwrap()
            .unwrap();
        grant(&pool, "admin.tiers", Grantee::Tier(Tier::Allied))
            .await
            .unwrap();

        let before: Vec<_> = effective(&pool, pilot).await.unwrap().into_iter().collect();
        assert_eq!(before, vec![ADMIN_AUDIT]);

        groups::add_member(&pool, officers, pilot).await.unwrap();
        let with_group: Vec<_> = effective(&pool, pilot).await.unwrap().into_iter().collect();
        assert_eq!(with_group, vec![ADMIN_AUDIT, ADMIN_GROUPS]);

        revoke(&pool, group_grant).await.unwrap();
        let after: Vec<_> = effective(&pool, pilot).await.unwrap().into_iter().collect();
        assert_eq!(after, vec![ADMIN_AUDIT]);
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn duplicate_grants_are_ignored(pool: PgPool) {
        let first = grant(&pool, ADMIN_AUDIT, Grantee::Tier(Tier::Member))
            .await
            .unwrap();
        let again = grant(&pool, ADMIN_AUDIT, Grantee::Tier(Tier::Member))
            .await
            .unwrap();
        assert!(first.is_some());
        assert!(again.is_none());
        assert_eq!(list(&pool).await.unwrap().len(), 1);
    }
}
