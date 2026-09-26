//! Administration (DESIGN.md): every admin page in one place, grouped by
//! what it's about. The sidebar shows one Administration item; its
//! overview (`/admin`) and the rail on each admin page list the pages a
//! viewer may open, and nothing else.

use crate::pages::AdminNav;

/// The overview's page name (`Shell::active`).
pub const OVERVIEW: &str = "admin_overview";

/// A group of admin pages, in order.
pub struct Group {
    pub key: &'static str,
    pub label: &'static str,
    /// What its pages are for, in one line.
    pub about: &'static str,
}

pub const GROUPS: &[Group] = &[
    Group {
        key: "access",
        label: "Access",
        about: "Who gets in, and what each pilot may do.",
    },
    Group {
        key: "members",
        label: "Members",
        about: "The pilots behind the accounts.",
    },
    Group {
        key: "integrations",
        label: "Integrations",
        about: "Discord, fleet pings and apps.",
    },
    Group {
        key: "instance",
        label: "Instance",
        about: "This server: health, updates, the sidebar and the record of changes.",
    },
];

/// An admin page.
pub struct Page {
    /// Its page name (`Shell::active`).
    pub active: &'static str,
    pub label: &'static str,
    pub href: &'static str,
    pub icon: &'static str,
    pub group: &'static str,
    /// What it does, in one sentence.
    pub about: &'static str,
}

pub const PAGES: &[Page] = &[
    Page {
        active: "states",
        label: "States",
        href: "/admin/states",
        icon: "layers",
        group: "access",
        about: "Member, Blue, Guest and your own: which corporations, alliances and pilots each covers.",
    },
    Page {
        active: "admin_groups",
        label: "Groups",
        href: "/admin/groups",
        icon: "users",
        group: "access",
        about: "Open, request and hidden groups, their leaders, and Secure Groups' filters.",
    },
    Page {
        active: "autogroups",
        label: "Auto Groups",
        href: "/admin/autogroups",
        icon: "layers",
        group: "access",
        about: "A group for every corporation and alliance in chosen states, kept up to date.",
    },
    Page {
        active: "permissions",
        label: "Permissions",
        href: "/admin/permissions",
        icon: "shield",
        group: "access",
        about: "What each state and group may do in Tether and its apps.",
    },
    Page {
        active: "permissions_audit",
        label: "Permissions Audit",
        href: "/admin/permissions/audit",
        icon: "search",
        group: "access",
        about: "Every permission, and exactly who holds it and through what.",
    },
    Page {
        active: "users",
        label: "Users",
        href: "/admin/users",
        icon: "user",
        group: "members",
        about: "Find any account by character; see its state, groups and permissions.",
    },
    Page {
        active: "blacklist",
        label: "Blacklist",
        href: "/blacklist",
        icon: "shield",
        group: "members",
        about: "Pilots, corporations and alliances kept out, with notes on why.",
    },
    Page {
        active: "compliance",
        label: "Compliance Report",
        href: "/compliance",
        icon: "check",
        group: "members",
        about: "Who hasn't registered every character with the access their state needs.",
    },
    Page {
        active: "corpstats",
        label: "Corporation Stats",
        href: "/corpstats",
        icon: "activity",
        group: "members",
        about: "Each corporation's mains, members and unregistered characters.",
    },
    Page {
        active: "discord",
        label: "Discord",
        href: "/admin/discord",
        icon: "message",
        group: "integrations",
        about: "The server, its roles for states and groups, and nicknames.",
    },
    Page {
        active: "pings_settings",
        label: "Fleet Pings",
        href: "/admin/pings",
        icon: "megaphone",
        group: "integrations",
        about: "Ping channels, fleet types, doctrines and who may ping where.",
    },
    Page {
        active: "plugins",
        label: "Apps",
        href: "/admin/plugins",
        icon: "package",
        group: "integrations",
        about: "Install, upgrade and roll back apps, and approve what they may reach.",
    },
    Page {
        active: "system",
        label: "System",
        href: "/admin/system",
        icon: "activity",
        group: "instance",
        about: "ESI requests, the job queue and schedules, platform updates and the accent colour.",
    },
    Page {
        active: "menu",
        label: "Menu",
        href: "/admin/menu",
        icon: "folder",
        group: "instance",
        about: "Arrange the sidebar: sections, folders, custom links and what's pinned.",
    },
    Page {
        active: "audit",
        label: "Audit log",
        href: "/admin/audit",
        icon: "scroll",
        group: "instance",
        about: "Every admin action and access change, newest first.",
    },
    Page {
        active: "setup",
        label: "Setup",
        href: "/setup",
        icon: "settings",
        group: "instance",
        about: "The setup wizard: the EVE application and the alliance this instance is for.",
    },
];

/// Whether `active` is the overview or one of its pages.
pub fn is_admin_page(active: &str) -> bool {
    active == OVERVIEW || PAGES.iter().any(|p| p.active == active)
}

/// Whether a viewer with `nav` may open a page.
pub fn may(nav: &AdminNav, page: &Page) -> bool {
    match page.active {
        "states" => nav.states,
        "admin_groups" | "autogroups" => nav.groups,
        "permissions" => nav.permissions,
        "permissions_audit" => nav.permissions_audit,
        "users" => nav.users,
        "blacklist" => nav.blacklist,
        "compliance" => nav.compliance,
        "corpstats" => nav.corpstats,
        "discord" | "pings_settings" => nav.discord,
        "plugins" => nav.plugins,
        "system" | "menu" => nav.system,
        "audit" => nav.audit,
        "setup" => nav.setup,
        _ => false,
    }
}

/// A group with the pages one viewer may open.
pub struct Listed {
    pub label: &'static str,
    pub about: &'static str,
    pub pages: Vec<Link>,
}

pub struct Link {
    pub label: &'static str,
    pub href: &'static str,
    pub icon: &'static str,
    pub about: &'static str,
    pub current: bool,
}

/// The groups one viewer may open pages in, `active` marked; empty
/// groups left out.
pub fn listed(nav: &AdminNav, active: &str) -> Vec<Listed> {
    GROUPS
        .iter()
        .filter_map(|group| {
            let pages: Vec<Link> = PAGES
                .iter()
                .filter(|p| p.group == group.key && may(nav, p))
                .map(|p| Link {
                    label: p.label,
                    href: p.href,
                    icon: p.icon,
                    about: p.about,
                    current: p.active == active,
                })
                .collect();
            (!pages.is_empty()).then_some(Listed {
                label: group.label,
                about: group.about,
                pages,
            })
        })
        .collect()
}

/// The rail an admin page shows, if `active` is one (not the overview,
/// whose tiles are the same list).
pub fn rail(nav: &AdminNav, active: &str) -> Option<Vec<Listed>> {
    (active != OVERVIEW && is_admin_page(active)).then(|| listed(nav, active))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_page_is_in_a_group_and_permitted_by_something() {
        let all = AdminNav {
            groups: true,
            permissions: true,
            states: true,
            discord: true,
            system: true,
            plugins: true,
            audit: true,
            setup: true,
            compliance: true,
            corpstats: true,
            permissions_audit: true,
            users: true,
            blacklist: true,
            pings: true,
        };
        for page in PAGES {
            assert!(GROUPS.iter().any(|g| g.key == page.group), "{}", page.label);
            assert!(may(&all, page), "{}", page.label);
        }
        let shown: usize = listed(&all, "").iter().map(|g| g.pages.len()).sum();
        assert_eq!(shown, PAGES.len());
        assert!(listed(&AdminNav::default(), "").is_empty());
    }
}
