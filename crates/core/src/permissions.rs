//! Permissions. Core defines the ones below; plugins add their own from
//! their manifests in milestone 2. Grants go to tiers and groups only.

pub const ADMIN_GROUPS: &str = "admin.groups";
pub const ADMIN_PERMISSIONS: &str = "admin.permissions";
pub const ADMIN_TIERS: &str = "admin.tiers";
pub const ADMIN_AUDIT: &str = "admin.audit";

/// Every permission that can be granted, with a description for admins.
pub const CORE_PERMISSIONS: &[(&str, &str)] = &[
    (
        ADMIN_GROUPS,
        "Create and delete groups, manage members and join requests",
    ),
    (
        ADMIN_PERMISSIONS,
        "Grant and revoke permissions (effectively full admin: holders can grant themselves anything)",
    ),
    (
        ADMIN_TIERS,
        "Choose which alliances and corporations are Member or Allied",
    ),
    (ADMIN_AUDIT, "Read the audit log"),
];

pub fn is_known(permission: &str) -> bool {
    CORE_PERMISSIONS.iter().any(|(name, _)| *name == permission)
}

/// How accounts get into a group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinPolicy {
    /// Anyone signed in can join and leave.
    Open,
    /// Members ask; an admin approves or denies.
    Request,
    /// Only admins add and remove members.
    Assigned,
}

impl JoinPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Request => "request",
            Self::Assigned => "assigned",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "open" => Some(Self::Open),
            "request" => Some(Self::Request),
            "assigned" => Some(Self::Assigned),
            _ => None,
        }
    }
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
    fn join_policy_round_trips() {
        for p in [JoinPolicy::Open, JoinPolicy::Request, JoinPolicy::Assigned] {
            assert_eq!(JoinPolicy::parse(p.as_str()), Some(p));
        }
        assert_eq!(JoinPolicy::parse("closed"), None);
    }
}
