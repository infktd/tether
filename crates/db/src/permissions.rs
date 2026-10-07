//! Permission grants and effective permissions.

use std::collections::BTreeSet;
use std::sync::Arc;

use tether_core::permissions::CORE_PERMISSIONS;
use tether_core::states::StateId;

use crate::PgPool;
use crate::accounts::AccountId;
use crate::groups::GroupId;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Grantee {
    State(StateId),
    Group(GroupId),
    /// A single user (AA's user permissions).
    Account(AccountId),
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
    let (state, group, account) = split(grantee);
    sqlx::query_scalar!(
        r#"
        INSERT INTO core.permission_grants (permission, state_id, group_id, account_id)
        VALUES ($1, $2, $3, $4)
        ON CONFLICT DO NOTHING
        RETURNING id
        "#,
        permission,
        state,
        group,
        account,
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
        "DELETE FROM core.permission_grants WHERE id = $1 RETURNING id, permission, state_id, group_id, account_id",
        grant_id
    )
    .fetch_optional(executor)
    .await?;
    Ok(row.and_then(|r| to_grant(r.id, r.permission, r.state_id, r.group_id, r.account_id)))
}

pub async fn list(pool: &PgPool) -> Result<Vec<Grant>, sqlx::Error> {
    let rows = sqlx::query!(
        "SELECT id, permission, state_id, group_id, account_id FROM core.permission_grants ORDER BY permission, id"
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|r| to_grant(r.id, r.permission, r.state_id, r.group_id, r.account_id))
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

/// Permissions granted to the account itself (AA's user permissions), as
/// `(grant id, permission)`, by permission.
pub async fn of_account<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    account: AccountId,
) -> Result<Vec<(i64, String)>, sqlx::Error> {
    let rows = sqlx::query!(
        "SELECT id, permission FROM core.permission_grants WHERE account_id = $1 ORDER BY permission",
        account.0
    )
    .fetch_all(executor)
    .await?;
    Ok(rows.into_iter().map(|r| (r.id, r.permission)).collect())
}

/// A request made with a personal access token: its account, the token,
/// and the token's scopes. While it runs, that account holds only the
/// permissions both it and the token have, in every check (grants,
/// "you must already hold it", a superuser's), and is never a superuser.
#[derive(Debug, Clone)]
pub struct TokenScope {
    pub account: AccountId,
    pub token_id: i64,
    pub scopes: Arc<BTreeSet<String>>,
}

tokio::task_local! {
    static TOKEN_SCOPE: TokenScope;
}

/// Runs a request under a token's scope.
pub async fn with_token_scope<F: std::future::Future>(scope: TokenScope, request: F) -> F::Output {
    TOKEN_SCOPE.scope(scope, request).await
}

/// The token the current request runs on, if any.
pub fn token_scope() -> Option<TokenScope> {
    TOKEN_SCOPE.try_with(Clone::clone).ok()
}

/// The scopes limiting `account` in the current request, if it's the
/// token's account.
pub fn scoped_by_token(account: AccountId) -> Option<Arc<BTreeSet<String>>> {
    token_scope()
        .filter(|t| t.account == account)
        .map(|t| t.scopes)
}

/// What the account may do: everything for a superuser; otherwise the
/// grants to it, to its state and to its groups (AA's user, state and
/// group permissions).
pub async fn effective(pool: &PgPool, account: AccountId) -> Result<BTreeSet<String>, sqlx::Error> {
    effective_in(&mut *pool.acquire().await?, account).await
}

/// [`effective`], inside the caller's transaction.
pub async fn effective_in(
    conn: &mut sqlx::PgConnection,
    account: AccountId,
) -> Result<BTreeSet<String>, sqlx::Error> {
    let mut held = held_in(conn, account).await?;
    if let Some(scopes) = scoped_by_token(account) {
        held.retain(|p| scopes.contains(p));
    }
    Ok(held)
}

/// What the account holds, whatever token the request runs on.
async fn held_in(
    conn: &mut sqlx::PgConnection,
    account: AccountId,
) -> Result<BTreeSet<String>, sqlx::Error> {
    // Deactivated accounts hold nothing (AA's inactive users); superusers
    // always hold everything. A blacklisted account holds what the
    // Blacklist state is granted (AA).
    let owner = sqlx::query_scalar!(
        r#"
        SELECT a.is_owner AS "is_owner!" FROM core.accounts a
        WHERE a.id = $1 AND a.active
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
           -- Its own grants, main or not (AA's user permissions).
           OR g.account_id = $1
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

/// A grant moved to a renamed permission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MovedGrant {
    /// The grant, under its new name.
    pub grant: Grant,
    pub from: String,
}

/// What [`sync_plugin_permissions`] changed, for the audit log.
#[derive(Debug, Default)]
pub struct PermissionsSynced {
    pub removed: Vec<Grant>,
    pub moved: Vec<MovedGrant>,
}

/// Makes a plugin's permissions exactly `permissions` on an upgrade or
/// rollback: new ones are added, descriptions updated, and ones it no
/// longer declares removed with every grant of them. Grants of the ones it
/// keeps stay.
///
/// `renames` (old name, new name) move grants first: only from a
/// permission this plugin held that it no longer declares, to one it
/// declares now and didn't hold, so grants never cross plugins or merge
/// into a permission someone already holds. A holder who somehow has
/// both keeps the new one; the duplicate goes with the old name.
pub async fn sync_plugin_permissions(
    tx: &mut sqlx::PgConnection,
    plugin_id: &str,
    permissions: &[(String, String)],
    renames: &[(String, String)],
) -> Result<PermissionsSynced, sqlx::Error> {
    let names: Vec<String> = permissions.iter().map(|(name, _)| name.clone()).collect();
    let held = sqlx::query_scalar!(
        "SELECT permission FROM core.plugin_permissions WHERE plugin_id = $1 FOR UPDATE",
        plugin_id
    )
    .fetch_all(&mut *tx)
    .await?;
    // New ones start with nobody holding them, as at install.
    let new: Vec<String> = names
        .iter()
        .filter(|n| !held.contains(n))
        .cloned()
        .collect();
    sqlx::query!(
        "DELETE FROM core.permission_grants WHERE permission = ANY($1)",
        &new
    )
    .execute(&mut *tx)
    .await?;
    let mut moved = Vec::new();
    for (from, to) in renames {
        if !held.contains(from) || names.contains(from) || !new.contains(to) {
            continue;
        }
        let rows = sqlx::query!(
            r#"
            UPDATE core.permission_grants g SET permission = $2
            WHERE g.permission = $1
              AND NOT EXISTS (
                  SELECT 1 FROM core.permission_grants h
                  WHERE h.permission = $2
                    AND h.state_id IS NOT DISTINCT FROM g.state_id
                    AND h.group_id IS NOT DISTINCT FROM g.group_id
                    AND h.account_id IS NOT DISTINCT FROM g.account_id
              )
            RETURNING id, permission, state_id, group_id, account_id
            "#,
            from,
            to
        )
        .fetch_all(&mut *tx)
        .await?;
        moved.extend(rows.into_iter().filter_map(|r| {
            to_grant(r.id, r.permission, r.state_id, r.group_id, r.account_id).map(|grant| {
                MovedGrant {
                    grant,
                    from: from.clone(),
                }
            })
        }));
    }
    let rows = sqlx::query!(
        r#"
        DELETE FROM core.permission_grants
        WHERE permission IN (
            SELECT permission FROM core.plugin_permissions
            WHERE plugin_id = $1 AND NOT permission = ANY($2)
        )
        RETURNING id, permission, state_id, group_id, account_id
        "#,
        plugin_id,
        &names
    )
    .fetch_all(&mut *tx)
    .await?;
    sqlx::query!(
        "DELETE FROM core.plugin_permissions WHERE plugin_id = $1 AND NOT permission = ANY($2)",
        plugin_id,
        &names
    )
    .execute(&mut *tx)
    .await?;
    for (permission, description) in permissions {
        // A name another plugin holds fails on the UNIQUE constraint, as
        // at install; the name carries the plugin's id, so it can't.
        sqlx::query!(
            r#"
            INSERT INTO core.plugin_permissions (plugin_id, permission, description)
            VALUES ($1, $2, $3)
            ON CONFLICT (plugin_id, permission) DO UPDATE SET description = EXCLUDED.description
            "#,
            plugin_id,
            permission,
            description,
        )
        .execute(&mut *tx)
        .await?;
    }
    Ok(PermissionsSynced {
        removed: rows
            .into_iter()
            .filter_map(|r| to_grant(r.id, r.permission, r.state_id, r.group_id, r.account_id))
            .collect(),
        moved,
    })
}

/// How many grants each permission has, for an upgrade's review.
pub async fn grant_counts<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    permissions: &[String],
) -> Result<Vec<(String, i64)>, sqlx::Error> {
    let rows = sqlx::query!(
        r#"SELECT permission, count(*) AS "count!" FROM core.permission_grants
           WHERE permission = ANY($1) GROUP BY permission"#,
        permissions
    )
    .fetch_all(executor)
    .await?;
    Ok(rows.into_iter().map(|r| (r.permission, r.count)).collect())
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
        RETURNING id, permission, state_id, group_id, account_id
        "#,
        plugin_id
    )
    .fetch_all(&mut *tx)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|r| to_grant(r.id, r.permission, r.state_id, r.group_id, r.account_id))
        .collect())
}

pub(crate) fn split(grantee: Grantee) -> (Option<i64>, Option<i64>, Option<i64>) {
    match grantee {
        Grantee::State(state) => (Some(state.0), None, None),
        Grantee::Group(group) => (None, Some(group.0), None),
        Grantee::Account(account) => (None, None, Some(account.0)),
    }
}

/// Rebuilds a grantee from its `(state_id, group_id, account_id)` columns.
pub(crate) fn grantee_from(
    state: Option<i64>,
    group: Option<i64>,
    account: Option<i64>,
) -> Option<Grantee> {
    match (state, group, account) {
        (Some(state), None, None) => Some(Grantee::State(StateId(state))),
        (None, Some(group), None) => Some(Grantee::Group(GroupId(group))),
        (None, None, Some(account)) => Some(Grantee::Account(AccountId(account))),
        _ => None,
    }
}

fn to_grant(
    id: i64,
    permission: String,
    state: Option<i64>,
    group: Option<i64>,
    account: Option<i64>,
) -> Option<Grant> {
    let grantee = grantee_from(state, group, account)?;
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
    use tether_core::permissions::{ADMIN_AUDIT, ADMIN_GROUPS, REQUEST_GROUPS};
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
        // Member also has request_groups by default (AA's docs).
        assert_eq!(before, vec![ADMIN_AUDIT, REQUEST_GROUPS]);

        groups::add_member(&pool, officers, pilot).await.unwrap();
        let with_group: Vec<_> = effective(&pool, pilot).await.unwrap().into_iter().collect();
        assert_eq!(with_group, vec![ADMIN_AUDIT, ADMIN_GROUPS, REQUEST_GROUPS]);

        revoke(&pool, group_grant).await.unwrap();
        let after: Vec<_> = effective(&pool, pilot).await.unwrap().into_iter().collect();
        assert_eq!(after, vec![ADMIN_AUDIT, REQUEST_GROUPS]);
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn grants_to_a_user_count_for_that_user_only(pool: PgPool) {
        account(&pool, 1).await; // owner
        let pilot = account(&pool, 2).await;
        let other = account(&pool, 3).await;
        let id = grant(&pool, ADMIN_AUDIT, Grantee::Account(pilot))
            .await
            .unwrap()
            .unwrap();
        assert!(effective(&pool, pilot).await.unwrap().contains(ADMIN_AUDIT));
        assert!(!effective(&pool, other).await.unwrap().contains(ADMIN_AUDIT));
        assert_eq!(
            of_account(&pool, pilot).await.unwrap(),
            vec![(id, ADMIN_AUDIT.to_owned())]
        );
        // Main or not, as AA's user permissions.
        sqlx::query("UPDATE core.accounts SET main_character_id = NULL WHERE id = $1")
            .bind(pilot.0)
            .execute(&pool)
            .await
            .unwrap();
        assert!(effective(&pool, pilot).await.unwrap().contains(ADMIN_AUDIT));
        // Deactivated accounts hold nothing.
        sqlx::query("UPDATE core.accounts SET active = false WHERE id = $1")
            .bind(pilot.0)
            .execute(&pool)
            .await
            .unwrap();
        assert!(effective(&pool, pilot).await.unwrap().is_empty());
        assert_eq!(
            revoke(&pool, id).await.unwrap().map(|g| g.grantee),
            Some(Grantee::Account(pilot))
        );
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
        // Beside the default: request_groups (Member).
        assert_eq!(list(&pool).await.unwrap().len(), 2);
    }
}
