//! The sidebar, as AA's Menu: sections (the headings), items, folders and
//! custom links, arranged by admins over a default layout. Items without
//! an entry keep their default place, so new features and apps appear
//! without anyone editing the menu. Who may see an item is never changed
//! here: hiding only hides.

use std::collections::HashMap;

use serde_json::json;
use tether_db::PgPool;
use tether_db::accounts::AccountId;
use tether_db::audit::{self, Actor};
use tether_db::menu::{self as db, Entry, Kind};

use crate::error::AppError;

/// The default sections, in order.
pub const SECTIONS: &[(&str, &str)] = &[
    ("account", "Account"),
    ("fleet", "Fleet"),
    ("apps", "Apps"),
    ("admin", "Admin"),
];

/// A built-in sidebar item: key, label, address, icon, default section,
/// and the page name that marks it current.
pub struct Builtin {
    pub key: &'static str,
    pub label: &'static str,
    pub href: &'static str,
    pub icon: &'static str,
    pub section: &'static str,
    pub active: &'static str,
}

const fn b(
    key: &'static str,
    label: &'static str,
    href: &'static str,
    icon: &'static str,
    section: &'static str,
    active: &'static str,
) -> Builtin {
    Builtin {
        key,
        label,
        href,
        icon,
        section,
        active,
    }
}

pub const BUILTINS: &[Builtin] = &[
    b(
        "dashboard",
        "Dashboard",
        "/dashboard",
        "user",
        "account",
        "profile",
    ),
    b(
        "services",
        "Services",
        "/services",
        "message",
        "account",
        "services",
    ),
    b(
        "tokens",
        "Token Management",
        "/tokens",
        "lock",
        "account",
        "tokens",
    ),
    b("groups", "Groups", "/groups", "users", "account", "groups"),
    b(
        "group_management",
        "Group Management",
        "/group-management",
        "users",
        "account",
        "group_management",
    ),
    b(
        "pings",
        "Fleet Pings",
        "/pings",
        "megaphone",
        "fleet",
        "pings",
    ),
    // Admin pages live in the Administration hub (its rail and overview);
    // they start hidden in the sidebar, and admins can pin any of them on
    // the Menu page.
    b(
        "administration",
        "Administration",
        "/admin",
        "sliders",
        "admin",
        crate::admin_nav::OVERVIEW,
    ),
    b(
        "system",
        "System",
        "/admin/system",
        "activity",
        "admin",
        "system",
    ),
    b(
        "plugins",
        "Apps",
        "/admin/plugins",
        "package",
        "admin",
        "plugins",
    ),
    b("users", "Users", "/admin/users", "user", "admin", "users"),
    b(
        "blacklist",
        "Blacklist",
        "/blacklist",
        "shield",
        "admin",
        "blacklist",
    ),
    b(
        "admin_groups",
        "Groups",
        "/admin/groups",
        "users",
        "admin",
        "admin_groups",
    ),
    b(
        "autogroups",
        "Auto Groups",
        "/admin/autogroups",
        "layers",
        "admin",
        "autogroups",
    ),
    b(
        "permissions",
        "Permissions",
        "/admin/permissions",
        "shield",
        "admin",
        "permissions",
    ),
    b(
        "permissions_audit",
        "Permissions Audit",
        "/admin/permissions/audit",
        "search",
        "admin",
        "permissions_audit",
    ),
    b(
        "states",
        "States",
        "/admin/states",
        "layers",
        "admin",
        "states",
    ),
    b(
        "discord",
        "Discord",
        "/admin/discord",
        "message",
        "admin",
        "discord",
    ),
    b(
        "compliance",
        "Compliance Report",
        "/compliance",
        "check",
        "admin",
        "compliance",
    ),
    b(
        "corpstats",
        "Corporation Stats",
        "/corpstats",
        "activity",
        "admin",
        "corpstats",
    ),
    b(
        "audit",
        "Audit log",
        "/admin/audit",
        "scroll",
        "admin",
        "audit",
    ),
    b("setup", "Setup", "/setup", "settings", "admin", "setup"),
];

/// An item one viewer may see (or, for the Menu page, any item).
#[derive(Debug, Clone)]
pub struct Available {
    pub key: String,
    pub label: String,
    pub href: String,
    pub icon: &'static str,
    pub section: &'static str,
    /// The page name that marks it current (built-in items).
    pub active: &'static str,
    pub badge: Option<i64>,
    /// Hidden in the sidebar until an admin shows it (admin pages).
    pub default_hidden: bool,
}

/// An app's sidebar link as an item: keyed by its page.
pub fn plugin_item(label: &str, href: &str) -> Available {
    Available {
        key: format!("plugin:{}", href.trim_start_matches("/plugins/")),
        label: label.to_owned(),
        href: href.to_owned(),
        icon: "package",
        section: "apps",
        active: "",
        badge: None,
        default_hidden: false,
    }
}

pub fn builtin_item(b: &Builtin, badge: Option<i64>) -> Available {
    Available {
        key: b.key.to_owned(),
        label: b.label.to_owned(),
        href: b.href.to_owned(),
        icon: b.icon,
        section: b.section,
        active: b.active,
        badge,
        default_hidden: b.section == "admin" && b.key != "administration",
    }
}

/// A link, item or folder in a section.
#[derive(Debug, Clone)]
pub struct Node {
    /// `id:<n>` for entries, or an item's key.
    pub reference: String,
    pub kind: Kind,
    pub label: String,
    /// An item's own name, when renamed.
    pub default_label: Option<String>,
    pub href: String,
    pub icon: &'static str,
    /// The page name that marks it current, if built in.
    pub active: &'static str,
    pub badge: Option<i64>,
    pub new_tab: bool,
    pub hidden: bool,
    /// Folders: what's in them.
    pub children: Vec<Node>,
    /// The reference of the section or folder it's in.
    pub parent: String,
    position: i32,
}

impl Node {
    pub fn is_folder(&self) -> bool {
        self.kind == Kind::Folder
    }

    pub fn is_link(&self) -> bool {
        self.kind == Kind::Link
    }

    pub fn kind_is_item(&self) -> bool {
        self.kind == Kind::Item
    }

    /// Whether it's the page being shown: by page name, or by address for
    /// apps' pages.
    pub fn is_current(&self, active: &str, active_href: &str) -> bool {
        (!self.active.is_empty() && self.active == active)
            || (self.kind == Kind::Item && !active_href.is_empty() && self.href == active_href)
            // Administration stays marked on every page in its hub.
            || (self.active == crate::admin_nav::OVERVIEW && crate::admin_nav::is_admin_page(active))
    }

    /// A folder holding the current page opens by itself.
    pub fn open(&self, active: &str, active_href: &str) -> bool {
        self.children
            .iter()
            .any(|c| c.is_current(active, active_href))
    }
}

#[derive(Debug, Clone)]
pub struct Section {
    pub reference: String,
    pub label: String,
    pub hidden: bool,
    /// Made by an admin (so it can be deleted).
    pub custom: bool,
    pub nodes: Vec<Node>,
    position: i32,
}

fn reference(entry: &Entry) -> String {
    match (&entry.kind, &entry.key) {
        (Kind::Item, Some(key)) => key.clone(),
        _ => format!("id:{}", entry.id),
    }
}

/// Sections with everything in them, hidden entries included.
pub fn build(entries: &[Entry], items: Vec<Available>) -> Vec<Section> {
    let mut sections: Vec<Section> = Vec::new();
    // Section entry id, or default key, to its index.
    let mut by_id: HashMap<i64, usize> = HashMap::new();
    let mut by_key: HashMap<&str, usize> = HashMap::new();
    for entry in entries.iter().filter(|e| e.kind == Kind::Section) {
        let default = entry
            .key
            .as_deref()
            .and_then(|k| k.strip_prefix("section:"))
            .and_then(|k| SECTIONS.iter().find(|(name, _)| *name == k));
        by_id.insert(entry.id, sections.len());
        if let Some((name, _)) = default {
            by_key.insert(name, sections.len());
        }
        sections.push(Section {
            reference: reference(entry),
            label: entry
                .label
                .clone()
                .or_else(|| default.map(|(_, label)| (*label).to_owned()))
                .unwrap_or_default(),
            hidden: entry.hidden,
            custom: entry.key.is_none(),
            nodes: Vec::new(),
            position: entry.position,
        });
    }
    for (i, (name, label)) in SECTIONS.iter().enumerate() {
        if !by_key.contains_key(name) {
            by_key.insert(name, sections.len());
            sections.push(Section {
                reference: format!("section:{name}"),
                label: (*label).to_owned(),
                hidden: false,
                custom: false,
                nodes: Vec::new(),
                position: 100_000 + i32::try_from(i).unwrap_or(0) * 10,
            });
        }
    }
    // Folders, by entry id: (section index, node).
    let mut folders: HashMap<i64, (usize, Node)> = HashMap::new();
    for entry in entries.iter().filter(|e| e.kind == Kind::Folder) {
        let Some(&section) = entry.parent_id.and_then(|p| by_id.get(&p)) else {
            continue;
        };
        folders.insert(
            entry.id,
            (
                section,
                Node {
                    reference: reference(entry),
                    kind: Kind::Folder,
                    label: entry.label.clone().unwrap_or_default(),
                    default_label: None,
                    href: String::new(),
                    icon: "folder",
                    active: "",
                    badge: None,
                    new_tab: false,
                    hidden: entry.hidden,
                    children: Vec::new(),
                    parent: sections[section].reference.clone(),
                    position: entry.position,
                },
            ),
        );
    }
    let by_item: HashMap<&str, &Entry> = entries
        .iter()
        .filter(|e| e.kind == Kind::Item)
        .filter_map(|e| Some((e.key.as_deref()?, e)))
        .collect();
    // Where a node goes: a folder, or a section (index).
    enum Place {
        Folder(i64),
        Section(usize),
    }
    let place = |parent: Option<i64>, default: Option<&str>| -> Option<Place> {
        match parent {
            Some(p) if folders.contains_key(&p) => Some(Place::Folder(p)),
            Some(p) if by_id.contains_key(&p) => by_id.get(&p).map(|i| Place::Section(*i)),
            _ => default
                .and_then(|d| by_key.get(d))
                .map(|i| Place::Section(*i)),
        }
    };
    let mut placed: Vec<(Place, Node)> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (index, item) in items.into_iter().enumerate() {
        // One entry per key, even if an app lists a page twice.
        if !seen.insert(item.key.clone()) {
            continue;
        }
        let entry = by_item.get(item.key.as_str());
        let Some(at) = place(entry.and_then(|e| e.parent_id), Some(item.section)) else {
            continue;
        };
        let renamed = entry.and_then(|e| e.label.clone());
        placed.push((
            at,
            Node {
                reference: item.key.clone(),
                kind: Kind::Item,
                default_label: renamed.as_ref().map(|_| item.label.clone()),
                label: renamed.unwrap_or(item.label),
                href: item.href,
                icon: item.icon,
                active: item.active,
                badge: item.badge,
                new_tab: false,
                hidden: entry.map_or(item.default_hidden, |e| e.hidden),
                children: Vec::new(),
                parent: String::new(),
                position: entry.map_or(100_000 + i32::try_from(index).unwrap_or(0), |e| e.position),
            },
        ));
    }
    for entry in entries.iter().filter(|e| e.kind == Kind::Link) {
        let Some(at) = place(entry.parent_id, None) else {
            continue;
        };
        placed.push((
            at,
            Node {
                reference: reference(entry),
                kind: Kind::Link,
                label: entry.label.clone().unwrap_or_default(),
                default_label: None,
                href: entry.url.clone().unwrap_or_default(),
                icon: "external",
                active: "",
                badge: None,
                new_tab: entry.new_tab,
                hidden: entry.hidden,
                children: Vec::new(),
                parent: String::new(),
                position: entry.position,
            },
        ));
    }
    for (at, mut node) in placed {
        match at {
            Place::Folder(id) => {
                if let Some((_, folder)) = folders.get_mut(&id) {
                    node.parent = folder.reference.clone();
                    folder.children.push(node);
                }
            }
            Place::Section(i) => {
                node.parent = sections[i].reference.clone();
                sections[i].nodes.push(node);
            }
        }
    }
    for (section, mut folder) in folders.into_values() {
        folder
            .children
            .sort_by(|a, b| (a.position, &a.label).cmp(&(b.position, &b.label)));
        sections[section].nodes.push(folder);
    }
    for section in &mut sections {
        section
            .nodes
            .sort_by(|a, b| (a.position, &a.label).cmp(&(b.position, &b.label)));
    }
    sections.sort_by(|a, b| (a.position, &a.label).cmp(&(b.position, &b.label)));
    sections
}

/// What one viewer's sidebar shows: nothing hidden, no empty folders or
/// sections.
pub fn visible(mut sections: Vec<Section>) -> Vec<Section> {
    sections.retain(|s| !s.hidden);
    for section in &mut sections {
        section.nodes.retain(|n| !n.hidden);
        for node in &mut section.nodes {
            node.children.retain(|c| !c.hidden);
        }
        section
            .nodes
            .retain(|n| !n.is_folder() || !n.children.is_empty());
    }
    sections.retain(|s| !s.nodes.is_empty());
    sections
}

/// The sidebar for a viewer, from the menu entries and what they may see.
pub async fn sidebar(db: &PgPool, items: Vec<Available>) -> Result<Vec<Section>, sqlx::Error> {
    Ok(visible(build(&db::entries(db).await?, items)))
}

// ---- editing (admin.system) -------------------------------------------------

/// Writes every section and item as an entry, in the order shown, with
/// positions 10, 20, ...: what editing starts from.
async fn materialize(
    tx: &mut sqlx::PgConnection,
    catalogue: &[Available],
) -> Result<Vec<Section>, AppError> {
    db::lock(tx).await?;
    let entries = db::entries(&mut *tx).await?;
    for (i, section) in build(&entries, catalogue.to_vec()).iter().enumerate() {
        if let Some(name) = section.reference.strip_prefix("section:") {
            db::insert(
                &mut *tx,
                db::NewEntry {
                    kind: Kind::Section,
                    key: Some(&format!("section:{name}")),
                    label: None,
                    url: None,
                    new_tab: false,
                    parent_id: None,
                    position: position(i),
                    hidden: false,
                },
            )
            .await?;
        }
    }
    let entries = db::entries(&mut *tx).await?;
    let sections = build(&entries, catalogue.to_vec());
    let id_of = |reference: &str| -> Option<i64> {
        reference.strip_prefix("id:").and_then(|id| id.parse().ok())
    };
    for (i, section) in sections.iter().enumerate() {
        let Some(section_id) = id_of(&section.reference) else {
            continue;
        };
        db::set_position(&mut *tx, section_id, position(i)).await?;
        for (j, node) in section.nodes.iter().enumerate() {
            place_node(tx, &entries, node, section_id, position(j)).await?;
            if let Some(folder_id) = id_of(&node.reference) {
                for (k, child) in node.children.iter().enumerate() {
                    place_node(tx, &entries, child, folder_id, position(k)).await?;
                }
            }
        }
    }
    let entries = db::entries(&mut *tx).await?;
    Ok(build(&entries, catalogue.to_vec()))
}

fn position(index: usize) -> i32 {
    i32::try_from(index).unwrap_or(i32::MAX / 10 - 1) * 10 + 10
}

/// Gives a node its entry (items may have none yet) at a position.
async fn place_node(
    tx: &mut sqlx::PgConnection,
    entries: &[Entry],
    node: &Node,
    parent: i64,
    at: i32,
) -> Result<(), AppError> {
    let existing = entries.iter().find(|e| reference(e) == node.reference);
    match existing {
        Some(entry) => {
            if entry.kind == Kind::Item && entry.parent_id.is_none() {
                db::update(
                    &mut *tx,
                    entry.id,
                    db::Change {
                        label: entry.label.as_deref(),
                        url: None,
                        new_tab: false,
                        parent_id: Some(parent),
                        hidden: entry.hidden,
                    },
                )
                .await?;
            }
            db::set_position(&mut *tx, entry.id, at).await?;
        }
        None if node.kind == Kind::Item => {
            db::insert(
                &mut *tx,
                db::NewEntry {
                    kind: Kind::Item,
                    key: Some(&node.reference),
                    label: None,
                    url: None,
                    new_tab: false,
                    parent_id: Some(parent),
                    position: at,
                    // Where it starts hidden (admin pages), it stays so.
                    hidden: node.hidden,
                },
            )
            .await?;
        }
        None => {}
    }
    Ok(())
}

/// The entry id for a reference, after [`materialize`].
async fn entry_id(tx: &mut sqlx::PgConnection, reference: &str) -> Result<Entry, AppError> {
    let entries = db::entries(&mut *tx).await?;
    entries
        .into_iter()
        .find(|e| self::reference(e) == reference || e.key.as_deref() == Some(reference))
        .ok_or_else(|| AppError::not_found("No such menu entry."))
}

fn label(text: &str, required: bool) -> Result<Option<String>, AppError> {
    let text = text.trim();
    if text.is_empty() {
        return if required {
            Err(AppError::bad_request("Give it a name."))
        } else {
            Ok(None)
        };
    }
    let invisible = |c: char| {
        c.is_control()
            || matches!(c, '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{206F}' | '\u{FEFF}')
    };
    if text.chars().count() > 40 || text.chars().any(invisible) {
        return Err(AppError::bad_request(
            "Names are at most 40 characters, on one line.",
        ));
    }
    Ok(Some(text.to_owned()))
}

/// A custom link's address: `https://...` or a page here (`/...`).
fn url(text: &str) -> Result<String, AppError> {
    let text = text.trim();
    let bad =
        || AppError::bad_request("A link is an https:// address or a page here starting with /.");
    if text.len() > 500 || text.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err(bad());
    }
    if let Some(page) = text.strip_prefix('/') {
        // `//host` would leave the site.
        if page.starts_with('/') || page.starts_with('\\') {
            return Err(bad());
        }
        return Ok(text.to_owned());
    }
    let parsed = reqwest::Url::parse(text).map_err(|_| bad())?;
    if parsed.scheme() != "https" || parsed.host_str().is_none() || parsed.as_str().len() > 500 {
        return Err(bad());
    }
    Ok(parsed.to_string())
}

/// What an admin asked for.
pub enum Edit<'a> {
    AddSection {
        label: &'a str,
    },
    AddFolder {
        label: &'a str,
        parent: &'a str,
    },
    AddLink {
        label: &'a str,
        url: &'a str,
        new_tab: bool,
        parent: &'a str,
    },
    Change {
        reference: &'a str,
        label: &'a str,
        url: &'a str,
        new_tab: bool,
        parent: &'a str,
        hidden: bool,
    },
    Move {
        reference: &'a str,
        up: bool,
    },
    Delete {
        reference: &'a str,
    },
    Reset,
}

/// Most sections, folders and links together.
pub const MAX_ADDED: usize = 200;

pub async fn edit(
    db_pool: &PgPool,
    actor: AccountId,
    catalogue: &[Available],
    edit: Edit<'_>,
) -> Result<(), AppError> {
    let mut tx = db_pool.begin().await?;
    let sections = materialize(&mut tx, catalogue).await?;
    if matches!(
        edit,
        Edit::AddSection { .. } | Edit::AddFolder { .. } | Edit::AddLink { .. }
    ) {
        let added = db::entries(&mut *tx)
            .await?
            .iter()
            .filter(|e| e.key.is_none())
            .count();
        if added >= MAX_ADDED {
            return Err(AppError::bad_request(format!(
                "The menu has {MAX_ADDED} sections, folders and links already."
            )));
        }
    }
    let (action, target, details) = match edit {
        Edit::Reset => {
            db::reset(&mut *tx).await?;
            ("menu.reset", None, json!({}))
        }
        Edit::AddSection { label: text } => {
            let text = label(text, true)?;
            let id = db::insert(
                &mut *tx,
                db::NewEntry {
                    kind: Kind::Section,
                    key: None,
                    label: text.as_deref(),
                    url: None,
                    new_tab: false,
                    parent_id: None,
                    position: position(sections.len()),
                    hidden: false,
                },
            )
            .await?;
            (
                "menu.add",
                Some(format!("menu:{id}")),
                json!({ "kind": "section", "label": text }),
            )
        }
        Edit::AddFolder {
            label: text,
            parent,
        } => {
            let text = label(text, true)?;
            let section = entry_id(&mut tx, parent).await?;
            if section.kind != Kind::Section {
                return Err(AppError::bad_request("Folders go in a section."));
            }
            let id = db::insert(
                &mut *tx,
                db::NewEntry {
                    kind: Kind::Folder,
                    key: None,
                    label: text.as_deref(),
                    url: None,
                    new_tab: false,
                    parent_id: Some(section.id),
                    position: 100_000,
                    hidden: false,
                },
            )
            .await?;
            (
                "menu.add",
                Some(format!("menu:{id}")),
                json!({ "kind": "folder", "label": text }),
            )
        }
        Edit::AddLink {
            label: text,
            url: address,
            new_tab,
            parent,
        } => {
            let text = label(text, true)?;
            let address = url(address)?;
            let parent = entry_id(&mut tx, parent).await?;
            if !matches!(parent.kind, Kind::Section | Kind::Folder) {
                return Err(AppError::bad_request("Links go in a section or folder."));
            }
            let id = db::insert(
                &mut *tx,
                db::NewEntry {
                    kind: Kind::Link,
                    key: None,
                    label: text.as_deref(),
                    url: Some(&address),
                    new_tab,
                    parent_id: Some(parent.id),
                    position: 100_000,
                    hidden: false,
                },
            )
            .await?;
            (
                "menu.add",
                Some(format!("menu:{id}")),
                json!({ "kind": "link", "label": text, "url": address }),
            )
        }
        Edit::Change {
            reference,
            label: text,
            url: address,
            new_tab,
            parent,
            hidden,
        } => {
            let entry = entry_id(&mut tx, reference).await?;
            let text = label(
                text,
                !matches!(entry.kind, Kind::Item | Kind::Section)
                    || entry.key.is_none() && entry.kind == Kind::Section,
            )?;
            let address = if entry.kind == Kind::Link {
                Some(url(address)?)
            } else {
                None
            };
            let parent_id = match entry.kind {
                Kind::Section => None,
                _ => {
                    let parent = entry_id(&mut tx, parent).await?;
                    let fits = match entry.kind {
                        // One level of folders.
                        Kind::Folder => parent.kind == Kind::Section,
                        _ => matches!(parent.kind, Kind::Section | Kind::Folder),
                    };
                    if !fits || parent.id == entry.id {
                        return Err(AppError::bad_request("It can't go there."));
                    }
                    Some(parent.id)
                }
            };
            db::update(
                &mut *tx,
                entry.id,
                db::Change {
                    label: text.as_deref(),
                    url: address.as_deref(),
                    new_tab: new_tab && entry.kind == Kind::Link,
                    parent_id,
                    hidden,
                },
            )
            .await?;
            (
                "menu.change",
                Some(format!("menu:{}", entry.id)),
                json!({ "reference": reference, "label": text, "url": address, "parent": parent_id, "hidden": hidden }),
            )
        }
        Edit::Move { reference, up } => {
            let entry = entry_id(&mut tx, reference).await?;
            let entries = db::entries(&mut *tx).await?;
            // Siblings in order (positions were just renumbered).
            let mut siblings: Vec<&Entry> = entries
                .iter()
                .filter(|e| {
                    if entry.kind == Kind::Section {
                        e.kind == Kind::Section
                    } else {
                        e.kind != Kind::Section && e.parent_id == entry.parent_id
                    }
                })
                // Rows for apps no longer running aren't shown: skip them.
                .filter(|e| {
                    e.kind != Kind::Item
                        || catalogue
                            .iter()
                            .any(|c| Some(c.key.as_str()) == e.key.as_deref())
                })
                .collect();
            siblings.sort_by_key(|e| (e.position, e.id));
            let index = siblings
                .iter()
                .position(|e| e.id == entry.id)
                .ok_or_else(|| AppError::not_found("No such menu entry."))?;
            let other = if up {
                index.checked_sub(1)
            } else {
                Some(index + 1)
            };
            if let Some(other) = other.and_then(|i| siblings.get(i)) {
                db::set_position(&mut *tx, entry.id, other.position).await?;
                db::set_position(&mut *tx, other.id, entry.position).await?;
            }
            (
                "menu.move",
                Some(format!("menu:{}", entry.id)),
                json!({ "up": up }),
            )
        }
        Edit::Delete { reference } => {
            let entry = entry_id(&mut tx, reference).await?;
            let refuse = || {
                AppError::bad_request(
                    "Only sections, folders and links you added can be deleted; hide anything else.",
                )
            };
            if entry.kind == Kind::Item || entry.key.is_some() {
                return Err(refuse());
            }
            if !db::delete(&mut tx, entry.id).await? {
                return Err(refuse());
            }
            (
                "menu.delete",
                Some(format!("menu:{}", entry.id)),
                json!({ "label": entry.label }),
            )
        }
    };
    audit::record(
        &mut *tx,
        Actor::Account(actor),
        action,
        target.as_deref(),
        details,
    )
    .await?;
    tx.commit().await?;
    Ok(())
}
