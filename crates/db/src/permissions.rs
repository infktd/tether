//! Permission grants and effective permissions.

use std::collections::BTreeSet;

use tether_core::permissions::CORE_PERMISSIONS;
use tether_core::states::StateId;

use crate::PgPool;
use crate::accounts::AccountId;
use crate::groups::GroupId;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Grantee {
    State(StateId),
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
    let (state, group) = split(grantee);
    sqlx::query_scalar!(
        r#"
        INSERT INTO core.permission_grants (permission, state_id, group_id)
        VALUES ($1, $2, $3)
        ON CONFLICT DO NOTHING
        RETURNING id
        "#,
        permission,
        state,
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
        "DELETE FROM core.permission_grants WHERE id = $1 RETURNING id, permission, state_id, group_id",
        grant_id
    )
    .fetch_optional(executor)
    .await?;
    Ok(row.and_then(|r| to_grant(r.id, r.permission, r.state_id, r.group_id)))
}

pub async fn list(pool: &PgPool) -> Result<Vec<Grant>, sqlx::Error> {
    let rows = sqlx::query!(
        "SELECT id, permission, state_id, group_id FROM core.permission_grants ORDER BY permission, id"
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|r| to_grant(r.id, r.permission, r.state_id, r.group_id))
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
/// to its state plus the grants to its groups.
pub async fn effective(pool: &PgPool, account: AccountId) -> Result<BTreeSet<String>, sqlx::Error> {
    effective_in(&mut *pool.acquire().await?, account).await
}

/// [`effective`], inside the caller's transaction.
pub async fn effective_in(
    conn: &mut sqlx::PgConnection,
    account: AccountId,
) -> Result<BTreeSet<String>, sqlx::Error> {
    // Deactivated accounts hold nothing (AA's inactive users), nor do
    // blacklisted ones; the owner always holds everything.
    let owner = sqlx::query_scalar!(
        r#"
        SELECT a.is_owner AS "is_owner!" FROM core.accounts a
        WHERE a.id = $1 AND a.active
          AND NOT core.blacklisted(a.id)
        "#,
        account.0
    )
    .fetch_optional(&mut *conn)
    .await?;
    match owner {
        None => return Ok(BTreeSet::new()),
        Some(true) => {
            return Ok(available(&mut *conn)
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
        WHERE g.state_id = a.state_id
           -- Groups count only while the account has a main (AA: services
           -- off until the owner picks one).
           OR (a.main_character_id IS NOT NULL
               AND g.group_id IN (SELECT group_id FROM core.group_members WHERE account_id = $1))
        "#,
        account.0,
    )
    .fetch_all(&mut *conn)
    .await?;
    Ok(permissions.into_iter().collect())
}

/// Every permission that can be granted: core's, then installed plugins',
/// with their descriptions.
pub async fn available<'e>(
    executor: impl sqlx::PgExecutor<'e>,
) -> Result<Vec<(String, String)>, sqlx::Error> {
    let mut all: Vec<(String, String)> = CORE_PERMISSIONS
        .iter()
        .map(|(name, description)| ((*name).to_owned(), (*description).to_owned()))
        .collect();
    let plugins = sqlx::query!(
        "SELECT permission, description FROM core.plugin_permissions ORDER BY permission"
    )
    .fetch_all(executor)
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
        RETURNING id, permission, state_id, group_id
        "#,
        plugin_id
    )
    .fetch_all(&mut *tx)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|r| to_grant(r.id, r.permission, r.state_id, r.group_id))
        .collect())
}

pub(crate) fn split(grantee: Grantee) -> (Option<i64>, Option<i64>) {
    match grantee {
        Grantee::State(state) => (Some(state.0), None),
        Grantee::Group(group) => (None, Some(group.0)),
    }
}

/// Rebuilds a grantee from its `(state_id, group_id)` columns.
pub(crate) fn grantee_from(state: Option<i64>, group: Option<i64>) -> Option<Grantee> {
    match (state, group) {
        (Some(state), None) => Some(Grantee::State(StateId(state))),
        (None, Some(group)) => Some(Grantee::Group(GroupId(group))),
        _ => None,
    }
}

fn to_grant(id: i64, permission: String, state: Option<i64>, group: Option<i64>) -> Option<Grant> {
    let grantee = grantee_from(state, group)?;
    Some(Grant {
        id,
        permission,
        grantee,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{accounts, groups, states};
    use tether_core::permissions::{ADMIN_AUDIT, ADMIN_GROUPS, DISCORD_ACCESS, REQUEST_GROUPS};
    use tether_core::states::Builtin;

    /// Character 1 claims ownership; others are ordinary accounts.
    async fn account(pool: &PgPool, id: i64) -> AccountId {
        let login = accounts::Login {
            character_id: id,
            character_name: "Pilot",
            owner_hash: "h",
        };
        accounts::sign_in(pool, login, id == 1)
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
    async fn grants_come_from_state_and_groups(pool: PgPool) {
        account(&pool, 1).await; // owner
        let pilot = account(&pool, 2).await;
        let member = states::builtin(&pool, Builtin::Member)
            .await
            .unwrap()
            .unwrap()
            .id;
        let blue = states::builtin(&pool, Builtin::Blue)
            .await
            .unwrap()
            .unwrap()
            .id;
        states::set_account_state(&pool, pilot, member, true)
            .await
            .unwrap();
        let officers = groups::create(
            &pool,
            "Officers",
            "",
            tether_core::groups::Flags {
                internal: true,
                hidden: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        grant(&pool, ADMIN_AUDIT, Grantee::State(member))
            .await
            .unwrap();
        let group_grant = grant(&pool, ADMIN_GROUPS, Grantee::Group(officers))
            .await
            .unwrap()
            .unwrap();
        grant(&pool, "admin.states", Grantee::State(blue))
            .await
            .unwrap();

        let before: Vec<_> = effective(&pool, pilot).await.unwrap().into_iter().collect();
        // Member also has Discord access and request_groups by default (AA).
        assert_eq!(before, vec![ADMIN_AUDIT, DISCORD_ACCESS, REQUEST_GROUPS]);

        groups::add_member(&pool, officers, pilot).await.unwrap();
        let with_group: Vec<_> = effective(&pool, pilot).await.unwrap().into_iter().collect();
        assert_eq!(
            with_group,
            vec![ADMIN_AUDIT, ADMIN_GROUPS, DISCORD_ACCESS, REQUEST_GROUPS]
        );

        revoke(&pool, group_grant).await.unwrap();
        let after: Vec<_> = effective(&pool, pilot).await.unwrap().into_iter().collect();
        assert_eq!(after, vec![ADMIN_AUDIT, DISCORD_ACCESS, REQUEST_GROUPS]);
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn duplicate_grants_are_ignored(pool: PgPool) {
        let member = states::builtin(&pool, Builtin::Member)
            .await
            .unwrap()
            .unwrap()
            .id;
        let first = grant(&pool, ADMIN_AUDIT, Grantee::State(member))
            .await
            .unwrap();
        let again = grant(&pool, ADMIN_AUDIT, Grantee::State(member))
            .await
            .unwrap();
        assert!(first.is_some());
        assert!(again.is_none());
        // Beside the defaults: request_groups (Member) and Discord access
        // (Member and Blue).
        assert_eq!(list(&pool).await.unwrap().len(), 4);
    }
}
