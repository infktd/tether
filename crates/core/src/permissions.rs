//! Permissions. Core defines the ones below; plugins add their own from
//! their manifests. Grants go to states, groups and single users (AA's).

pub const ADMIN_GROUPS: &str = "admin.groups";
pub const ADMIN_PERMISSIONS: &str = "admin.permissions";
pub const ADMIN_STATES: &str = "admin.states";
pub const ADMIN_AUDIT: &str = "admin.audit";
pub const ADMIN_DISCORD: &str = "admin.discord";
pub const ADMIN_SYSTEM: &str = "admin.system";
pub const ADMIN_PLUGINS: &str = "admin.plugins";
/// aa-fleetpings' permission: send fleet pings.
pub const FLEETPINGS_ACCESS: &str = "fleetpings.basic_access";
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
/// allianceauth-secure-groups' `access_sec_group`: the Secure Groups page
/// (check yourself against a smart group, join or ask to, leave).
pub const SECUREGROUPS_ACCESS: &str = "securegroups.access_sec_group";
/// allianceauth-secure-groups' `audit_sec_group`: Secure Group Audit for
/// the smart groups you manage (Group Management over them): every
/// member against every filter, Check now, removing members.
pub const SECUREGROUPS_AUDIT: &str = "securegroups.audit_sec_group";
/// AA's `discord.access_discord`: may link Discord and be in the server.
pub const DISCORD_ACCESS: &str = "discord.access_discord";
/// allianceauth-blacklist's 16 permissions: the Pilot Log's notes (on
/// your own corporation's pilots, or anyone), the Blacklist, the
/// restricted and ultra restricted tiers, and comments on notes.
pub const BLACKLIST_VIEW_BASIC_NOTES: &str = "blacklist.view_basic_eve_notes";
pub const BLACKLIST_VIEW_BLACKLIST: &str = "blacklist.view_eve_blacklist";
pub const BLACKLIST_VIEW_NOTES: &str = "blacklist.view_eve_notes";
pub const BLACKLIST_ADD_BASIC_NOTES: &str = "blacklist.add_basic_eve_notes";
pub const BLACKLIST_ADD_NOTES: &str = "blacklist.add_new_eve_notes";
pub const BLACKLIST_ADD_TO_BLACKLIST: &str = "blacklist.add_to_blacklist";
pub const BLACKLIST_VIEW_RESTRICTED: &str = "blacklist.view_restricted_eve_notes";
pub const BLACKLIST_VIEW_ULTRA: &str = "blacklist.view_ultra_restricted_eve_notes";
pub const BLACKLIST_ADD_RESTRICTED: &str = "blacklist.add_restricted_eve_notes";
pub const BLACKLIST_ADD_ULTRA: &str = "blacklist.add_ultra_restricted_eve_notes";
pub const BLACKLIST_VIEW_COMMENTS: &str = "blacklist.view_eve_note_comments";
pub const BLACKLIST_VIEW_RESTRICTED_COMMENTS: &str = "blacklist.view_eve_note_restricted_comments";
pub const BLACKLIST_VIEW_ULTRA_COMMENTS: &str = "blacklist.view_eve_note_ultra_restricted_comments";
pub const BLACKLIST_ADD_COMMENTS: &str = "blacklist.add_new_eve_note_comments";
pub const BLACKLIST_ADD_RESTRICTED_COMMENTS: &str =
    "blacklist.add_new_eve_note_restricted_comments";
pub const BLACKLIST_ADD_ULTRA_COMMENTS: &str =
    "blacklist.add_new_eve_note_ultra_restricted_comments";
/// Any of these opens the Blacklist page.
pub const BLACKLIST_PAGE: &[&str] = &[
    BLACKLIST_VIEW_BLACKLIST,
    BLACKLIST_VIEW_NOTES,
    BLACKLIST_VIEW_BASIC_NOTES,
];
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
    (
        SECUREGROUPS_ACCESS,
        "Secure Groups: check yourself against smart groups, and join, ask to join or leave them",
    ),
    (
        SECUREGROUPS_AUDIT,
        "Secure Group Audit: every member of the smart groups you manage against each filter; Check now; remove members",
    ),
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
        "Grant and revoke permissions they hold themselves, to states, groups and single users",
    ),
    (
        ADMIN_STATES,
        "Set up access states: who is Member, Blue or in a state you create, and in what order. Holders can only change who is in a state if they hold everything granted to it",
    ),
    (ADMIN_AUDIT, "Read the audit log"),
    (
        BLACKLIST_VIEW_BASIC_NOTES,
        "Pilot Log: see notes on your main's corporation's pilots",
    ),
    (
        BLACKLIST_VIEW_BLACKLIST,
        "See the Blacklist (restricted reasons stay hidden without their tier)",
    ),
    (
        BLACKLIST_VIEW_NOTES,
        "Pilot Log: see every note (on pilots, corporations and alliances)",
    ),
    (
        BLACKLIST_ADD_BASIC_NOTES,
        "Pilot Log: add notes on your main's corporation's pilots",
    ),
    (
        BLACKLIST_ADD_NOTES,
        "Pilot Log: add notes on anyone, and edit notes",
    ),
    (
        BLACKLIST_ADD_TO_BLACKLIST,
        "Blacklist and unblacklist through a note: an account whose main is, or is in, a blacklisted pilot, corporation or alliance is in the Blacklist state",
    ),
    (BLACKLIST_VIEW_RESTRICTED, "Pilot Log: see restricted notes"),
    (
        BLACKLIST_VIEW_ULTRA,
        "Pilot Log: see ultra restricted notes",
    ),
    (BLACKLIST_ADD_RESTRICTED, "Pilot Log: mark notes restricted"),
    (
        BLACKLIST_ADD_ULTRA,
        "Pilot Log: mark notes ultra restricted",
    ),
    (BLACKLIST_VIEW_COMMENTS, "Pilot Log: see comments on notes"),
    (
        BLACKLIST_VIEW_RESTRICTED_COMMENTS,
        "Pilot Log: see restricted comments",
    ),
    (
        BLACKLIST_VIEW_ULTRA_COMMENTS,
        "Pilot Log: see ultra restricted comments",
    ),
    (BLACKLIST_ADD_COMMENTS, "Pilot Log: comment on notes"),
    (
        BLACKLIST_ADD_RESTRICTED_COMMENTS,
        "Pilot Log: add restricted comments",
    ),
    (
        BLACKLIST_ADD_ULTRA_COMMENTS,
        "Pilot Log: add ultra restricted comments",
    ),
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
        FLEETPINGS_ACCESS,
        "Fleet Pings: send fleet pings to Discord, including @everyone",
    ),
    (
        COMPLIANCE_VIEW,
        "See which accounts aren't compliant, what each character is missing (naming their alts), and which corporation members never registered",
    ),
];

/// Where a core permission is listed on Permissions: the area of Tether
/// it opens.
pub fn area(permission: &str) -> &'static str {
    match permission {
        ADMIN_GROUPS | GROUP_MANAGEMENT | REQUEST_GROUPS => "Groups",
        SECUREGROUPS_ACCESS | SECUREGROUPS_AUDIT => "Secure Groups",
        DISCORD_ACCESS | ADMIN_DISCORD => "Discord",
        FLEETPINGS_ACCESS => "Fleet Pings",
        CORPSTATS_CORP | CORPSTATS_ALLIANCE | CORPSTATS_STATE | COMPLIANCE_VIEW => {
            "Corporation Stats and compliance"
        }
        p if p.starts_with("blacklist.") => "Blacklist and Pilot Log",
        _ => "Administration",
    }
}

/// Who a core permission is usually for, and what holding it means, for
/// admins choosing whom to grant it (Permissions, under each one's
/// description): Alliance Auth's conventions where it has them.
pub const CORE_NOTES: &[(&str, &str)] = &[
    (
        ADMIN_GROUPS,
        "For admins. Holders shape every group, Internal ones included, and can add anyone to them, so give it only to those who run the alliance's access.",
    ),
    (
        GROUP_MANAGEMENT,
        "For officers who handle requests for all groups. A group's own leaders don't need it: make them Group Leaders on that group instead, and they handle only theirs.",
    ),
    (
        REQUEST_GROUPS,
        "Usually Member (a fresh Tether gives it to Member, as Alliance Auth's docs advise). Without it pilots only see Public groups; Secure Groups still take their applications.",
    ),
    (
        SECUREGROUPS_ACCESS,
        "Usually Member, and any state whose pilots should apply for smart groups. Without it nobody sees the Secure Groups page, so no one can apply.",
    ),
    (
        SECUREGROUPS_AUDIT,
        "For officers who keep smart groups clean. It shows only the smart groups they manage: they also need Group Management, or to lead the group.",
    ),
    (
        DISCORD_ACCESS,
        "Whoever belongs on your Discord server: usually Member, and Blue if allies share it. Losing it removes the pilot from the server at the next sync.",
    ),
    (
        CORPSTATS_CORP,
        "For a corporation's CEO and directors: their own corporation only.",
    ),
    (
        CORPSTATS_ALLIANCE,
        "For alliance leadership: every corporation in their main's alliance.",
    ),
    (
        CORPSTATS_STATE,
        "For officers over the whole state: every corporation it covers, allies included for Blue.",
    ),
    (
        COMPLIANCE_VIEW,
        "For officers who chase registration. Holders see every account's characters, alts included, so keep it to trusted staff.",
    ),
    (
        ADMIN_PERMISSIONS,
        "For admins. Holders can pass on only what they hold themselves, so it can't be used to climb.",
    ),
    (
        ADMIN_STATES,
        "For admins. States decide who is Member, so holders decide who gets everything granted to Member.",
    ),
    (
        ADMIN_AUDIT,
        "For admins, and anyone who reviews what admins changed.",
    ),
    (
        ADMIN_USERS,
        "For officers who help pilots with their accounts. Holders see every pilot's alts and can lock an account out.",
    ),
    (
        ADMIN_SYSTEM,
        "For whoever runs the server: health, settings, the job queue, upgrades and roll backs.",
    ),
    (
        ADMIN_PLUGINS,
        "For admins. Installing an app decides what it may read and where it may post, and holders see every app's data sources.",
    ),
    (
        ADMIN_DISCORD,
        "For admins. Holders decide which Discord roles every state and group gets.",
    ),
    (
        FLEETPINGS_ACCESS,
        "For FCs and fleet staff. Holders can ping whole states on Discord, so keep it to those who run fleets.",
    ),
    (
        PERMISSIONS_AUDIT,
        "For admins and leadership checking who holds what. It maps out who the admins are.",
    ),
    (
        BLACKLIST_VIEW_BASIC_NOTES,
        "For corporation leaders: notes on their own corporation's pilots only.",
    ),
    (
        BLACKLIST_VIEW_BLACKLIST,
        "For officers who vet recruits. Restricted reasons stay hidden unless they also hold that tier.",
    ),
    (
        BLACKLIST_VIEW_NOTES,
        "For alliance-wide recruiters and security staff: every note.",
    ),
    (
        BLACKLIST_ADD_BASIC_NOTES,
        "For corporation leaders: notes on their own corporation's pilots.",
    ),
    (
        BLACKLIST_ADD_NOTES,
        "For recruiters and security staff: notes on anyone, and editing notes.",
    ),
    (
        BLACKLIST_ADD_TO_BLACKLIST,
        "For alliance security leadership. A blacklisted pilot's account drops to the Blacklist state and loses everything granted to its old state.",
    ),
    (
        BLACKLIST_VIEW_RESTRICTED,
        "For senior security staff: notes marked restricted.",
    ),
    (
        BLACKLIST_VIEW_ULTRA,
        "For the few who handle the most sensitive notes.",
    ),
    (
        BLACKLIST_ADD_RESTRICTED,
        "For senior security staff, alongside seeing restricted notes.",
    ),
    (
        BLACKLIST_ADD_ULTRA,
        "For the few who handle the most sensitive notes.",
    ),
    (
        BLACKLIST_VIEW_COMMENTS,
        "For those who read notes and should see the discussion on them.",
    ),
    (
        BLACKLIST_VIEW_RESTRICTED_COMMENTS,
        "For senior security staff, alongside restricted notes.",
    ),
    (
        BLACKLIST_VIEW_ULTRA_COMMENTS,
        "For the few who handle the most sensitive notes.",
    ),
    (
        BLACKLIST_ADD_COMMENTS,
        "For those who discuss notes: recruiters and security staff.",
    ),
    (
        BLACKLIST_ADD_RESTRICTED_COMMENTS,
        "For senior security staff, alongside restricted notes.",
    ),
    (
        BLACKLIST_ADD_ULTRA_COMMENTS,
        "For the few who handle the most sensitive notes.",
    ),
];

/// [`CORE_NOTES`] for one permission.
pub fn note(permission: &str) -> Option<&'static str> {
    CORE_NOTES
        .iter()
        .find(|(name, _)| *name == permission)
        .map(|(_, note)| *note)
}

pub fn is_known(permission: &str) -> bool {
    CORE_PERMISSIONS.iter().any(|(name, _)| *name == permission)
}

/// Permissions that must never reach people anyone can become (Guest, or
/// a group anyone can join): admin powers, managing groups, and pinging the
/// whole server.
pub fn is_sensitive(permission: &str) -> bool {
    permission.starts_with("admin.")
        || permission == FLEETPINGS_ACCESS
        || permission == COMPLIANCE_VIEW
        || permission == GROUP_MANAGEMENT
        // Who holds what maps out the admins.
        || permission == PERMISSIONS_AUDIT
        || permission.starts_with("blacklist.")
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

    #[test]
    fn every_core_permission_says_who_it_is_for() {
        for (name, _) in CORE_PERMISSIONS {
            assert!(note(name).is_some(), "{name} has no note");
        }
        for (name, _) in CORE_NOTES {
            assert!(is_known(name), "{name} isn't a permission");
        }
        assert_eq!(area(REQUEST_GROUPS), "Groups");
        assert_eq!(area(BLACKLIST_ADD_ULTRA), "Blacklist and Pilot Log");
        assert_eq!(area(ADMIN_SYSTEM), "Administration");
    }
}
