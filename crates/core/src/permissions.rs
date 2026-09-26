//! Permissions. Core defines the ones below; plugins add their own from
//! their manifests in milestone 2. Grants go to states and groups only.

pub const ADMIN_GROUPS: &str = "admin.groups";
pub const ADMIN_PERMISSIONS: &str = "admin.permissions";
pub const ADMIN_STATES: &str = "admin.states";
pub const ADMIN_AUDIT: &str = "admin.audit";
pub const ADMIN_DISCORD: &str = "admin.discord";
pub const ADMIN_SYSTEM: &str = "admin.system";
pub const ADMIN_PLUGINS: &str = "admin.plugins";
pub const FLEET_PING: &str = "fleet.ping";
pub const COMPLIANCE_VIEW: &str = "compliance.view";
/// AA's Corporation Stats views: the main's own corporation, its
/// alliance's corporations, or the corporations the main's state covers.
/// (`compliance.view` sees every corporation.)
pub const CORPSTATS_CORP: &str = "corpstats.view_corp_corpstats";
pub const CORPSTATS_ALLIANCE: &str = "corpstats.view_alliance_corpstats";
pub const CORPSTATS_STATE: &str = "corpstats.view_state_corpstats";
pub const ADMIN_USERS: &str = "admin.users";
/// AA's `group_management`: process every non-internal group's requests,
/// see and remove its members, read its audit log.
pub const GROUP_MANAGEMENT: &str = "group_management";
/// AA's `request_groups`: see and ask to join groups that aren't Public.
pub const REQUEST_GROUPS: &str = "request_groups";
/// AA's `discord.access_discord`: may link Discord and be in the server.
pub const DISCORD_ACCESS: &str = "discord.access_discord";
/// AA's permissions tool: who holds every permission, and through what.
pub const PERMISSIONS_AUDIT: &str = "permissions_tool.audit_permissions";

/// Every permission that can be granted, with a description for admins.
pub const CORE_PERMISSIONS: &[(&str, &str)] = &[
    (
        ADMIN_GROUPS,
        "Create, change and delete groups (flags, allowed states, leaders), and add members directly",
    ),
    (
        GROUP_MANAGEMENT,
        "Group Management: accept and reject requests, see and remove members, and read the audit log of every group that isn't Internal",
    ),
    (REQUEST_GROUPS, "Can request non-public groups"),
    (DISCORD_ACCESS, "Can access the Discord service"),
    (
        CORPSTATS_CORP,
        "Corporation Stats for your main's corporation: its members, mains and who never registered",
    ),
    (
        CORPSTATS_ALLIANCE,
        "Corporation Stats for every corporation in your main's alliance",
    ),
    (
        CORPSTATS_STATE,
        "Corporation Stats for every corporation your state covers",
    ),
    (
        ADMIN_PERMISSIONS,
        "Grant and revoke permissions (effectively full admin: holders can grant themselves anything)",
    ),
    (
        ADMIN_STATES,
        "Set up access states: who is Member, Blue or in a state you create, and in what order. Holders can only change who is in a state if they hold everything granted to it",
    ),
    (ADMIN_AUDIT, "Read the audit log"),
    (
        PERMISSIONS_AUDIT,
        "Permissions Audit: see who holds every permission, and through which state or group",
    ),
    (
        ADMIN_USERS,
        "Users: find every account and see its characters (alts), groups and permissions; deactivate and reactivate accounts (a deactivated account is Guest and can't sign in)",
    ),
    (
        ADMIN_SYSTEM,
        "See ESI, job queue and update status; retry failed jobs; switch update checks",
    ),
    (
        ADMIN_PLUGINS,
        "Install, enable, disable and uninstall plugins, approve what they may access, and re-pin publisher keys",
    ),
    (
        ADMIN_DISCORD,
        "Set up the Discord bot and choose which roles states and groups get",
    ),
    (
        FLEET_PING,
        "Send fleet pings to Discord, including @everyone",
    ),
    (
        COMPLIANCE_VIEW,
        "See which accounts aren't compliant, what each character is missing (naming their alts), and which corporation members never registered",
    ),
];

pub fn is_known(permission: &str) -> bool {
    CORE_PERMISSIONS.iter().any(|(name, _)| *name == permission)
}

/// Permissions that must never reach people anyone can become (Guest, or
/// a group anyone can join): admin powers, managing groups, and pinging the
/// whole server.
pub fn is_sensitive(permission: &str) -> bool {
    permission.starts_with("admin.")
        || permission == FLEET_PING
        || permission == COMPLIANCE_VIEW
        || permission == GROUP_MANAGEMENT
        // Who holds what maps out the admins.
        || permission == PERMISSIONS_AUDIT
        // Member lists are intel.
        || permission.starts_with("corpstats.")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_permissions() {
        assert!(is_known(ADMIN_GROUPS));
        assert!(!is_known("admin.everything"));
    }
}
