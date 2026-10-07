//! What an app asks for, in plain words, and what changes from one of its
//! versions to the next: the approval screen's lists, and the test for a
//! bundled update Tether may apply itself (nothing new asked for).

use tether_plugins::manifest::Manifest;

use crate::AppState;
use crate::error::AppError;

/// One thing a plugin asks for, in plain words.
#[derive(Clone, PartialEq, Eq)]
pub struct Capability {
    pub title: String,
    pub detail: String,
}

pub fn capabilities(manifest: &Manifest) -> Vec<Capability> {
    let c = &manifest.capabilities;
    let mut lines = Vec::new();
    let mut add = |title: &str, detail: String| {
        lines.push(Capability {
            title: title.to_owned(),
            detail,
        });
    };
    if c.storage {
        add(
            "Database storage",
            "Keeps its own data in a schema only it can use.".to_owned(),
        );
    }
    if !c.esi.user.is_empty() {
        let writes: Vec<&str> = c
            .esi
            .user
            .iter()
            .map(String::as_str)
            .filter(|s| tether_core::scopes::is_write(s))
            .collect();
        add(
            "ESI access to the characters registered for it",
            format!(
                "{}. Whoever holds one of its permissions (below), whatever their state, may \
                 register characters for it: one EVE login from the app grants these. It reads \
                 only the characters registered for it, while their pilots hold one of its \
                 permissions. {}{}",
                c.esi.user.join(", "),
                if manifest.permissions.is_empty() {
                    "It adds no permissions, so only superusers can. "
                } else {
                    ""
                },
                if writes.is_empty() {
                    format!(
                        "No state requires them unless you choose to (States: Require {}'s \
                         scopes).",
                        manifest.plugin.name
                    )
                } else {
                    "No state can require them.".to_owned()
                }
            ),
        );
        if !writes.is_empty() {
            add(
                "Changes pilots' characters in EVE",
                format!(
                    "{}: it can save fittings to a pilot's own registered characters, only when \
                     that pilot presses one of its buttons, one change per press. Each change is \
                     on the audit log as the pilot.",
                    writes.join(", ")
                ),
            );
        }
    }
    if !c.esi.data_source.is_empty() {
        let adders: Vec<&str> = crate::plugin_consent::owner_permissions(manifest);
        add(
            "ESI access through its data sources' characters",
            format!(
                "{}. Data sources (Alliance Auth's owners) are added, and used at once, by {}: \
                 each logs in with one of their own characters. You can see and remove any \
                 data source on the app's page.",
                c.esi.data_source.join(", "),
                if adders.is_empty() {
                    "app admins only".to_owned()
                } else {
                    format!(
                        "holders of {} and app admins",
                        adders
                            .iter()
                            .map(|p| format!("{} ({p})", manifest.permissions[*p]))
                            .collect::<Vec<_>>()
                            .join(" or ")
                    )
                }
            ),
        );
    }
    if !c.discord.is_empty() {
        add("Discord", c.discord.join(", ").replace('_', " "));
    }
    for schedule in &c.schedules {
        add(
            "Scheduled work",
            format!("{} every {}", schedule.name, schedule.every),
        );
    }
    for host in &c.http {
        add(
            "HTTPS requests",
            format!("{host}, which sees this server's IP address"),
        );
    }
    for (name, secret) in &c.secrets {
        add(
            "A secret you enter",
            match &secret.prefix {
                Some(prefix) => format!(
                    "{name}, sent only to {} in the {} header, after \"{prefix}\"",
                    secret.host, secret.header
                ),
                None => format!(
                    "{name}, sent only to {} in the {} header",
                    secret.host, secret.header
                ),
            },
        );
    }
    match c.timers {
        Some(tether_plugins::manifest::TimersAccess::Publish) => add(
            "Shared timers",
            "Publishes timers other apps (such as Structure Timers) can show".to_owned(),
        ),
        Some(tether_plugins::manifest::TimersAccess::Read) => add(
            "Shared timers",
            "Shows timers other apps publish (such as Structures' reinforcement timers)".to_owned(),
        ),
        None => {}
    }
    match c.doctrines {
        Some(tether_plugins::manifest::TimersAccess::Publish) => add(
            "Shared doctrines",
            "Publishes its doctrines (names, links, and which groups see each) for Fleet Pings \
             and other apps to offer, each only to whoever may see it"
                .to_owned(),
        ),
        Some(tether_plugins::manifest::TimersAccess::Read) => add(
            "Shared doctrines",
            "Offers doctrines other apps publish (such as Fittings'), each only to whoever may \
             see it. Which ones it gets tells it which of the publisher's groups a pilot is in"
                .to_owned(),
        ),
        None => {}
    }
    if c.downloads {
        add(
            "Downloads",
            "Offers files for download (CSV Tether writes from rows the app hands over), each \
             only to holders of the permission it names; every download is audited"
                .to_owned(),
        );
    }
    let structures = "esi-universe.read_structures.v1";
    if c.esi.user.iter().any(|s| s == structures)
        || c.esi.data_source.iter().any(|s| s == structures)
    {
        add(
            "Structure names",
            "Can name any Upwell structure a member may dock at, by its id: when ESI won't name \
             one to the app's own character, Tether asks through members' characters that \
             granted the structure scope (the name, system and type only)."
                .to_owned(),
        );
    }
    if c.notify {
        add(
            "Notifications",
            "Sends notices to Tether's notifications (the bell), under its own name, only to \
             accounts holding one of its permissions or that submitted one of its forms, and a \
             limited number an hour"
                .to_owned(),
        );
    }
    if c.groups {
        add(
            "Groups",
            "Sees which groups each pilot using it is in (Hidden and Internal ones included), \
             and the groups they may pick from, to limit things to groups"
                .to_owned(),
        );
    }
    for filter in &manifest.filters {
        let combine = match filter.combine {
            tether_plugins::manifest::Combine::Any => "passes if any character does",
            tether_plugins::manifest::Combine::Sum => "adds characters' values up",
        };
        let fields: Vec<String> = filter
            .fields
            .iter()
            .map(|f| match f.kind {
                tether_plugins::manifest::FieldKind::Text => format!("{} (text)", f.name),
                tether_plugins::manifest::FieldKind::Number => format!("{} (number)", f.name),
            })
            .collect();
        add(
            "Secure Groups filter",
            format!(
                "{} ({}; {combine}{}{}): its answers decide who is in any smart group an admin \
                 uses it for",
                filter.label,
                filter.name,
                if fields.is_empty() { "" } else { "; settings " },
                fields.join(", "),
            ),
        );
    }
    if lines.is_empty() {
        lines.push(Capability {
            title: "Nothing else".to_owned(),
            detail: "It can only show its own pages and write its own log.".to_owned(),
        });
    }
    lines
}

#[derive(Clone, PartialEq, Eq)]
pub struct PermissionRow {
    pub name: String,
    pub description: String,
}

pub fn permissions(manifest: &Manifest) -> Vec<PermissionRow> {
    manifest
        .permissions
        .iter()
        .map(|(name, description)| PermissionRow {
            name: format!("plugin.{}.{name}", manifest.plugin.id),
            description: description.clone(),
        })
        .collect()
}

/// A `[[pages]]` rule: who may open which of its pages.
#[derive(Clone, PartialEq, Eq)]
pub struct PageRuleRow {
    /// "All its pages", or "Pages under <path>".
    pub pages: String,
    /// The permissions that open them, by their full names, any one of
    /// which will do; none when any signed-in pilot may open them.
    pub permissions: Vec<String>,
    /// Every view is written to the audit log.
    pub audited: bool,
}

pub fn page_rules(manifest: &Manifest) -> Vec<PageRuleRow> {
    manifest
        .pages
        .iter()
        .map(|rule| {
            // Sorted: the same names in another order are the same rule.
            let mut permissions: Vec<String> = rule
                .permission
                .as_ref()
                .map_or(&[][..], |p| p.names())
                .iter()
                .map(|p| format!("plugin.{}.{p}", manifest.plugin.id))
                .collect();
            permissions.sort();
            PageRuleRow {
                pages: if rule.path.is_empty() {
                    "All its pages".to_owned()
                } else {
                    format!("Pages under {}", rule.path)
                },
                permissions,
                audited: rule.audit,
            }
        })
        .collect()
}

/// From one version's manifest to another's.
pub struct Changes {
    pub added: Vec<Capability>,
    pub removed: Vec<Capability>,
    pub permissions_added: Vec<PermissionRow>,
    pub permissions_removed: Vec<PermissionRow>,
    /// Kept (with their grants), but described differently.
    pub permissions_changed: Vec<PermissionRow>,
    /// Renamed: their grants move to the new name.
    pub permissions_renamed: Vec<RenameRow>,
    /// Who may open which pages: rules it has now and didn't, and the
    /// reverse (a changed rule is both).
    pub pages_added: Vec<PageRuleRow>,
    pub pages_removed: Vec<PageRuleRow>,
}

/// A permission renamed, and how many grants move with it.
pub struct RenameRow {
    pub from: String,
    pub to: String,
    pub description: String,
    pub grants: i64,
}

impl Changes {
    /// What changes from `old` to `new`, with the grants that renames
    /// would move counted.
    pub async fn counted(
        state: &AppState,
        old: &Manifest,
        new: &Manifest,
    ) -> Result<Self, AppError> {
        let mut changes = Self::new(old, new);
        let names: Vec<String> = changes
            .permissions_renamed
            .iter()
            .map(|r| r.from.clone())
            .collect();
        if !names.is_empty() {
            let counts = tether_db::permissions::grant_counts(&state.db, &names).await?;
            for rename in &mut changes.permissions_renamed {
                rename.grants = counts
                    .iter()
                    .find(|(p, _)| *p == rename.from)
                    .map_or(0, |(_, n)| *n);
            }
        }
        Ok(changes)
    }

    /// What changes from `old` to `new`: everything the review lists. An
    /// update that changes nothing here asks for nothing an admin didn't
    /// approve already, which is what lets Tether apply a bundled one
    /// itself ([`crate::plugins::Plugins::start`]).
    pub fn new(old: &Manifest, new: &Manifest) -> Self {
        let (before, after) = (capabilities(old), capabilities(new));
        let (rules_before, rules_after) = (page_rules(old), page_rules(new));
        let id = &new.plugin.id;
        let full = |name: &str| format!("plugin.{id}.{name}");
        let renames = tether_plugins::manifest::permission_renames(Some(old), new);
        let permissions_renamed: Vec<RenameRow> = renames
            .iter()
            .map(|(from, to)| RenameRow {
                from: full(from),
                to: full(to),
                description: new.permissions.get(to).cloned().unwrap_or_default(),
                grants: 0,
            })
            .collect();
        let renamed = |name: &str| {
            permissions_renamed
                .iter()
                .any(|r| r.from == name || r.to == name)
        };
        let (had, has) = (permissions(old), permissions(new));
        let (had, has): (Vec<PermissionRow>, Vec<PermissionRow>) = (
            had.into_iter().filter(|p| !renamed(&p.name)).collect(),
            has.into_iter().filter(|p| !renamed(&p.name)).collect(),
        );
        Self {
            added: after
                .iter()
                .filter(|c| !before.contains(c))
                .cloned()
                .collect(),
            removed: before
                .iter()
                .filter(|c| !after.contains(c))
                .cloned()
                .collect(),
            permissions_added: has
                .iter()
                .filter(|p| !had.iter().any(|h| h.name == p.name))
                .cloned()
                .collect(),
            permissions_removed: had
                .iter()
                .filter(|p| !has.iter().any(|h| h.name == p.name))
                .cloned()
                .collect(),
            permissions_changed: has
                .iter()
                .filter(|p| {
                    had.iter()
                        .any(|h| h.name == p.name && h.description != p.description)
                })
                .cloned()
                .collect(),
            permissions_renamed,
            pages_added: rules_after
                .iter()
                .filter(|r| !rules_before.contains(r))
                .cloned()
                .collect(),
            pages_removed: rules_before
                .iter()
                .filter(|r| !rules_after.contains(r))
                .cloned()
                .collect(),
        }
    }

    pub fn unchanged(&self) -> bool {
        self.permissions_renamed.is_empty()
            && self.added.is_empty()
            && self.removed.is_empty()
            && self.permissions_added.is_empty()
            && self.permissions_removed.is_empty()
            && self.permissions_changed.is_empty()
            && self.pages_added.is_empty()
            && self.pages_removed.is_empty()
    }

    pub fn any_added(&self) -> bool {
        !self.added.is_empty() || !self.permissions_added.is_empty() || !self.pages_added.is_empty()
    }

    pub fn any_removed(&self) -> bool {
        !self.removed.is_empty()
            || !self.permissions_removed.is_empty()
            || !self.pages_removed.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(main_page: &str) -> Manifest {
        Manifest::parse(&format!(
            "[plugin]\nid = \"acme.hr\"\nname = \"HR\"\nversion = \"1.0.0\"\nhost_api = \"1\"\n\n\
             [permissions]\nview = \"See\"\nmanage = \"Manage\"\n\n[[pages]]\npath = \"\"\n{main_page}\n"
        ))
        .unwrap()
    }

    #[test]
    fn opening_pages_to_everyone_signed_in_asks_again() {
        let before = manifest("permission = \"view\"");
        let after = manifest("signed_in = true");
        let changes = Changes::new(&before, &after);
        assert!(!changes.unchanged());
        assert!(changes.any_added());
        assert_eq!(changes.pages_added.len(), 1);
        assert!(changes.pages_added[0].permissions.is_empty());
        assert!(Changes::new(&after, &after).unchanged());
    }

    #[test]
    fn opening_pages_to_another_permission_asks_again() {
        let before = manifest("permission = \"view\"");
        let after = manifest("permission = [\"view\", \"manage\"]");
        let changes = Changes::new(&before, &after);
        assert!(changes.any_added());
        assert_eq!(
            changes.pages_added[0].permissions,
            ["plugin.acme.hr.manage", "plugin.acme.hr.view"]
        );
        // The same names, written either way or in another order, are the
        // same rule.
        let listed = manifest("permission = [\"view\"]");
        assert!(Changes::new(&before, &listed).unchanged());
        let reordered = manifest("permission = [\"manage\", \"view\"]");
        assert!(Changes::new(&after, &reordered).unchanged());
    }
}
