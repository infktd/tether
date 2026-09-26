//! Groups, Alliance Auth style: flags, allowed states, members, join and
//! leave requests, Group Leaders, the per-group Audit Log, reserved names
//! and compliance groups.

use chrono::{DateTime, Utc};
use tether_core::groups::Flags;
use tether_core::states::StateId;

use crate::PgPool;
use crate::accounts::AccountId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct GroupId(pub i64);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    pub id: GroupId,
    pub name: String,
    pub description: String,
    pub flags: Flags,
    /// A Member Audit style compliance group: Tether keeps its members
    /// (the compliant accounts of its allowed states); nobody edits them.
    pub compliance: bool,
}

fn flags(internal: bool, hidden: bool, open: bool, public: bool, restricted: bool) -> Flags {
    Flags {
        internal,
        hidden,
        open,
        public,
        restricted,
    }
}

pub async fn create<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    name: &str,
    description: &str,
    flags: Flags,
) -> Result<GroupId, sqlx::Error> {
    let id = sqlx::query_scalar!(
        r#"
        INSERT INTO core.groups (name, description, internal, hidden, open, public, restricted)
        VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING id
        "#,
        name,
        description,
        flags.internal,
        flags.hidden,
        flags.open,
        flags.public,
        flags.restricted,
    )
    .fetch_one(executor)
    .await?;
    Ok(GroupId(id))
}

/// Changes a group's description, flags and compliance designation.
pub async fn update<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    group: GroupId,
    description: &str,
    flags: Flags,
    compliance: bool,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query!(
        r#"
        UPDATE core.groups
        SET description = $2, internal = $3, hidden = $4, open = $5, public = $6,
            restricted = $7, compliance = $8
        WHERE id = $1
        "#,
        group.0,
        description,
        flags.internal,
        flags.hidden,
        flags.open,
        flags.public,
        flags.restricted,
        compliance,
    )
    .execute(executor)
    .await?;
    Ok(result.rows_affected() == 1)
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

/// Takes the account out of every group (and its pending requests), for
/// deactivation. Returns the groups it was in.
pub async fn leave_all(
    tx: &mut sqlx::PgConnection,
    account: AccountId,
) -> Result<Vec<GroupId>, sqlx::Error> {
    sqlx::query!(
        "DELETE FROM core.group_requests WHERE account_id = $1",
        account.0
    )
    .execute(&mut *tx)
    .await?;
    let groups = sqlx::query_scalar!(
        "DELETE FROM core.group_members WHERE account_id = $1 RETURNING group_id",
        account.0
    )
    .fetch_all(&mut *tx)
    .await?;
    Ok(groups.into_iter().map(GroupId).collect())
}

/// Whether Tether keeps the group's members itself (a compliance group):
/// nobody adds, removes, joins, leaves or deletes it by hand.
pub async fn is_managed<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    group: GroupId,
) -> Result<bool, sqlx::Error> {
    let managed = sqlx::query_scalar!("SELECT compliance FROM core.groups WHERE id = $1", group.0)
        .fetch_optional(executor)
        .await?;
    Ok(managed.unwrap_or(false))
}

/// Locks the group row until the transaction ends: `exclusive` for
/// changes to its settings or grants, shared for changes that depend on
/// them (joining, accepting, granting), so a check can't pass against
/// settings a concurrent change is replacing. False if there's no group.
pub async fn lock(
    tx: &mut sqlx::PgConnection,
    group: GroupId,
    exclusive: bool,
) -> Result<bool, sqlx::Error> {
    let found = if exclusive {
        sqlx::query_scalar!(
            "SELECT id FROM core.groups WHERE id = $1 FOR UPDATE",
            group.0
        )
        .fetch_optional(&mut *tx)
        .await?
    } else {
        sqlx::query_scalar!(
            "SELECT id FROM core.groups WHERE id = $1 FOR SHARE",
            group.0
        )
        .fetch_optional(&mut *tx)
        .await?
    };
    Ok(found.is_some())
}

pub async fn get<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    group: GroupId,
) -> Result<Option<Group>, sqlx::Error> {
    let row = sqlx::query!(
        r#"
        SELECT id, name, description, internal, hidden, open, public, restricted, compliance
        FROM core.groups WHERE id = $1
        "#,
        group.0
    )
    .fetch_optional(executor)
    .await?;
    Ok(row.map(|r| Group {
        id: GroupId(r.id),
        name: r.name,
        description: r.description,
        flags: flags(r.internal, r.hidden, r.open, r.public, r.restricted),
        compliance: r.compliance,
    }))
}

/// Whether a group has this name, ignoring case (for reserving it).
pub async fn name_taken<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    name: &str,
) -> Result<bool, sqlx::Error> {
    let found = sqlx::query_scalar!(
        r#"SELECT true AS "found!" FROM core.groups WHERE lower(name) = lower(trim($1))"#,
        name
    )
    .fetch_optional(executor)
    .await?;
    Ok(found.is_some())
}

/// Every group with its allowed states, by name.
pub async fn all(pool: &PgPool) -> Result<Vec<(Group, Vec<StateId>)>, sqlx::Error> {
    let rows = sqlx::query!(
        r#"
        SELECT g.id, g.name, g.description, g.internal, g.hidden, g.open, g.public,
               g.restricted, g.compliance,
               COALESCE((SELECT array_agg(s.state_id) FROM core.group_states s
                         WHERE s.group_id = g.id), '{}') AS "states!"
        FROM core.groups g
        ORDER BY g.name
        "#
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            (
                Group {
                    id: GroupId(r.id),
                    name: r.name,
                    description: r.description,
                    flags: flags(r.internal, r.hidden, r.open, r.public, r.restricted),
                    compliance: r.compliance,
                },
                r.states.into_iter().map(StateId).collect(),
            )
        })
        .collect())
}

pub async fn allowed_states<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    group: GroupId,
) -> Result<Vec<StateId>, sqlx::Error> {
    let ids = sqlx::query_scalar!(
        "SELECT state_id FROM core.group_states WHERE group_id = $1 ORDER BY state_id",
        group.0
    )
    .fetch_all(executor)
    .await?;
    Ok(ids.into_iter().map(StateId).collect())
}

pub async fn set_allowed_states(
    tx: &mut sqlx::PgConnection,
    group: GroupId,
    states: &[StateId],
) -> Result<(), sqlx::Error> {
    sqlx::query!("DELETE FROM core.group_states WHERE group_id = $1", group.0)
        .execute(&mut *tx)
        .await?;
    let ids: Vec<i64> = states.iter().map(|s| s.0).collect();
    sqlx::query!(
        r#"
        INSERT INTO core.group_states (group_id, state_id)
        SELECT $1, unnest($2::bigint[]) ON CONFLICT DO NOTHING
        "#,
        group.0,
        &ids,
    )
    .execute(&mut *tx)
    .await?;
    Ok(())
}

/// Members whose state the group doesn't allow (after a change to its
/// allowed states).
pub async fn members_not_allowed<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    group: GroupId,
) -> Result<Vec<AccountId>, sqlx::Error> {
    let ids = sqlx::query_scalar!(
        r#"
        SELECT m.account_id FROM core.group_members m
        JOIN core.accounts a ON a.id = m.account_id
        WHERE m.group_id = $1
          AND EXISTS (SELECT 1 FROM core.group_states s WHERE s.group_id = $1)
          AND NOT EXISTS (
            SELECT 1 FROM core.group_states s WHERE s.group_id = $1 AND s.state_id = a.state_id
          )
        "#,
        group.0
    )
    .fetch_all(executor)
    .await?;
    Ok(ids.into_iter().map(AccountId).collect())
}

/// The account's groups that don't allow `state` (AA removes it from them
/// on a state change). Compliance groups are left to their own rule.
pub async fn groups_not_allowing<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    account: AccountId,
    state: StateId,
) -> Result<Vec<GroupId>, sqlx::Error> {
    let ids = sqlx::query_scalar!(
        r#"
        SELECT m.group_id FROM core.group_members m
        JOIN core.groups g ON g.id = m.group_id
        WHERE m.account_id = $1 AND NOT g.compliance
          AND EXISTS (SELECT 1 FROM core.group_states s WHERE s.group_id = m.group_id)
          AND NOT EXISTS (
            SELECT 1 FROM core.group_states s WHERE s.group_id = m.group_id AND s.state_id = $2
          )
        "#,
        account.0,
        state.0,
    )
    .fetch_all(executor)
    .await?;
    Ok(ids.into_iter().map(GroupId).collect())
}

/// Compliance groups and their allowed states.
pub async fn compliance_groups<'e>(
    executor: impl sqlx::PgExecutor<'e>,
) -> Result<Vec<(GroupId, Vec<StateId>)>, sqlx::Error> {
    let rows = sqlx::query!(
        r#"
        SELECT g.id,
               COALESCE((SELECT array_agg(s.state_id) FROM core.group_states s
                         WHERE s.group_id = g.id), '{}') AS "states!"
        FROM core.groups g WHERE g.compliance
        "#
    )
    .fetch_all(executor)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| (GroupId(r.id), r.states.into_iter().map(StateId).collect()))
        .collect())
}

pub async fn is_member<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    group: GroupId,
    account: AccountId,
) -> Result<bool, sqlx::Error> {
    let found = sqlx::query_scalar!(
        r#"SELECT true AS "found!" FROM core.group_members WHERE group_id = $1 AND account_id = $2"#,
        group.0,
        account.0
    )
    .fetch_optional(executor)
    .await?;
    Ok(found.is_some())
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

/// Removes a member and clears any pending request. False if not a
/// member.
pub async fn remove_member<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    group: GroupId,
    account: AccountId,
) -> Result<bool, sqlx::Error> {
    let removed = sqlx::query_scalar!(
        r#"
        WITH cleared AS (
            DELETE FROM core.group_requests WHERE group_id = $1 AND account_id = $2
        )
        DELETE FROM core.group_members WHERE group_id = $1 AND account_id = $2
        RETURNING true AS "removed!"
        "#,
        group.0,
        account.0,
    )
    .fetch_optional(executor)
    .await?;
    Ok(removed.is_some())
}

/// Returns false if a request (join or leave) is already pending.
pub async fn add_request<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    group: GroupId,
    account: AccountId,
    leave: bool,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query!(
        r#"
        INSERT INTO core.group_requests (group_id, account_id, leave) VALUES ($1, $2, $3)
        ON CONFLICT DO NOTHING
        "#,
        group.0,
        account.0,
        leave,
    )
    .execute(executor)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// The pending request, if any: `Some(true)` to leave, `Some(false)` to
/// join.
pub async fn pending<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    group: GroupId,
    account: AccountId,
) -> Result<Option<bool>, sqlx::Error> {
    sqlx::query_scalar!(
        "SELECT leave FROM core.group_requests WHERE group_id = $1 AND account_id = $2",
        group.0,
        account.0
    )
    .fetch_optional(executor)
    .await
}

/// Removes a pending request, returning whether it was to leave.
pub async fn remove_request<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    group: GroupId,
    account: AccountId,
) -> Result<Option<bool>, sqlx::Error> {
    sqlx::query_scalar!(
        "DELETE FROM core.group_requests WHERE group_id = $1 AND account_id = $2 RETURNING leave",
        group.0,
        account.0,
    )
    .fetch_optional(executor)
    .await
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingRequest {
    pub group_id: i64,
    pub group_name: String,
    pub account_id: i64,
    pub main_id: i64,
    pub main_name: String,
    pub leave: bool,
    pub requested_at: DateTime<Utc>,
}

/// Pending requests for these groups, oldest first.
pub async fn requests_for(
    pool: &PgPool,
    groups: &[GroupId],
) -> Result<Vec<PendingRequest>, sqlx::Error> {
    let ids: Vec<i64> = groups.iter().map(|g| g.0).collect();
    sqlx::query_as!(
        PendingRequest,
        r#"
        SELECT r.group_id, g.name AS group_name, r.account_id,
               COALESCE(c.id, 0) AS "main_id!", COALESCE(c.name, '(no main)') AS "main_name!",
               r.leave, r.requested_at
        FROM core.group_requests r
        JOIN core.groups g ON g.id = r.group_id
        JOIN core.accounts a ON a.id = r.account_id
        LEFT JOIN core.characters c ON c.id = a.main_character_id
        WHERE r.group_id = ANY($1)
        ORDER BY r.requested_at
        "#,
        &ids,
    )
    .fetch_all(pool)
    .await
}

/// How many requests are pending for these groups.
pub async fn pending_count(pool: &PgPool, groups: &[GroupId]) -> Result<i64, sqlx::Error> {
    let ids: Vec<i64> = groups.iter().map(|g| g.0).collect();
    sqlx::query_scalar!(
        r#"SELECT count(*) AS "count!" FROM core.group_requests WHERE group_id = ANY($1)"#,
        &ids
    )
    .fetch_one(pool)
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

/// The groups the account is in, and its pending requests (`true`:
/// to leave).
pub async fn memberships(
    pool: &PgPool,
    account: AccountId,
) -> Result<(Vec<GroupId>, Vec<(GroupId, bool)>), sqlx::Error> {
    let member = sqlx::query_scalar!(
        "SELECT group_id FROM core.group_members WHERE account_id = $1",
        account.0
    )
    .fetch_all(pool)
    .await?;
    let pending = sqlx::query!(
        "SELECT group_id, leave FROM core.group_requests WHERE account_id = $1",
        account.0
    )
    .fetch_all(pool)
    .await?;
    Ok((
        member.into_iter().map(GroupId).collect(),
        pending
            .into_iter()
            .map(|r| (GroupId(r.group_id), r.leave))
            .collect(),
    ))
}

/// A group with its member and pending-request counts, for admins.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupSummary {
    pub group: Group,
    pub members: i64,
    pub pending: i64,
}

pub async fn summaries(pool: &PgPool) -> Result<Vec<GroupSummary>, sqlx::Error> {
    let rows = sqlx::query!(
        r#"
        SELECT g.id, g.name, g.description, g.internal, g.hidden, g.open, g.public,
               g.restricted, g.compliance,
               (SELECT count(*) FROM core.group_members m WHERE m.group_id = g.id) AS "members!",
               (SELECT count(*) FROM core.group_requests r WHERE r.group_id = g.id) AS "pending!"
        FROM core.groups g
        ORDER BY g.name
        "#
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| GroupSummary {
            group: Group {
                id: GroupId(r.id),
                name: r.name,
                description: r.description,
                flags: flags(r.internal, r.hidden, r.open, r.public, r.restricted),
                compliance: r.compliance,
            },
            members: r.members,
            pending: r.pending,
        })
        .collect())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    pub account_id: i64,
    pub main_id: i64,
    pub main_name: String,
    /// The account's state's name.
    pub state: String,
    /// `member`, `blue`, `guest` or `custom`, for the badge.
    pub state_style: String,
}

pub async fn members(pool: &PgPool, group: GroupId) -> Result<Vec<Member>, sqlx::Error> {
    sqlx::query_as!(
        Member,
        r#"
        SELECT a.id AS account_id, COALESCE(c.id, 0) AS "main_id!",
               COALESCE(c.name, '(no main)') AS "main_name!",
               s.name AS state, COALESCE(s.builtin, 'custom') AS "state_style!"
        FROM core.group_members m
        JOIN core.accounts a ON a.id = m.account_id
        JOIN core.states s ON s.id = a.state_id
        LEFT JOIN core.characters c ON c.id = a.main_character_id
        WHERE m.group_id = $1
        ORDER BY c.name NULLS LAST
        "#,
        group.0,
    )
    .fetch_all(pool)
    .await
}

// ---- Group Leaders ---------------------------------------------------------

/// A group's leaders, directly listed: `(account, main id, main name)`.
pub async fn leaders(
    pool: &PgPool,
    group: GroupId,
) -> Result<Vec<(AccountId, i64, String)>, sqlx::Error> {
    let rows = sqlx::query!(
        r#"
        SELECT l.account_id, COALESCE(c.id, 0) AS "main_id!",
               COALESCE(c.name, '(no main)') AS "name!"
        FROM core.group_leaders l
        JOIN core.accounts a ON a.id = l.account_id
        LEFT JOIN core.characters c ON c.id = a.main_character_id
        WHERE l.group_id = $1
        ORDER BY 3
        "#,
        group.0
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| (AccountId(r.account_id), r.main_id, r.name))
        .collect())
}

pub async fn set_leader<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    group: GroupId,
    account: AccountId,
    leader: bool,
) -> Result<bool, sqlx::Error> {
    let result = if leader {
        sqlx::query!(
            "INSERT INTO core.group_leaders (group_id, account_id) VALUES ($1, $2) ON CONFLICT DO NOTHING",
            group.0,
            account.0
        )
        .execute(executor)
        .await?
    } else {
        sqlx::query!(
            "DELETE FROM core.group_leaders WHERE group_id = $1 AND account_id = $2",
            group.0,
            account.0
        )
        .execute(executor)
        .await?
    };
    Ok(result.rows_affected() == 1)
}

/// A group's Group Leader Groups.
pub async fn leader_groups<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    group: GroupId,
) -> Result<Vec<GroupId>, sqlx::Error> {
    let ids = sqlx::query_scalar!(
        "SELECT leader_group_id FROM core.group_leader_groups WHERE group_id = $1",
        group.0
    )
    .fetch_all(executor)
    .await?;
    Ok(ids.into_iter().map(GroupId).collect())
}

pub async fn set_leader_group<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    group: GroupId,
    leader_group: GroupId,
    on: bool,
) -> Result<bool, sqlx::Error> {
    let result = if on {
        sqlx::query!(
            "INSERT INTO core.group_leader_groups (group_id, leader_group_id) VALUES ($1, $2) ON CONFLICT DO NOTHING",
            group.0,
            leader_group.0
        )
        .execute(executor)
        .await?
    } else {
        sqlx::query!(
            "DELETE FROM core.group_leader_groups WHERE group_id = $1 AND leader_group_id = $2",
            group.0,
            leader_group.0
        )
        .execute(executor)
        .await?
    };
    Ok(result.rows_affected() == 1)
}

/// The non-Internal groups the account leads: listed as a leader, or a
/// member of one of the group's leader groups. Only while the account is
/// active, has a main and isn't Guest: a leader who left the alliance
/// loses sight of the group's members at once (stricter than AA).
pub async fn led_by<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    account: AccountId,
) -> Result<Vec<GroupId>, sqlx::Error> {
    let ids = sqlx::query_scalar!(
        r#"
        SELECT g.id FROM core.groups g
        WHERE NOT g.internal
          AND EXISTS (
            SELECT 1 FROM core.accounts a
            WHERE a.id = $1 AND a.active AND a.main_character_id IS NOT NULL
              AND a.state_id <> core.guest_state()
          AND NOT core.blacklisted(a.id)
              AND NOT core.blacklisted(a.id)
          )
          AND (
            EXISTS (SELECT 1 FROM core.group_leaders l WHERE l.group_id = g.id AND l.account_id = $1)
            OR EXISTS (
                SELECT 1 FROM core.group_leader_groups lg
                JOIN core.group_members m ON m.group_id = lg.leader_group_id
                WHERE lg.group_id = g.id AND m.account_id = $1
            )
        )
        ORDER BY g.name
        "#,
        account.0
    )
    .fetch_all(executor)
    .await?;
    Ok(ids.into_iter().map(GroupId).collect())
}

/// The groups this group leads (it's one of their leader groups).
pub async fn leads<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    leader_group: GroupId,
) -> Result<Vec<GroupId>, sqlx::Error> {
    let ids = sqlx::query_scalar!(
        "SELECT group_id FROM core.group_leader_groups WHERE leader_group_id = $1",
        leader_group.0
    )
    .fetch_all(executor)
    .await?;
    Ok(ids.into_iter().map(GroupId).collect())
}

/// Every non-Internal group (what `group_management` covers).
pub async fn not_internal<'e>(
    executor: impl sqlx::PgExecutor<'e>,
) -> Result<Vec<GroupId>, sqlx::Error> {
    let ids = sqlx::query_scalar!("SELECT id FROM core.groups WHERE NOT internal ORDER BY name")
        .fetch_all(executor)
        .await?;
    Ok(ids.into_iter().map(GroupId).collect())
}

/// Accounts that lead the group (directly or through a leader group), for
/// request notifications; the same accounts [`led_by`] counts.
pub async fn leader_accounts<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    group: GroupId,
) -> Result<Vec<AccountId>, sqlx::Error> {
    let ids = sqlx::query_scalar!(
        r#"
        SELECT a.id FROM core.accounts a
        WHERE a.active AND a.main_character_id IS NOT NULL
          AND a.state_id <> core.guest_state()
          AND (
            a.id IN (SELECT account_id FROM core.group_leaders WHERE group_id = $1)
            OR a.id IN (
                SELECT m.account_id FROM core.group_leader_groups lg
                JOIN core.group_members m ON m.group_id = lg.leader_group_id
                WHERE lg.group_id = $1
            )
          )
        "#,
        group.0
    )
    .fetch_all(executor)
    .await?;
    Ok(ids.into_iter().map(AccountId).collect())
}

// ---- Audit Log (AA's RequestLog) -------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestType {
    Join,
    Leave,
    Removed,
}

impl RequestType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Join => "join",
            Self::Leave => "leave",
            Self::Removed => "removed",
        }
    }
}

/// Records a request decision or removal in the group's Audit Log, with
/// name snapshots taken now.
pub async fn log<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    group: GroupId,
    kind: RequestType,
    accepted: bool,
    requestor: AccountId,
    actor: Option<AccountId>,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        r#"
        INSERT INTO core.group_request_log
            (group_id, group_name, request_type, action, requestor_account_id, requestor_main,
             requestor_corporation_id, actor_account_id, actor_name)
        SELECT g.id, g.name, $2, $3, $4,
               (SELECT c.name FROM core.accounts a
                JOIN core.characters c ON c.id = a.main_character_id WHERE a.id = $4),
               (SELECT c.corporation_id FROM core.accounts a
                JOIN core.characters c ON c.id = a.main_character_id WHERE a.id = $4),
               $5,
               (SELECT c.name FROM core.accounts a
                JOIN core.characters c ON c.id = a.main_character_id WHERE a.id = $5)
        FROM core.groups g WHERE g.id = $1
        "#,
        group.0,
        kind.as_str(),
        if accepted { "accept" } else { "reject" },
        requestor.0,
        actor.map(|a| a.0),
    )
    .execute(executor)
    .await?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogEntry {
    pub at: DateTime<Utc>,
    pub request_type: String,
    pub action: String,
    pub requestor_main: Option<String>,
    pub requestor_corporation: Option<String>,
    pub actor_name: Option<String>,
}

/// A group's Audit Log, newest first.
pub async fn audit_log(
    pool: &PgPool,
    group: GroupId,
    limit: i64,
) -> Result<Vec<LogEntry>, sqlx::Error> {
    sqlx::query_as!(
        LogEntry,
        r#"
        SELECT l.at, l.request_type, l.action, l.requestor_main,
               n.name AS "requestor_corporation?", l.actor_name
        FROM core.group_request_log l
        LEFT JOIN core.entity_names n ON n.id = l.requestor_corporation_id
        WHERE l.group_id = $1
        ORDER BY l.id DESC
        LIMIT $2
        "#,
        group.0,
        limit
    )
    .fetch_all(pool)
    .await
}

// ---- Reserved group names --------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reserved {
    pub name: String,
    pub reason: String,
    pub created_at: DateTime<Utc>,
}

pub async fn reserved(pool: &PgPool) -> Result<Vec<Reserved>, sqlx::Error> {
    sqlx::query_as!(
        Reserved,
        "SELECT name, reason, created_at FROM core.reserved_group_names ORDER BY name"
    )
    .fetch_all(pool)
    .await
}

pub async fn is_reserved<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    name: &str,
) -> Result<bool, sqlx::Error> {
    let found = sqlx::query_scalar!(
        r#"SELECT true AS "found!" FROM core.reserved_group_names WHERE name = lower(trim($1))"#,
        name
    )
    .fetch_optional(executor)
    .await?;
    Ok(found.is_some())
}

/// False if already reserved.
pub async fn reserve<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    name: &str,
    reason: &str,
    by: Option<AccountId>,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query!(
        r#"
        INSERT INTO core.reserved_group_names (name, reason, created_by)
        VALUES (lower(trim($1)), $2, $3) ON CONFLICT DO NOTHING
        "#,
        name,
        reason,
        by.map(|b| b.0),
    )
    .execute(executor)
    .await?;
    Ok(result.rows_affected() == 1)
}

pub async fn unreserve<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    name: &str,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query!(
        "DELETE FROM core.reserved_group_names WHERE name = lower(trim($1))",
        name
    )
    .execute(executor)
    .await?;
    Ok(result.rows_affected() == 1)
}
