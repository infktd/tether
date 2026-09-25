//! Groups, Alliance Auth style: what each flag means and the order AA
//! checks things in when someone joins or leaves. Pure rules; the web
//! layer does the rest.

use crate::states::StateId;

/// A group's settings, with AA's names and meanings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Flags {
    /// Users can't see, join, request, leave or retract; only admins
    /// change its members. Overrides everything else.
    pub internal: bool,
    /// Not listed on the Groups page; joinable through its direct link.
    pub hidden: bool,
    /// Joining and leaving happen at once, without approval.
    pub open: bool,
    /// Joinable without the `request_groups` permission.
    pub public: bool,
    /// Only the owner changes this flag or the group's membership.
    pub restricted: bool,
}

impl Flags {
    /// Whether anyone who may see it can get in without approval: such a
    /// group must never carry sensitive permissions or roles.
    pub fn anyone_can_join(&self) -> bool {
        !self.internal && self.open
    }

    /// The badge users see: Open or Requestable (AA's labels).
    pub fn label(&self) -> &'static str {
        if self.internal {
            "Internal"
        } else if self.open {
            "Open"
        } else {
            "Requestable"
        }
    }
}

/// Whether the account's state may be in the group (no allowed states
/// means every state).
pub fn state_allowed(allowed: &[StateId], state: StateId) -> bool {
    allowed.is_empty() || allowed.contains(&state)
}

/// AA's "joinable": not Internal, and the state is allowed.
pub fn joinable(flags: Flags, allowed: &[StateId], state: StateId) -> bool {
    !flags.internal && state_allowed(allowed, state)
}

/// Listed on the Groups page: joinable, not Hidden, and the account may
/// request it (holds `request_groups`, or the group is Public).
pub fn listed(flags: Flags, allowed: &[StateId], state: StateId, can_request: bool) -> bool {
    joinable(flags, allowed, state) && !flags.hidden && (can_request || flags.public)
}

/// What a join attempt comes to, checked in AA's order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Join {
    NotJoinable,
    AlreadyMember,
    /// Needs `request_groups` (not Public).
    NotAllowed,
    /// Open: added at once.
    Added,
    /// A request (join or leave) is already pending.
    Pending,
    /// A join request for its leaders to decide.
    Requested,
}

pub fn join(
    flags: Flags,
    allowed: &[StateId],
    state: StateId,
    is_member: bool,
    can_request: bool,
    pending: bool,
) -> Join {
    if !joinable(flags, allowed, state) {
        Join::NotJoinable
    } else if is_member {
        Join::AlreadyMember
    } else if !can_request && !flags.public {
        Join::NotAllowed
    } else if flags.open {
        Join::Added
    } else if pending {
        Join::Pending
    } else {
        Join::Requested
    }
}

/// What a leave attempt comes to, checked in AA's order. Leaving never
/// looks at the state or `request_groups`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Leave {
    Internal,
    NotMember,
    /// Open, or auto-leave is on: removed at once.
    Removed,
    Pending,
    /// A leave request for its leaders to decide.
    Requested,
}

pub fn leave(flags: Flags, is_member: bool, pending: bool, auto_leave: bool) -> Leave {
    if flags.internal {
        Leave::Internal
    } else if !is_member {
        Leave::NotMember
    } else if flags.open {
        Leave::Removed
    } else if pending {
        Leave::Pending
    } else if auto_leave {
        Leave::Removed
    } else {
        Leave::Requested
    }
}

/// Longest group description (AA's limit).
pub const MAX_DESCRIPTION: usize = 512;

/// Whether `name` is reserved (case-insensitively).
pub fn is_reserved(name: &str, reserved: &[String]) -> bool {
    let name = name.trim().to_lowercase();
    reserved.iter().any(|r| r.to_lowercase() == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MEMBER: StateId = StateId(1);
    const BLUE: StateId = StateId(2);

    fn requestable() -> Flags {
        Flags::default()
    }

    #[test]
    fn internal_overrides_everything() {
        let internal = Flags {
            internal: true,
            open: true,
            public: true,
            ..Flags::default()
        };
        assert_eq!(
            join(internal, &[], MEMBER, false, true, false),
            Join::NotJoinable
        );
        assert_eq!(leave(internal, true, false, true), Leave::Internal);
        assert!(!listed(internal, &[], MEMBER, true));
        assert!(!internal.anyone_can_join());
    }

    #[test]
    fn join_follows_aas_order() {
        let open = Flags {
            open: true,
            ..Flags::default()
        };
        assert_eq!(
            join(open, &[MEMBER], BLUE, false, true, false),
            Join::NotJoinable
        );
        assert_eq!(
            join(open, &[], MEMBER, true, true, false),
            Join::AlreadyMember
        );
        assert_eq!(
            join(open, &[], MEMBER, false, false, false),
            Join::NotAllowed
        );
        assert_eq!(join(open, &[], MEMBER, false, true, false), Join::Added);
        assert_eq!(
            join(requestable(), &[], MEMBER, false, true, true),
            Join::Pending
        );
        assert_eq!(
            join(requestable(), &[], MEMBER, false, true, false),
            Join::Requested
        );
        // Public lifts request_groups only.
        let public = Flags {
            public: true,
            ..Flags::default()
        };
        assert_eq!(
            join(public, &[], MEMBER, false, false, false),
            Join::Requested
        );
        // Hidden is never checked when joining (the direct link works).
        let hidden = Flags {
            hidden: true,
            open: true,
            ..Flags::default()
        };
        assert_eq!(join(hidden, &[], MEMBER, false, true, false), Join::Added);
    }

    #[test]
    fn leave_follows_aas_order() {
        let open = Flags {
            open: true,
            ..Flags::default()
        };
        assert_eq!(leave(open, false, false, false), Leave::NotMember);
        assert_eq!(leave(open, true, true, false), Leave::Removed);
        assert_eq!(leave(requestable(), true, true, true), Leave::Pending);
        assert_eq!(leave(requestable(), true, false, true), Leave::Removed);
        assert_eq!(leave(requestable(), true, false, false), Leave::Requested);
    }

    #[test]
    fn listing_needs_request_groups_or_public_and_not_hidden() {
        assert!(listed(requestable(), &[], MEMBER, true));
        assert!(!listed(requestable(), &[], MEMBER, false));
        let public = Flags {
            public: true,
            ..Flags::default()
        };
        assert!(listed(public, &[], MEMBER, false));
        let hidden = Flags {
            hidden: true,
            ..Flags::default()
        };
        assert!(!listed(hidden, &[], MEMBER, true));
        assert!(!listed(requestable(), &[BLUE], MEMBER, true));
    }

    #[test]
    fn reserved_names_ignore_case() {
        let reserved = vec!["Directors".to_owned()];
        assert!(is_reserved(" directors ", &reserved));
        assert!(!is_reserved("Officers", &reserved));
    }
}
