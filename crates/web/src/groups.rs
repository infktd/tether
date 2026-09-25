//! Groups, Alliance Auth style (F5, F23): joining and leaving (the users'
//! Groups page), Group Management (requests, members, the Audit Log, for
//! `group_management` holders and Group Leaders) and the admin settings
//! (callers check `admin.groups`). The rules are in `docs/AA_PARITY.md`
//! and `tether_core::groups`; every change is audited in the same
//! transaction.

use std::collections::BTreeSet;

use axum::http::StatusCode;
use serde_json::json;
use tether_core::groups::{self as rules, Flags, Join, Leave};
use tether_core::permissions::{GROUP_MANAGEMENT, REQUEST_GROUPS};
use tether_core::states::StateId;
use tether_db::PgPool;
use tether_db::accounts::{self, AccountId, Standing};
use tether_db::audit::{self, Actor};
use tether_db::groups::{self, Group, GroupId, RequestType};
use tether_db::permissions;
use tether_db::settings;

use crate::error::{AppError, is_foreign_key_violation, is_unique_violation};

pub const MAX_NAME: usize = 100;

fn target(group: GroupId) -> String {
    format!("group:{}", group.0)
}

async fn load(tx: &mut sqlx::PgConnection, group: GroupId) -> Result<Group, AppError> {
    groups::get(&mut *tx, group)
        .await?
        .ok_or_else(|| AppError::not_found("No such group."))
}

/// [`load`], holding the group's row lock (see `groups::lock`).
async fn load_locked(
    tx: &mut sqlx::PgConnection,
    group: GroupId,
    exclusive: bool,
) -> Result<Group, AppError> {
    if !groups::lock(&mut *tx, group, exclusive).await? {
        return Err(AppError::not_found("No such group."));
    }
    load(tx, group).await
}

async fn standing(tx: &mut sqlx::PgConnection, account: AccountId) -> Result<Standing, AppError> {
    accounts::standing(&mut *tx, account)
        .await?
        .ok_or_else(|| AppError::not_found("No such account."))
}

/// Refusal for changing the members of a compliance group by hand.
pub fn managed_group() -> AppError {
    AppError::bad_request(
        "This is a compliance group: Tether keeps its members (everyone compliant in its allowed \
         states). Grant it permissions or Discord roles instead.",
    )
}

fn owner_only() -> AppError {
    AppError::new(
        StatusCode::FORBIDDEN,
        "This group is Restricted: only the owner changes its members or that setting.",
    )
}

/// Adding someone to a group hands them its permissions, and leadership
/// of the groups it leads, so whoever does it (an admin, a leader
/// accepting a request, an admin appointing leaders, opening it or making
/// it a compliance group) must already hold all of those permissions:
/// group rights mustn't be a path to more.
async fn require_grants(
    tx: &mut sqlx::PgConnection,
    actor: AccountId,
    group: GroupId,
    doing: &str,
) -> Result<(), AppError> {
    let mine = permissions::effective_in(&mut *tx, actor).await?;
    let mut grants = permissions::of_group(&mut *tx, group).await?;
    for led in groups::leads(&mut *tx, group).await? {
        grants.extend(permissions::of_group(&mut *tx, led).await?);
    }
    if let Some(missing) = grants.iter().find(|p| !mine.contains(*p)) {
        return Err(AppError::new(
            StatusCode::FORBIDDEN,
            format!("This group grants {missing}, which you don't have, so you can't {doing}."),
        ));
    }
    Ok(())
}

// ---- the users' side -------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Joined {
    /// An Open group: in at once.
    Added,
    /// A join request for the group's leaders.
    Requested,
}

/// Joins a group, or asks to, in AA's order.
pub async fn join(db: &PgPool, account: AccountId, group: GroupId) -> Result<Joined, AppError> {
    let mut tx = db.begin().await?;
    let group = load_locked(&mut tx, group, false).await?;
    let me = standing(&mut tx, account).await?;
    if !me.has_main {
        return Err(AppError::new(
            StatusCode::FORBIDDEN,
            "Choose a main character before joining groups.",
        ));
    }
    let allowed = groups::allowed_states(&mut *tx, group.id).await?;
    let can_request = permissions::effective_in(&mut tx, account)
        .await?
        .contains(REQUEST_GROUPS);
    let is_member = groups::is_member(&mut *tx, group.id, account).await?;
    let pending = groups::pending(&mut *tx, group.id, account)
        .await?
        .is_some();
    // Only the owner lets anyone into a Restricted group: even Open, joining
    // it is a request (accepted by the owner).
    let flags = Flags {
        open: group.flags.open && !group.flags.restricted,
        ..group.flags
    };
    let joined = match rules::join(flags, &allowed, me.state, is_member, can_request, pending) {
        // Internal groups don't exist as far as users can tell.
        Join::NotJoinable if group.flags.internal => {
            return Err(AppError::not_found("No such group."));
        }
        Join::NotJoinable => {
            return Err(AppError::new(
                StatusCode::FORBIDDEN,
                "Your state can't join this group.",
            ));
        }
        Join::AlreadyMember => {
            return Err(AppError::new(
                StatusCode::CONFLICT,
                "You're already in this group.",
            ));
        }
        Join::NotAllowed => {
            return Err(AppError::new(
                StatusCode::FORBIDDEN,
                "You can't request this group: it's only for pilots who may request groups.",
            ));
        }
        Join::Pending => {
            return Err(AppError::new(
                StatusCode::CONFLICT,
                "You already have a request waiting for this group.",
            ));
        }
        Join::Added => {
            groups::add_member(&mut *tx, group.id, account).await?;
            groups::log(
                &mut *tx,
                group.id,
                RequestType::Join,
                true,
                account,
                Some(account),
            )
            .await?;
            audit::record(
                &mut *tx,
                Actor::Account(account),
                "group.join",
                Some(&target(group.id)),
                json!({}),
            )
            .await?;
            Joined::Added
        }
        Join::Requested => {
            groups::add_request(&mut *tx, group.id, account, false).await?;
            crate::notifications::group_request(&mut tx, group.id, &group.name, account, false)
                .await?;
            audit::record(
                &mut *tx,
                Actor::Account(account),
                "group.request",
                Some(&target(group.id)),
                json!({ "leave": false }),
            )
            .await?;
            Joined::Requested
        }
    };
    tx.commit().await?;
    Ok(joined)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Left {
    /// An Open group, or auto-leave is on: out at once.
    Removed,
    /// A leave request for the group's leaders.
    Requested,
}

/// Leaves a group, or asks to, in AA's order.
pub async fn leave(db: &PgPool, account: AccountId, group: GroupId) -> Result<Left, AppError> {
    let mut tx = db.begin().await?;
    let group = load(&mut tx, group).await?;
    let is_member = groups::is_member(&mut *tx, group.id, account).await?;
    let pending = groups::pending(&mut *tx, group.id, account)
        .await?
        .is_some();
    let auto_leave = settings::get_bool(&mut *tx, settings::GROUPS_AUTO_LEAVE).await?;
    let left = match rules::leave(group.flags, is_member, pending, auto_leave) {
        // Internal groups don't exist for those outside them.
        Leave::Internal if !is_member => return Err(AppError::not_found("No such group.")),
        Leave::Internal if group.compliance => return Err(managed_group()),
        Leave::Internal => {
            return Err(AppError::new(
                StatusCode::FORBIDDEN,
                "Only admins change who is in this group.",
            ));
        }
        Leave::NotMember => return Err(AppError::not_found("You're not in this group.")),
        Leave::Pending => {
            return Err(AppError::new(
                StatusCode::CONFLICT,
                "You already have a request waiting for this group.",
            ));
        }
        Leave::Removed => {
            groups::remove_member(&mut *tx, group.id, account).await?;
            groups::log(
                &mut *tx,
                group.id,
                RequestType::Leave,
                true,
                account,
                Some(account),
            )
            .await?;
            audit::record(
                &mut *tx,
                Actor::Account(account),
                "group.leave",
                Some(&target(group.id)),
                json!({}),
            )
            .await?;
            Left::Removed
        }
        Leave::Requested => {
            groups::add_request(&mut *tx, group.id, account, true).await?;
            crate::notifications::group_request(&mut tx, group.id, &group.name, account, true)
                .await?;
            audit::record(
                &mut *tx,
                Actor::Account(account),
                "group.request",
                Some(&target(group.id)),
                json!({ "leave": true }),
            )
            .await?;
            Left::Requested
        }
    };
    tx.commit().await?;
    Ok(left)
}

/// Withdraws a join request (AA retracts join requests only, and doesn't
/// log them in the group's Audit Log).
pub async fn retract(db: &PgPool, account: AccountId, group: GroupId) -> Result<(), AppError> {
    let mut tx = db.begin().await?;
    match groups::pending(&mut *tx, group, account).await? {
        Some(false) => {}
        Some(true) => {
            return Err(AppError::bad_request(
                "A leave request can't be withdrawn; wait for the group's leaders.",
            ));
        }
        None => return Err(AppError::not_found("You have no request for this group.")),
    }
    groups::remove_request(&mut *tx, group, account).await?;
    audit::record(
        &mut *tx,
        Actor::Account(account),
        "group.request.withdraw",
        Some(&target(group)),
        json!({}),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// A group as the users' Groups page shows it.
#[derive(Debug, Clone)]
pub struct Available {
    pub group: Group,
    pub is_member: bool,
    /// A pending request: `Some(true)` to leave, `Some(false)` to join.
    pub pending: Option<bool>,
}

/// The users' Groups page: the groups they're in (Internal ones too, to
/// see, not leave) and the listed groups they may join (AA's Available
/// Groups).
pub async fn available(db: &PgPool, account: AccountId) -> Result<Vec<Available>, AppError> {
    let mut conn = db.acquire().await?;
    let me = standing(&mut conn, account).await?;
    let can_request = permissions::effective_in(&mut conn, account)
        .await?
        .contains(REQUEST_GROUPS);
    drop(conn);
    let (member, pending) = groups::memberships(db, account).await?;
    let mut out = Vec::new();
    for (group, allowed) in groups::all(db).await? {
        let is_member = member.contains(&group.id);
        let request = pending.iter().find(|(g, _)| *g == group.id).map(|p| p.1);
        let listed = me.has_main && rules::listed(group.flags, &allowed, me.state, can_request);
        if is_member || request.is_some() || listed {
            out.push(Available {
                group,
                is_member,
                pending: request,
            });
        }
    }
    Ok(out)
}

/// One group through its direct join link: shown if the account could
/// join it (Hidden groups too), is in it or has asked.
pub async fn direct(
    db: &PgPool,
    account: AccountId,
    group: GroupId,
) -> Result<Available, AppError> {
    let mut conn = db.acquire().await?;
    let found = load(&mut conn, group).await?;
    let me = standing(&mut conn, account).await?;
    let allowed = groups::allowed_states(&mut *conn, group).await?;
    let is_member = groups::is_member(&mut *conn, group, account).await?;
    let pending = groups::pending(&mut *conn, group, account).await?;
    if !is_member && pending.is_none() && !rules::joinable(found.flags, &allowed, me.state) {
        return Err(AppError::not_found("No such group."));
    }
    Ok(Available {
        group: found,
        is_member,
        pending,
    })
}

// ---- Group Management ------------------------------------------------------

/// The groups the account manages: every non-Internal group with
/// `group_management`, else the ones it leads.
pub async fn managed_by(db: &PgPool, account: AccountId) -> Result<Vec<GroupId>, AppError> {
    let mut conn = db.acquire().await?;
    managed_in(&mut conn, account).await
}

async fn managed_in(
    conn: &mut sqlx::PgConnection,
    account: AccountId,
) -> Result<Vec<GroupId>, AppError> {
    if permissions::effective_in(&mut *conn, account)
        .await?
        .contains(GROUP_MANAGEMENT)
    {
        Ok(groups::not_internal(&mut *conn).await?)
    } else {
        Ok(groups::led_by(&mut *conn, account).await?)
    }
}

/// The group, if the account manages it (else not found: leaders don't
/// learn about other groups).
async fn managed(
    tx: &mut sqlx::PgConnection,
    actor: AccountId,
    group: GroupId,
) -> Result<Group, AppError> {
    if !managed_in(&mut *tx, actor).await?.contains(&group) {
        return Err(AppError::not_found("No such group."));
    }
    load_locked(tx, group, false).await
}

/// Whether the account may open Group Management at all.
pub async fn can_manage_any(db: &PgPool, account: AccountId) -> Result<bool, AppError> {
    Ok(!managed_by(db, account).await?.is_empty())
}

/// The group, if the account manages it (for its pages).
pub async fn managed_group_for(
    db: &PgPool,
    actor: AccountId,
    group: GroupId,
) -> Result<Group, AppError> {
    let mut conn = db.acquire().await?;
    managed(&mut conn, actor, group).await
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Accept,
    Reject,
}

/// Accepts or rejects a pending join or leave request. Accepting a join
/// re-checks that the requester's state may join, as AA does.
pub async fn decide(
    db: &PgPool,
    actor: AccountId,
    group: GroupId,
    requester: AccountId,
    decision: Decision,
) -> Result<(), AppError> {
    let mut tx = db.begin().await?;
    let group = managed(&mut tx, actor, group).await?;
    let me = standing(&mut tx, actor).await?;
    if group.flags.restricted && decision == Decision::Accept && !me.is_owner {
        return Err(owner_only());
    }
    let Some(leave) = groups::remove_request(&mut *tx, group.id, requester).await? else {
        return Err(AppError::not_found("No pending request from that account."));
    };
    if decision == Decision::Accept {
        if leave {
            groups::remove_member(&mut *tx, group.id, requester).await?;
        } else {
            let them = standing(&mut tx, requester).await?;
            let allowed = groups::allowed_states(&mut *tx, group.id).await?;
            if !them.active || !them.has_main || !rules::joinable(group.flags, &allowed, them.state)
            {
                return Err(AppError::bad_request(
                    "Their state can't be in this group now, so the request can only be rejected.",
                ));
            }
            require_grants(&mut tx, actor, group.id, "accept members").await?;
            groups::add_member(&mut *tx, group.id, requester).await?;
        }
    }
    let kind = if leave {
        RequestType::Leave
    } else {
        RequestType::Join
    };
    let accepted = decision == Decision::Accept;
    groups::log(&mut *tx, group.id, kind, accepted, requester, Some(actor)).await?;
    crate::notifications::group_decision(&mut tx, requester, &group.name, leave, accepted).await?;
    audit::record(
        &mut *tx,
        Actor::Account(actor),
        if accepted {
            "group.request.accept"
        } else {
            "group.request.reject"
        },
        Some(&target(group.id)),
        json!({ "account_id": requester.0, "leave": leave }),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Removes a member through Group Management: logged as Removed.
pub async fn kick(
    db: &PgPool,
    actor: AccountId,
    group: GroupId,
    member: AccountId,
) -> Result<(), AppError> {
    let mut tx = db.begin().await?;
    let group = managed(&mut tx, actor, group).await?;
    remove_in(&mut tx, actor, &group, member).await?;
    tx.commit().await?;
    Ok(())
}

async fn remove_in(
    tx: &mut sqlx::PgConnection,
    actor: AccountId,
    group: &Group,
    member: AccountId,
) -> Result<(), AppError> {
    if group.compliance {
        return Err(managed_group());
    }
    if group.flags.restricted && !standing(&mut *tx, actor).await?.is_owner {
        return Err(owner_only());
    }
    if !groups::remove_member(&mut *tx, group.id, member).await? {
        return Err(AppError::not_found("That account isn't in this group."));
    }
    if !group.flags.internal {
        groups::log(
            &mut *tx,
            group.id,
            RequestType::Removed,
            true,
            member,
            Some(actor),
        )
        .await?;
    }
    audit::record(
        &mut *tx,
        Actor::Account(actor),
        "group.member.remove",
        Some(&target(group.id)),
        json!({ "account_id": member.0 }),
    )
    .await?;
    Ok(())
}

// ---- admin (callers check admin.groups) -----------------------------------

fn check_name(name: &str) -> Result<&str, AppError> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > MAX_NAME {
        return Err(AppError::bad_request(
            "Group names are 1 to 100 characters.",
        ));
    }
    Ok(name)
}

fn check_description(description: &str) -> Result<&str, AppError> {
    let description = description.trim();
    if description.chars().count() > rules::MAX_DESCRIPTION {
        return Err(AppError::bad_request(
            "Group descriptions are at most 512 characters.",
        ));
    }
    Ok(description)
}

fn flags_json(flags: Flags) -> serde_json::Value {
    json!({
        "internal": flags.internal,
        "hidden": flags.hidden,
        "open": flags.open,
        "public": flags.public,
        "restricted": flags.restricted,
    })
}

/// Creates a group; new groups are Internal and Hidden unless the admin
/// says otherwise (AA's defaults).
pub async fn create(
    db: &PgPool,
    actor: AccountId,
    name: &str,
    description: &str,
    flags: Flags,
) -> Result<GroupId, AppError> {
    let name = check_name(name)?;
    let description = check_description(description)?;
    let mut tx = db.begin().await?;
    if flags.restricted && !standing(&mut tx, actor).await?.is_owner {
        return Err(owner_only());
    }
    if groups::is_reserved(&mut *tx, name).await? {
        return Err(AppError::bad_request(
            "That name is reserved: pick another, or unreserve it first.",
        ));
    }
    let id = match groups::create(&mut *tx, name, description, flags).await {
        Ok(id) => id,
        Err(err) if is_unique_violation(&err) => {
            return Err(AppError::new(
                StatusCode::CONFLICT,
                "A group with that name already exists.",
            ));
        }
        Err(err) => return Err(err.into()),
    };
    audit::record(
        &mut *tx,
        Actor::Account(actor),
        "group.create",
        Some(&target(id)),
        json!({ "name": name, "flags": flags_json(flags) }),
    )
    .await?;
    tx.commit().await?;
    Ok(id)
}

/// A group's settings, as the admin page saves them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub description: String,
    pub flags: Flags,
    pub compliance: bool,
    pub states: Vec<StateId>,
}

/// Saves a group's settings. Members whose state the group no longer
/// allows leave it (AA).
pub async fn update(
    db: &PgPool,
    actor: AccountId,
    group: GroupId,
    new: Settings,
) -> Result<(), AppError> {
    let description = check_description(&new.description)?.to_owned();
    let mut tx = db.begin().await?;
    let old = load_locked(&mut tx, group, true).await?;
    let old_states = groups::allowed_states(&mut *tx, group).await?;
    let states: Vec<StateId> = new
        .states
        .iter()
        .copied()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let owner = standing(&mut tx, actor).await?.is_owner;
    let membership_moves = states != old_states || new.compliance != old.compliance;
    // A Restricted group's settings are the owner's alone, as is the flag.
    if (old.flags.restricted || new.flags.restricted) && !owner {
        return Err(owner_only());
    }
    if new.compliance && !new.flags.internal {
        return Err(AppError::bad_request(
            "A compliance group must be Internal: Tether keeps its members.",
        ));
    }
    // A group that leads others hands out Group Management: never to
    // anyone who can walk in, nor to everyone compliant.
    let opening = new.flags.anyone_can_join() && !old.flags.anyone_can_join();
    if (opening || (new.compliance && !old.compliance))
        && !groups::leads(&mut *tx, group).await?.is_empty()
    {
        return Err(AppError::bad_request(
            "This group leads other groups, so it can't be Open or a compliance group. \
             Remove it as their leader group first.",
        ));
    }
    // Opening a group (or widening who may walk into an Open one) lets
    // people in, so it needs what adding them would.
    if new.flags.anyone_can_join() && (opening || states != old_states) {
        require_grants(&mut tx, actor, group, "open it").await?;
    }
    // Sensitive permissions never go where anyone can walk in.
    if opening {
        let grants = permissions::of_group(&mut *tx, group).await?;
        if let Some(p) = grants
            .iter()
            .find(|p| tether_core::permissions::is_sensitive(p))
        {
            return Err(AppError::bad_request(format!(
                "This group grants {p}, which can't go to an Open group: anyone can join it. \
                 Revoke it first."
            )));
        }
    }
    // Tether fills a compliance group itself, so making one (or widening
    // who it takes) hands its grants out.
    if new.compliance && membership_moves {
        require_grants(&mut tx, actor, group, "make it a compliance group").await?;
    }
    let known = tether_db::states::list(&mut *tx).await?;
    if let Some(bad) = states.iter().find(|s| !known.iter().any(|k| k.id == **s)) {
        return Err(AppError::bad_request(format!("No state {}.", bad.0)));
    }
    groups::update(&mut *tx, group, &description, new.flags, new.compliance).await?;
    groups::set_allowed_states(&mut tx, group, &states).await?;
    let mut removed = Vec::new();
    for account in groups::members_not_allowed(&mut *tx, group).await? {
        groups::remove_member(&mut *tx, group, account).await?;
        audit::record(
            &mut *tx,
            Actor::Account(actor),
            "group.member.remove",
            Some(&target(group)),
            json!({ "account_id": account.0, "reason": "state not allowed" }),
        )
        .await?;
        removed.push(account.0);
    }
    audit::record(
        &mut *tx,
        Actor::Account(actor),
        "group.update",
        Some(&target(group)),
        json!({
            "description": description,
            "flags": flags_json(new.flags),
            "compliance": new.compliance,
            "states": states.iter().map(|s| s.0).collect::<Vec<_>>(),
            "removed": removed,
        }),
    )
    .await?;
    if new.compliance || old.compliance {
        crate::states::enqueue_evaluate_all(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(())
}

pub async fn delete(db: &PgPool, actor: AccountId, group: GroupId) -> Result<(), AppError> {
    let mut tx = db.begin().await?;
    let found = load_locked(&mut tx, group, true).await?;
    if found.compliance {
        return Err(AppError::bad_request(
            "This is a compliance group: untick Compliance group in its settings first.",
        ));
    }
    if found.flags.restricted && !standing(&mut tx, actor).await?.is_owner {
        return Err(owner_only());
    }
    groups::delete(&mut *tx, group).await?;
    audit::record(
        &mut *tx,
        Actor::Account(actor),
        "group.delete",
        Some(&target(group)),
        json!({ "name": found.name }),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Adds a member directly (admins only; Group Management can't).
pub async fn add_member(
    db: &PgPool,
    actor: AccountId,
    group: GroupId,
    account: AccountId,
) -> Result<(), AppError> {
    let mut tx = db.begin().await?;
    let found = load_locked(&mut tx, group, false).await?;
    if found.compliance {
        return Err(managed_group());
    }
    if found.flags.restricted && !standing(&mut tx, actor).await?.is_owner {
        return Err(owner_only());
    }
    let them = standing(&mut tx, account).await?;
    if !them.active {
        return Err(AppError::bad_request(
            "That account is deactivated: reactivate it first.",
        ));
    }
    let allowed = groups::allowed_states(&mut *tx, group).await?;
    if !rules::state_allowed(&allowed, them.state) {
        return Err(AppError::bad_request(
            "That account's state isn't allowed in this group.",
        ));
    }
    require_grants(&mut tx, actor, group, "add members to it").await?;
    let added = match groups::add_member(&mut *tx, group, account).await {
        Ok(added) => added,
        Err(err) if is_foreign_key_violation(&err) => {
            return Err(AppError::not_found("No such account."));
        }
        Err(err) => return Err(err.into()),
    };
    if added {
        audit::record(
            &mut *tx,
            Actor::Account(actor),
            "group.member.add",
            Some(&target(group)),
            json!({ "account_id": account.0 }),
        )
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Removes a member as an admin (any group but a compliance group).
pub async fn remove_member(
    db: &PgPool,
    actor: AccountId,
    group: GroupId,
    account: AccountId,
) -> Result<(), AppError> {
    let mut tx = db.begin().await?;
    let found = load(&mut tx, group).await?;
    remove_in(&mut tx, actor, &found, account).await?;
    tx.commit().await?;
    Ok(())
}

/// Makes an account a Group Leader, or stops it being one.
pub async fn set_leader(
    db: &PgPool,
    actor: AccountId,
    group: GroupId,
    account: AccountId,
    on: bool,
) -> Result<(), AppError> {
    let mut tx = db.begin().await?;
    let found = load_locked(&mut tx, group, false).await?;
    if found.flags.restricted && !standing(&mut tx, actor).await?.is_owner {
        return Err(owner_only());
    }
    if on {
        if !standing(&mut tx, account).await?.active {
            return Err(AppError::bad_request(
                "That account is deactivated: reactivate it first.",
            ));
        }
        require_grants(&mut tx, actor, group, "appoint its leaders").await?;
    }
    if !groups::set_leader(&mut *tx, group, account, on).await? {
        return Err(if on {
            AppError::new(StatusCode::CONFLICT, "They already lead this group.")
        } else {
            AppError::not_found("They don't lead this group.")
        });
    }
    audit::record(
        &mut *tx,
        Actor::Account(actor),
        if on {
            "group.leader.add"
        } else {
            "group.leader.remove"
        },
        Some(&target(group)),
        json!({ "account_id": account.0 }),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Makes another group a Group Leader Group (its members lead this one),
/// or stops it being one.
pub async fn set_leader_group(
    db: &PgPool,
    actor: AccountId,
    group: GroupId,
    leader_group: GroupId,
    on: bool,
) -> Result<(), AppError> {
    if group == leader_group {
        return Err(AppError::bad_request("A group can't lead itself."));
    }
    let mut tx = db.begin().await?;
    let found = load_locked(&mut tx, group, false).await?;
    let leading = load_locked(&mut tx, leader_group, false).await?;
    if found.flags.restricted && !standing(&mut tx, actor).await?.is_owner {
        return Err(owner_only());
    }
    if on {
        // Leading a group is Group Management over it: never for anyone
        // who can walk in, nor for everyone compliant.
        if leading.flags.anyone_can_join() || leading.compliance {
            return Err(AppError::bad_request(
                "An Open group or a compliance group can't lead others: anyone could get in.",
            ));
        }
        require_grants(&mut tx, actor, group, "appoint its leaders").await?;
    }
    if !groups::set_leader_group(&mut *tx, group, leader_group, on).await? {
        return Err(if on {
            AppError::new(StatusCode::CONFLICT, "That group already leads this one.")
        } else {
            AppError::not_found("That group doesn't lead this one.")
        });
    }
    audit::record(
        &mut *tx,
        Actor::Account(actor),
        if on {
            "group.leader_group.add"
        } else {
            "group.leader_group.remove"
        },
        Some(&target(group)),
        json!({ "leader_group_id": leader_group.0 }),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Reserves a group name (ignoring case) with a reason.
pub async fn reserve(
    db: &PgPool,
    actor: AccountId,
    name: &str,
    reason: &str,
) -> Result<(), AppError> {
    let name = check_name(name)?;
    let reason = reason.trim();
    if reason.is_empty() || reason.chars().count() > 200 {
        return Err(AppError::bad_request(
            "Give a reason of at most 200 characters.",
        ));
    }
    let mut tx = db.begin().await?;
    if groups::name_taken(&mut *tx, name).await? {
        return Err(AppError::bad_request(
            "A group already has that name: rename or delete it first.",
        ));
    }
    if !groups::reserve(&mut *tx, name, reason, Some(actor)).await? {
        return Err(AppError::new(
            StatusCode::CONFLICT,
            "That name is already reserved.",
        ));
    }
    audit::record(
        &mut *tx,
        Actor::Account(actor),
        "group.name.reserve",
        None,
        json!({ "name": name.to_lowercase(), "reason": reason }),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

pub async fn unreserve(db: &PgPool, actor: AccountId, name: &str) -> Result<(), AppError> {
    let mut tx = db.begin().await?;
    if !groups::unreserve(&mut *tx, name).await? {
        return Err(AppError::not_found("That name isn't reserved."));
    }
    audit::record(
        &mut *tx,
        Actor::Account(actor),
        "group.name.unreserve",
        None,
        json!({ "name": name.trim().to_lowercase() }),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// The two Group Management settings, as AA's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Options {
    pub auto_leave: bool,
    pub notify_requests: bool,
}

pub async fn options(db: &PgPool) -> Result<Options, AppError> {
    Ok(Options {
        auto_leave: settings::get_bool(db, settings::GROUPS_AUTO_LEAVE).await?,
        notify_requests: settings::get_bool(db, settings::GROUPS_NOTIFY_REQUESTS).await?,
    })
}

pub async fn set_options(db: &PgPool, actor: AccountId, new: Options) -> Result<(), AppError> {
    let mut tx = db.begin().await?;
    settings::set(&mut *tx, settings::GROUPS_AUTO_LEAVE, json!(new.auto_leave)).await?;
    settings::set(
        &mut *tx,
        settings::GROUPS_NOTIFY_REQUESTS,
        json!(new.notify_requests),
    )
    .await?;
    audit::record(
        &mut *tx,
        Actor::Account(actor),
        "group.settings",
        None,
        json!({ "auto_leave": new.auto_leave, "notify_requests": new.notify_requests }),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}
