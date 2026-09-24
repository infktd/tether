//! Groups, memberships and join requests.

use tether_core::permissions::JoinPolicy;

use crate::PgPool;
use crate::accounts::AccountId;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GroupId(pub i64);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    pub id: GroupId,
    pub name: String,
    pub description: String,
    pub join_policy: JoinPolicy,
}

/// A group as seen by one account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupView {
    pub group: Group,
    pub is_member: bool,
    pub has_requested: bool,
}

fn policy(value: &str) -> JoinPolicy {
    // The CHECK constraint allows nothing else; fall back to the most
    // restrictive policy rather than panicking.
    JoinPolicy::parse(value).unwrap_or(JoinPolicy::Assigned)
}

pub async fn create<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    name: &str,
    description: &str,
    join_policy: JoinPolicy,
) -> Result<GroupId, sqlx::Error> {
    let id = sqlx::query_scalar!(
        "INSERT INTO core.groups (name, description, join_policy) VALUES ($1, $2, $3) RETURNING id",
        name,
        description,
        join_policy.as_str(),
    )
    .fetch_one(executor)
    .await?;
    Ok(GroupId(id))
}

pub async fn delete<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    group: GroupId,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query!("DELETE FROM core.groups WHERE id = $1", group.0)
        .execute(executor)
        .await?;
    Ok(result.rows_affected() == 1)
}

pub async fn get<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    group: GroupId,
) -> Result<Option<Group>, sqlx::Error> {
    let row = sqlx::query!(
        "SELECT id, name, description, join_policy FROM core.groups WHERE id = $1",
        group.0
    )
    .fetch_optional(executor)
    .await?;
    Ok(row.map(|r| Group {
        id: GroupId(r.id),
        name: r.name,
        description: r.description,
        join_policy: policy(&r.join_policy),
    }))
}

/// All groups, with this account's membership and request status.
pub async fn list_for(pool: &PgPool, account: AccountId) -> Result<Vec<GroupView>, sqlx::Error> {
    let rows = sqlx::query!(
        r#"
        SELECT g.id, g.name, g.description, g.join_policy,
               EXISTS (SELECT 1 FROM core.group_members m
                       WHERE m.group_id = g.id AND m.account_id = $1) AS "is_member!",
               EXISTS (SELECT 1 FROM core.group_requests r
                       WHERE r.group_id = g.id AND r.account_id = $1) AS "has_requested!"
        FROM core.groups g
        ORDER BY g.name
        "#,
        account.0,
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| GroupView {
            group: Group {
                id: GroupId(r.id),
                name: r.name,
                description: r.description,
                join_policy: policy(&r.join_policy),
            },
            is_member: r.is_member,
            has_requested: r.has_requested,
        })
        .collect())
}

/// Adds a member and clears any pending request. Returns false if already a
/// member.
pub async fn add_member<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    group: GroupId,
    account: AccountId,
) -> Result<bool, sqlx::Error> {
    let added = sqlx::query_scalar!(
        r#"
        WITH cleared AS (
            DELETE FROM core.group_requests WHERE group_id = $1 AND account_id = $2
        )
        INSERT INTO core.group_members (group_id, account_id) VALUES ($1, $2)
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

pub async fn remove_member<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    group: GroupId,
    account: AccountId,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query!(
        "DELETE FROM core.group_members WHERE group_id = $1 AND account_id = $2",
        group.0,
        account.0,
    )
    .execute(executor)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// Returns false if a request is already pending.
pub async fn add_request<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    group: GroupId,
    account: AccountId,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query!(
        r#"
        INSERT INTO core.group_requests (group_id, account_id) VALUES ($1, $2)
        ON CONFLICT DO NOTHING
        "#,
        group.0,
        account.0,
    )
    .execute(executor)
    .await?;
    Ok(result.rows_affected() == 1)
}

pub async fn remove_request<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    group: GroupId,
    account: AccountId,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query!(
        "DELETE FROM core.group_requests WHERE group_id = $1 AND account_id = $2",
        group.0,
        account.0,
    )
    .execute(executor)
    .await?;
    Ok(result.rows_affected() == 1)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingRequest {
    pub account_id: i64,
    pub main_name: String,
}

pub async fn requests(pool: &PgPool, group: GroupId) -> Result<Vec<PendingRequest>, sqlx::Error> {
    sqlx::query_as!(
        PendingRequest,
        r#"
        SELECT r.account_id, c.name AS main_name
        FROM core.group_requests r
        JOIN core.accounts a ON a.id = r.account_id
        JOIN core.characters c ON c.id = a.main_character_id
        WHERE r.group_id = $1
        ORDER BY r.requested_at
        "#,
        group.0,
    )
    .fetch_all(pool)
    .await
}

/// Names of the groups the account belongs to.
pub async fn names_for(pool: &PgPool, account: AccountId) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar!(
        r#"
        SELECT g.name FROM core.groups g
        JOIN core.group_members m ON m.group_id = g.id
        WHERE m.account_id = $1
        ORDER BY g.name
        "#,
        account.0,
    )
    .fetch_all(pool)
    .await
}
