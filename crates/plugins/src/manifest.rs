//! `plugin.toml`: who a plugin is, and everything it asks for.
//!
//! Unknown fields are refused, so a typo can't silently drop a capability
//! (or an admin miss one). Everything a plugin declares here is shown to
//! the admin before install, and nothing undeclared is ever granted.

use std::collections::BTreeMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// The only host API major version this host speaks.
pub const HOST_API: &str = "1";

/// An app's settings page (`[[pages]] path = "settings"`): opened from
/// the app's Administration page, left out of the app's own header.
pub const SETTINGS_PATH: &str = "settings";

/// A settings page or one under it (`settings/tags`).
pub fn is_settings(path: &str) -> bool {
    path == SETTINGS_PATH || path.starts_with("settings/")
}

/// Pages Tether draws inside every app's space, under its Manage
/// (DESIGN.md, App shell): never an app's own.
pub const DATA_SOURCES_PATH: &str = "data-sources";
pub const ACTIVITY_PATH: &str = "activity";

/// One of Tether's own pages in an app's space.
pub fn is_host_page(path: &str) -> bool {
    path == DATA_SOURCES_PATH || path == ACTIVITY_PATH
}

/// One of Tether's own pages, or under one, in any case: never the app's.
pub fn under_host_page(path: &str) -> bool {
    path.split('/')
        .next()
        .is_some_and(|first| is_host_page(&first.to_ascii_lowercase()))
}

/// The labels of Tether's own Manage pages, which no app's may wear.
const HOST_LABELS: [&str; 2] = ["data sources", "activity"];

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("plugin.toml: {0}")]
pub struct ManifestError(pub String);

fn bad(text: impl Into<String>) -> ManifestError {
    ManifestError(text.into())
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub plugin: Identity,
    /// Who signs its packages. Every package installed from a file or
    /// GitHub needs one ([`crate::package::Unverified::verify`] refuses
    /// it otherwise); only the apps bundled into Tether's image leave it
    /// out, as they aren't signed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publisher: Option<Publisher>,
    #[serde(default)]
    pub capabilities: Capabilities,
    /// Permission name to description, e.g. `view = "View the mining
    /// ledger"`. Granted like core ones, as `plugin.<id>.<name>`.
    #[serde(default)]
    pub permissions: BTreeMap<String, String>,
    /// What holding a permission means and who it's usually for, by name,
    /// for admins deciding whom to grant it (Permissions shows it under
    /// the description), e.g. `characters_access = "For recruiters: ..."`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub permission_notes: BTreeMap<String, String>,
    /// Permissions an earlier version called something else, old name to
    /// new, e.g. `view = "extractions_access"`: on an upgrade the old
    /// one's grants move to the new one instead of going with it.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub renamed_permissions: BTreeMap<String, String>,
    /// Who may open which pages: a path prefix and the permission it needs.
    /// A page no rule covers is for admins only (`admin.plugins`).
    #[serde(default)]
    pub pages: Vec<PageRule>,
    /// Sidebar entries, shown to whoever may open their page.
    #[serde(default)]
    pub navigation: Vec<NavEntry>,
    /// The app's views, in the order of its views bar (DESIGN.md, App
    /// shell); the first is its main page, its Overview. Each shows to
    /// whoever may open its page. Required of an app with pages to open
    /// (`[[pages]]` or `[[navigation]]`): Tether draws every app's frame.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub views: Vec<PageLink>,
    /// Its one primary action, in its header on every view but its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<PageLink>,
    /// Pages for those who run the app, its Manage pages with Tether's
    /// own (Settings, Data sources, Activity).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub manage: Vec<PageLink>,
    /// Dashboard widgets, which Tether no longer shows (the Dashboard is
    /// the character audit). Still read, so packages that declare them
    /// load; never used.
    #[serde(default, skip_serializing)]
    pub widgets: Vec<Widget>,
    /// Secure Groups filters it offers.
    #[serde(default)]
    pub filters: Vec<FilterSpec>,
}

/// `[[pages]]`: pages under `path` (a page path; `""` for all) need
/// `permission`, one of `[permissions]`, or, with `signed_in = true`,
/// only a signed-in account with a main (as AA's `login_required` views,
/// such as applying to a corporation). The longest matching path wins.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PageRule {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub signed_in: bool,
    /// Every view of a page under this rule is written to Tether's audit
    /// log (`plugin.page_view`: who, which page), for pages showing
    /// private data such as mail.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub audit: bool,
}

/// `[[navigation]]`: a sidebar link to one of the plugin's pages.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NavEntry {
    pub label: String,
    /// A page path; `""` for the plugin's main page.
    pub path: String,
    /// The sidebar section it goes in by default, one of [`NAV_SECTIONS`];
    /// `apps` when left out. Admins can move it on the Menu page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub section: Option<String>,
}

/// `[[views]]`, `[action]` and `[[manage]]`: one of the app's pages by its
/// label.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PageLink {
    pub label: String,
    /// A page path; `""` for the app's main page.
    pub path: String,
}

/// At most this many views, and this many manage pages.
pub const MAX_VIEWS: usize = 8;

/// The sidebar's default sections a `[[navigation]]` entry can name, in the
/// order the sidebar shows them.
pub const NAV_SECTIONS: &[&str] = &[
    "account",
    "fleet",
    "industry",
    "corporation",
    "apps",
    "admin",
];

/// Where a `[[navigation]]` entry goes when it names no section.
pub const DEFAULT_NAV_SECTION: &str = "apps";

impl NavEntry {
    /// Its section: one of [`NAV_SECTIONS`] (checked when parsed).
    pub fn section(&self) -> &'static str {
        self.section
            .as_deref()
            .and_then(|s| NAV_SECTIONS.iter().copied().find(|known| *known == s))
            .unwrap_or(DEFAULT_NAV_SECTION)
    }
}

/// `[[widgets]]`, as packages built before the Dashboard became the
/// character audit declare them: read, and ignored.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Widget {
    pub title: String,
    /// A page path; `""` for the plugin's main page.
    pub path: String,
}

/// The permissions renamed going from `from` to `to` (an upgrade, or a
/// rollback going back over a rename), as (old name, new name) without the
/// `plugin.<id>.` prefix: each named in `to`'s `[renamed_permissions]`, or
/// the reverse of one in `from`'s. Only pairs where the old name is
/// declared by `from` and not by `to`, and the new one the other way
/// round. With no `from` (unknown), `to`'s renames as they are.
pub fn permission_renames(from: Option<&Manifest>, to: &Manifest) -> Vec<(String, String)> {
    let mut pairs: Vec<(String, String)> = to
        .renamed_permissions
        .iter()
        .map(|(old, new)| (old.clone(), new.clone()))
        .collect();
    if let Some(from) = from {
        pairs.extend(
            from.renamed_permissions
                .iter()
                .map(|(old, new)| (new.clone(), old.clone())),
        );
    }
    let mut out: Vec<(String, String)> = Vec::new();
    for (old, new) in pairs {
        let fits = to.permissions.contains_key(&new)
            && !to.permissions.contains_key(&old)
            && from.is_none_or(|f| {
                f.permissions.contains_key(&old) && !f.permissions.contains_key(&new)
            });
        // A rename mustn't hand adding owners to everyone who held a
        // permission that didn't: grants onto one of `to`'s owner
        // permissions move only from one of `from`'s (unknown `from`: as
        // `to` would count it). Otherwise the old grants just go.
        let owners_before = match from {
            Some(f) => f.owner_permissions(),
            None => to.owner_permissions_by_prefix(),
        };
        let hands_owners = to.owner_permissions().contains(&new.as_str())
            && !owners_before.contains(&old.as_str());
        if fits && !hands_owners && !out.iter().any(|(o, n)| *o == old || *n == new) {
            out.push((old, new));
        }
    }
    out
}

/// Who may open a page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PageAccess {
    /// No rule covers it: app admins (core's `admin.plugins`) only.
    Admins,
    /// Any signed-in account with a main (`signed_in = true`).
    SignedIn,
    /// Holders of this permission (its full name, `plugin.<id>.<name>`).
    Permission(String),
}

impl Manifest {
    /// The permissions that add owners: those `owner_permissions` names
    /// (for AA names such as aa-contacts' `manage_alliance_contacts`), else
    /// the `add_…` ones, as AA's `add_refinery_owner` and aa-afat's
    /// `add_fatlink`. In AA only these add owners, not an app's general
    /// management permission.
    pub fn owner_permissions(&self) -> Vec<&str> {
        match &self.capabilities.esi.owner_permissions {
            Some(named) => named.iter().map(String::as_str).collect(),
            None => self.owner_permissions_by_prefix(),
        }
    }

    /// The `add_…` permissions: the owner permissions of a manifest that
    /// names none.
    fn owner_permissions_by_prefix(&self) -> Vec<&str> {
        self.permissions
            .keys()
            .map(String::as_str)
            .filter(|name| name.starts_with("add_"))
            .collect()
    }

    /// Who may open a page: its rule's permission, any signed-in pilot,
    /// or admins when no rule covers it.
    pub fn page_access(&self, path: &str) -> PageAccess {
        match self.page_rule(path) {
            None => PageAccess::Admins,
            Some(rule) => match &rule.permission {
                Some(permission) => {
                    PageAccess::Permission(format!("plugin.{}.{permission}", self.plugin.id))
                }
                None => PageAccess::SignedIn,
            },
        }
    }

    /// The `[[pages]]` rule that covers a page: the longest matching path.
    pub fn page_rule(&self, path: &str) -> Option<&PageRule> {
        let covers = |rule: &PageRule| {
            rule.path.is_empty()
                || path == rule.path
                || path
                    .strip_prefix(rule.path.as_str())
                    .is_some_and(|rest| rest.starts_with('/'))
        };
        self.pages
            .iter()
            .filter(|rule| covers(rule))
            .max_by_key(|rule| rule.path.len())
    }

    /// Whether views of a page are audited (its rule says `audit = true`).
    pub fn page_audited(&self, path: &str) -> bool {
        self.page_rule(path).is_some_and(|rule| rule.audit)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    /// e.g. `acme.mining-ledger`.
    pub id: String,
    pub name: String,
    pub version: String,
    pub host_api: String,
    pub description: Option<String>,
    /// `https://github.com/<owner>/<repo>`.
    pub repository: Option<String>,
    /// Its icon in the sidebar and its pages' headers: one of [`ICONS`].
    /// Left out, a generic one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
}

/// The icons an app may choose (`[plugin] icon`), all in Tether's own set
/// (`templates/icons.html`), so an app never ships a picture.
pub const ICONS: &[&str] = &[
    "activity",
    "blueprint",
    "book",
    "box",
    "chart",
    "citadel",
    "clipboard",
    "clock",
    "contract",
    "crosshair",
    "flag",
    "globe",
    "hexagon",
    "life-buoy",
    "megaphone",
    "moon",
    "package",
    "pin",
    "radio",
    "scan-user",
    "scroll",
    "shield",
    "users",
];

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Publisher {
    /// The minisign public key (the base64 line of the `.pub` file).
    pub key: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Capabilities {
    #[serde(default)]
    pub esi: EsiScopes,
    /// A schema of its own in Postgres.
    #[serde(default)]
    pub storage: bool,
    /// Discord actions, e.g. `send_message`.
    #[serde(default)]
    pub discord: Vec<String>,
    #[serde(default)]
    pub schedules: Vec<Schedule>,
    /// Exact HTTPS hosts it may call, e.g. `janice.e-351.com`.
    #[serde(default)]
    pub http: Vec<String>,
    /// Named secrets the admin enters at install (e.g. an API key). The
    /// host adds each to requests to its one host, in its one header; the
    /// plugin never sees them.
    #[serde(default)]
    pub secrets: BTreeMap<String, SecretSpec>,
    /// Shared timers (aa-structures feeding the timerboard): `publish` or
    /// `read`.
    #[serde(default)]
    pub timers: Option<TimersAccess>,
    /// Shared doctrines (allianceauth-fittings' in aa-fleetpings and
    /// aa-fat): `publish` or `read`.
    #[serde(default)]
    pub doctrines: Option<TimersAccess>,
    /// Files for download (aa-memberaudit's data exports), built from
    /// rows the app hands over and served by the host.
    #[serde(default)]
    pub downloads: bool,
    /// Notices in Tether's notifications (the bell) to pilots who use the
    /// app, as AA apps `notify`.
    #[serde(default)]
    pub notify: bool,
    /// The viewer's groups and the groups to offer them (`identity.groups`,
    /// `identity.all-groups`), to limit things to groups.
    #[serde(default)]
    pub groups: bool,
}

/// Publishing or reading something apps share (timers, doctrines).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TimersAccess {
    Publish,
    Read,
}

/// `[[filters]]`: a Secure Groups filter the plugin offers.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FilterSpec {
    pub name: String,
    /// What it asks, e.g. "Has the skills in a skill set".
    pub label: String,
    /// How characters make an account: `any` passes if one does; `sum`
    /// adds values up and needs at least an admin-chosen total.
    pub combine: Combine,
    #[serde(default)]
    pub fields: Vec<FilterField>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Combine {
    Any,
    Sum,
}

/// A setting an admin gives the filter.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FilterField {
    pub name: String,
    pub label: String,
    pub kind: FieldKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum FieldKind {
    Text,
    Number,
}

/// Where a secret goes: `[capabilities.secrets.janice_api_key]` with
/// `host = "janice.e-351.com"`, `header = "X-ApiKey"`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SecretSpec {
    /// One of `capabilities.http`.
    pub host: String,
    pub header: String,
    /// Put before the value, e.g. `Bearer ` for `Authorization`.
    pub prefix: Option<String>,
}

/// Headers a secret can't be sent in: they frame the request or carry
/// someone else's credentials.
const RESERVED_HEADERS: &[&str] = &[
    "host",
    "cookie",
    "content-length",
    "content-type",
    "transfer-encoding",
    "connection",
    "upgrade",
    "te",
    "trailer",
    "expect",
    "proxy-authorization",
    "keep-alive",
];

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EsiScopes {
    /// Scopes read from pilots' own characters: holders of one of the
    /// plugin's permissions register characters for it, granting these.
    #[serde(default)]
    pub user: Vec<String>,
    /// Scopes linked once by characters an admin designates (such as a
    /// Station Manager for corp mining data).
    #[serde(default)]
    pub data_source: Vec<String>,
    /// Which of the plugin's permissions add data sources (AA's names for
    /// them, such as aa-contacts' `manage_alliance_contacts`). Left out:
    /// its `add_…` ones, as AA's `add_structure_owner`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_permissions: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Schedule {
    pub name: String,
    /// `30m`, `6h`, `1d`: at least 5 minutes.
    pub every: String,
}

impl Schedule {
    pub fn interval(&self) -> Result<Duration, ManifestError> {
        parse_every(&self.every)
    }
}

/// `send_message` posts to assigned channels; `mention_groups` (with it)
/// lets those messages ping the Discord roles Tether gives groups.
pub const DISCORD_ACTIONS: &[&str] = &["send_message", "mention_groups"];

impl Manifest {
    /// Parses and checks `plugin.toml`. Error text is bounded and has no
    /// control characters, but quotes the plugin: escape it when shown.
    pub fn parse(text: &str) -> Result<Self, ManifestError> {
        // The parser's message can quote plugin-chosen keys: keep it
        // bounded and free of control and formatting characters.
        let manifest: Manifest =
            toml::from_str(text).map_err(|e| bad(crate::host::printable(e.message(), 300)))?;
        manifest.check()?;
        Ok(manifest)
    }

    fn check(&self) -> Result<(), ManifestError> {
        let p = &self.plugin;
        check_id(&p.id)?;
        check_text("plugin.name", &p.name, 60, true)?;
        if let Some(d) = &p.description {
            check_text("plugin.description", d, 300, false)?;
        }
        if parse_version(&p.version).is_none() {
            return Err(bad(format!(
                "plugin.version {:?} isn't a version like 1.2.3",
                p.version
            )));
        }
        if p.host_api != HOST_API {
            return Err(bad(format!(
                "plugin.host_api is {:?}; this Tether speaks host API {HOST_API:?}",
                p.host_api
            )));
        }
        if let Some(repo) = &p.repository {
            github_repo(repo).ok_or_else(|| {
                bad("plugin.repository must be https://github.com/<owner>/<repo>")
            })?;
        }
        if let Some(icon) = &p.icon
            && !ICONS.contains(&icon.as_str())
        {
            return Err(bad(format!(
                "plugin.icon {icon:?} isn't one of Tether's icons: {}",
                ICONS.join(", ")
            )));
        }
        if let Some(publisher) = &self.publisher {
            check_key(&publisher.key)?;
        }

        let c = &self.capabilities;
        check_list("capabilities.esi.user", &c.esi.user, 50, check_scope)?;
        check_list(
            "capabilities.esi.data_source",
            &c.esi.data_source,
            50,
            check_scope,
        )?;
        if let Some(owners) = &c.esi.owner_permissions {
            if c.esi.data_source.is_empty() {
                return Err(bad(
                    "capabilities.esi.owner_permissions needs data_source scopes to add data sources for",
                ));
            }
            if owners.is_empty() || owners.len() > 10 {
                return Err(bad(
                    "capabilities.esi.owner_permissions names 1 to 10 permissions",
                ));
            }
            for name in owners {
                if !self.permissions.contains_key(name) {
                    return Err(bad(format!(
                        "capabilities.esi.owner_permissions names {name:?}, which [permissions] doesn't declare"
                    )));
                }
            }
        }
        check_list("capabilities.discord", &c.discord, 5, |a| {
            if DISCORD_ACTIONS.contains(&a) {
                Ok(())
            } else {
                Err(bad(format!(
                    "capabilities.discord: {a:?} isn't one of {}",
                    DISCORD_ACTIONS.join(", ")
                )))
            }
        })?;
        if c.discord.iter().any(|a| a == "mention_groups")
            && !c.discord.iter().any(|a| a == "send_message")
        {
            return Err(bad(
                "capabilities.discord: mention_groups needs send_message",
            ));
        }
        if c.schedules.len() > 20 {
            return Err(bad("more than 20 schedules"));
        }
        let mut seen = std::collections::BTreeSet::new();
        for schedule in &c.schedules {
            check_name("a schedule name", &schedule.name)?;
            if !seen.insert(schedule.name.as_str()) {
                return Err(bad(format!("schedule {:?} appears twice", schedule.name)));
            }
            schedule.interval()?;
        }
        check_list("capabilities.http", &c.http, 10, check_host)?;
        if c.secrets.len() > 10 {
            return Err(bad("capabilities.secrets has more than 10 entries"));
        }
        let mut targets = std::collections::BTreeSet::new();
        for (name, spec) in &c.secrets {
            check_name("a secret name", name)?;
            if !c.http.contains(&spec.host) {
                return Err(bad(format!(
                    "secret {name}: host {:?} isn't in capabilities.http",
                    spec.host
                )));
            }
            check_header(name, &spec.header)?;
            if !targets.insert((spec.host.as_str(), spec.header.to_ascii_lowercase())) {
                return Err(bad(format!(
                    "secret {name}: another secret already goes in {} to {}",
                    spec.header, spec.host
                )));
            }
            if let Some(prefix) = &spec.prefix
                && (prefix.len() > 20 || !prefix.bytes().all(|b| b == b' ' || b.is_ascii_graphic()))
            {
                return Err(bad(format!(
                    "secret {name}: prefix must be at most 20 printable ASCII characters"
                )));
            }
        }
        if self.permissions.len() > 20 {
            return Err(bad("more than 20 permissions"));
        }
        for (name, description) in &self.permissions {
            check_name("a permission name", name)?;
            check_text("a permission description", description, 120, true)?;
        }
        for (name, note) in &self.permission_notes {
            if !self.permissions.contains_key(name) {
                return Err(bad(format!(
                    "[permission_notes] {name:?} isn't one of [permissions]"
                )));
            }
            check_text("a permission note", note, 400, true)?;
        }
        if self.renamed_permissions.len() > 20 {
            return Err(bad("more than 20 [renamed_permissions]"));
        }
        let mut targets = std::collections::BTreeSet::new();
        for (old, new) in &self.renamed_permissions {
            check_name("a renamed permission", old)?;
            if self.permissions.contains_key(old) {
                return Err(bad(format!(
                    "[renamed_permissions] {old:?} is still in [permissions]"
                )));
            }
            if !self.permissions.contains_key(new) {
                return Err(bad(format!(
                    "[renamed_permissions] {old:?} becomes {new:?}, which [permissions] doesn't declare"
                )));
            }
            // Holding an `add_*` permission or one the manifest names in
            // `owner_permissions` lets an account offer the app data
            // sources (and `manage` everything else): a rename mustn't hand
            // that to everyone who held something else. Counted
            // conservatively for the new name. The old one may have added
            // owners only as an `add_*` (a named one is in [permissions]
            // and the old name isn't); whether it did in the version
            // actually replaced, `permission_renames` checks.
            let named = self.capabilities.esi.owner_permissions.as_deref();
            let offers_new = new == "manage"
                || new.starts_with("add_")
                || named.unwrap_or(&[]).iter().any(|n| n == new);
            let offered_old = old.starts_with("add_");
            if offers_new && !offered_old {
                return Err(bad(format!(
                    "[renamed_permissions] {old:?} can't become {new:?}: a manage or add_ permission starts with nobody holding it"
                )));
            }
            if !targets.insert(new.as_str()) {
                return Err(bad(format!(
                    "[renamed_permissions]: two permissions become {new:?}"
                )));
            }
        }
        if self.pages.len() > 20 {
            return Err(bad("more than 20 [[pages]] rules"));
        }
        let mut paths = std::collections::BTreeSet::new();
        for rule in &self.pages {
            check_page_path("[[pages]] path", &rule.path)?;
            match (&rule.permission, rule.signed_in) {
                (Some(permission), false) => {
                    if !self.permissions.contains_key(permission) {
                        return Err(bad(format!(
                            "[[pages]] {:?} needs permission {permission:?}, which [permissions] doesn't declare",
                            rule.path
                        )));
                    }
                }
                (None, true) => {}
                _ => {
                    return Err(bad(format!(
                        "[[pages]] {:?} needs either a permission or signed_in = true",
                        rule.path
                    )));
                }
            }
            if !paths.insert(rule.path.as_str()) {
                return Err(bad(format!("[[pages]] path {:?} appears twice", rule.path)));
            }
        }
        if self.navigation.len() > 10 {
            return Err(bad("more than 10 [[navigation]] entries"));
        }
        let mut nav_paths = std::collections::HashSet::new();
        for entry in &self.navigation {
            check_text("a navigation label", &entry.label, 40, true)?;
            check_page_path("[[navigation]] path", &entry.path)?;
            if let Some(section) = &entry.section
                && !NAV_SECTIONS.contains(&section.as_str())
            {
                return Err(bad(format!(
                    "[[navigation]] section must be one of {}",
                    NAV_SECTIONS.join(", ")
                )));
            }
            if !nav_paths.insert(entry.path.as_str()) {
                return Err(bad(format!(
                    "[[navigation]] path {:?} appears twice",
                    entry.path
                )));
            }
        }
        self.check_frame()?;
        if self.filters.len() > 10 {
            return Err(bad("more than 10 [[filters]]"));
        }
        let mut filter_names = std::collections::BTreeSet::new();
        for filter in &self.filters {
            check_name("a filter name", &filter.name)?;
            if !filter_names.insert(filter.name.as_str()) {
                return Err(bad(format!("filter {:?} appears twice", filter.name)));
            }
            check_text("a filter label", &filter.label, 80, true)?;
            if filter.fields.len() > 5 {
                return Err(bad(format!("filter {}: more than 5 fields", filter.name)));
            }
            let mut fields = std::collections::BTreeSet::new();
            for field in &filter.fields {
                check_name("a filter field name", &field.name)?;
                if !fields.insert(field.name.as_str()) {
                    return Err(bad(format!(
                        "filter {}: field {:?} appears twice",
                        filter.name, field.name
                    )));
                }
                check_text("a filter field label", &field.label, 60, true)?;
            }
        }
        Ok(())
    }
}

/// Longest plugin id: `plugin_<id>` names its Postgres schema and role,
/// and Postgres cuts identifiers at 63 bytes.
pub const MAX_ID: usize = 50;

/// `acme.mining-ledger`: 3 to [`MAX_ID`] characters, lowercase letters, digits and
/// single `.`, `-` or `_` between them, starting with a letter. The same
/// characters link paths allow, so an id is always safe in a URL.
pub fn check_id(id: &str) -> Result<(), ManifestError> {
    let bytes = id.as_bytes();
    let separator = |b: u8| matches!(b, b'.' | b'-' | b'_');
    let ok = (3..=MAX_ID).contains(&id.len())
        && bytes[0].is_ascii_lowercase()
        && bytes
            .iter()
            .all(|&b| b.is_ascii_lowercase() || b.is_ascii_digit() || separator(b))
        && !separator(bytes[bytes.len() - 1])
        && !bytes.windows(2).any(|w| separator(w[0]) && separator(w[1]));
    if ok {
        Ok(())
    } else {
        Err(bad(format!(
            "plugin.id {id:?} must be 3-{MAX_ID} lowercase letters, digits and single . - _, starting with a letter"
        )))
    }
}

/// A minisign public key: exactly the base64 line of a `.pub` file, so a
/// pinned key compares as a string.
pub fn check_key(key: &str) -> Result<(), ManifestError> {
    let base64 = |b: u8| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'=');
    // `RWQ` is base64 for `Ed`, the algorithm minisign keys carry; the
    // parser also takes `ED`, which would give one key two spellings.
    if key.len() > 100
        || !key.starts_with("RWQ")
        || !key.bytes().all(base64)
        || minisign_verify::PublicKey::from_base64(key).is_err()
    {
        return Err(bad("publisher.key isn't a minisign public key"));
    }
    Ok(())
}

/// A page path as plugins write them: what link paths allow, outside
/// `downloads/`, where Tether serves the app's downloads.
impl Manifest {
    /// An app with pages to open (any `[[pages]]` rule or `[[navigation]]`
    /// entry) and no `[[views]]`: refused when installed or upgraded, as
    /// Tether draws every app's frame. One stored before still loads,
    /// drawn without a views bar.
    pub fn needs_views(&self) -> bool {
        self.views.is_empty() && (!self.pages.is_empty() || !self.navigation.is_empty())
    }

    /// A page rule, sidebar entry, view, Manage page or action at or under
    /// one of Tether's own pages (`under_host_page`), or a link wearing
    /// one's label: refused when installed or upgraded, as Tether's page
    /// would be the one shown, or a look-alike beside it.
    pub fn claims_host_page(&self) -> bool {
        let links = self.views.iter().chain(&self.manage).chain(&self.action);
        self.pages
            .iter()
            .map(|r| r.path.as_str())
            .chain(self.navigation.iter().map(|n| n.path.as_str()))
            .chain(links.clone().map(|l| l.path.as_str()))
            .any(under_host_page)
            || links
                .map(|l| l.label.as_str())
                .chain(self.navigation.iter().map(|n| n.label.as_str()))
                .any(|label| HOST_LABELS.contains(&label.trim().to_lowercase().as_str()))
    }

    /// `[[views]]`, `[action]` and `[[manage]]`: plain labels and page
    /// paths, the first view the main page, no page twice, and no manage
    /// page under `settings` (Tether adds Settings itself).
    fn check_frame(&self) -> Result<(), ManifestError> {
        if self.views.len() > MAX_VIEWS {
            return Err(bad(format!("more than {MAX_VIEWS} [[views]]")));
        }
        if self.manage.len() > MAX_VIEWS {
            return Err(bad(format!("more than {MAX_VIEWS} [[manage]] pages")));
        }
        if let Some(first) = self.views.first()
            && !first.path.is_empty()
        {
            return Err(bad("the first of [[views]] is the main page: path = \"\""));
        }
        if self.views.is_empty() && (self.action.is_some() || !self.manage.is_empty()) {
            return Err(bad("[action] and [[manage]] need [[views]]"));
        }
        let mut seen = std::collections::HashSet::new();
        for (what, link) in self
            .views
            .iter()
            .map(|l| ("[[views]]", l))
            .chain(self.manage.iter().map(|l| ("[[manage]]", l)))
        {
            check_text(&format!("a {what} label"), &link.label, 30, true)?;
            check_page_path(&format!("{what} path"), &link.path)?;
            if !seen.insert(link.path.as_str()) {
                return Err(bad(format!("{what} path {:?} appears twice", link.path)));
            }
        }
        for link in &self.manage {
            if link.path.is_empty() || is_settings(&link.path) {
                return Err(bad(format!(
                    "[[manage]] path {:?}: the main page is a view, and Tether adds Settings itself",
                    link.path
                )));
            }
        }
        if let Some(action) = &self.action {
            check_text("the [action] label", &action.label, 30, true)?;
            check_page_path("[action] path", &action.path)?;
        }
        Ok(())
    }
}

fn check_page_path(what: &str, path: &str) -> Result<(), ManifestError> {
    crate::page::check_link_path(path).map_err(|_| {
        bad(format!(
            "{what} {path:?} isn't a page path (like \"moons/old\")"
        ))
    })?;
    if path.starts_with("downloads/") {
        return Err(bad(format!(
            "{what} {path:?}: downloads/ is where Tether serves your downloads"
        )));
    }
    Ok(())
}

/// Names of permissions, schedules and secrets: `view`, `sync_mining`.
fn check_name(what: &str, name: &str) -> Result<(), ManifestError> {
    let bytes = name.as_bytes();
    let ok = (1..=40).contains(&name.len())
        && bytes[0].is_ascii_lowercase()
        && bytes
            .iter()
            .all(|&b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
    if ok {
        Ok(())
    } else {
        Err(bad(format!(
            "{what} {name:?} must be lowercase letters, digits and _, starting with a letter"
        )))
    }
}

fn check_text(what: &str, text: &str, max: usize, required: bool) -> Result<(), ManifestError> {
    if required && text.trim().is_empty() {
        return Err(bad(format!("{what} is empty")));
    }
    if text.chars().count() > max {
        return Err(bad(format!("{what} is longer than {max} characters")));
    }
    // Admins read these on the approval screen: nothing that hides or
    // reorders text.
    if text
        .chars()
        .any(|c| c.is_control() || crate::host::is_format(c))
    {
        return Err(bad(format!(
            "{what} has control or invisible formatting characters"
        )));
    }
    Ok(())
}

fn check_list(
    what: &str,
    items: &[String],
    max: usize,
    check: impl Fn(&str) -> Result<(), ManifestError>,
) -> Result<(), ManifestError> {
    if items.len() > max {
        return Err(bad(format!("{what} has more than {max} entries")));
    }
    let mut seen = std::collections::BTreeSet::new();
    for item in items {
        if !seen.insert(item.as_str()) {
            return Err(bad(format!("{what}: {item:?} appears twice")));
        }
        check(item)?;
    }
    Ok(())
}

/// `esi-industry.read_corporation_mining.v1`.
fn check_scope(scope: &str) -> Result<(), ManifestError> {
    let parts: Vec<&str> = scope.split('.').collect();
    let ok = scope.len() <= 80
        && parts.len() == 3
        && parts.iter().all(|p| !p.is_empty())
        && parts[0].len() > 4
        && parts[0].starts_with("esi-")
        && parts[2].len() > 1
        && parts[2].starts_with('v')
        && parts[2][1..].bytes().all(|b| b.is_ascii_digit())
        && scope.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'_' | b'.')
        });
    if ok {
        Ok(())
    } else {
        Err(bad(format!("{scope:?} isn't an ESI scope")))
    }
}

/// An exact public hostname: no scheme, port, path, IP address or
/// wildcard.
fn check_host(host: &str) -> Result<(), ManifestError> {
    let labels: Vec<&str> = host.split('.').collect();
    let ok = host.len() <= 253
        && labels.len() >= 2
        && labels.iter().all(|l| {
            (1..=63).contains(&l.len())
                && !l.starts_with('-')
                && !l.ends_with('-')
                && l.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        })
        // A real top-level domain: letters only, or an IDN (`xn--`). Rules
        // out every IPv4 spelling, including `127.0.0.0x1`.
        && labels.last().is_some_and(|tld| {
            tld.bytes().all(|b| b.is_ascii_lowercase()) || tld.starts_with("xn--")
        });
    if ok {
        Ok(())
    } else {
        Err(bad(format!(
            "capabilities.http: {host:?} must be an exact lowercase hostname (no scheme, port, path or IP)"
        )))
    }
}

/// A header name a secret may go in: an HTTP token, and not one that
/// frames the request or carries cookies.
fn check_header(secret: &str, header: &str) -> Result<(), ManifestError> {
    let token = (1..=64).contains(&header.len())
        && header
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b));
    if !token || RESERVED_HEADERS.contains(&header.to_ascii_lowercase().as_str()) {
        return Err(bad(format!(
            "secret {secret}: {header:?} can't carry a secret"
        )));
    }
    Ok(())
}

/// `MAJOR.MINOR.PATCH`, numbers only.
pub fn parse_version(version: &str) -> Option<(u64, u64, u64)> {
    // Plain digits, no leading zeros, so each version has one spelling.
    let number = |part: &str| {
        let plain = !part.is_empty()
            && part.bytes().all(|b| b.is_ascii_digit())
            && (part == "0" || !part.starts_with('0'));
        if plain { part.parse().ok() } else { None }
    };
    let mut parts = version.split('.');
    let version = (
        number(parts.next()?)?,
        number(parts.next()?)?,
        number(parts.next()?)?,
    );
    parts.next().is_none().then_some(version)
}

/// `(owner, repo)` from `https://github.com/<owner>/<repo>`.
pub fn github_repo(url: &str) -> Option<(String, String)> {
    let rest = url.strip_prefix("https://github.com/")?;
    let rest = rest.strip_suffix('/').unwrap_or(rest);
    let (owner, repo) = rest.split_once('/')?;
    let fine = |s: &str| {
        (1..=100).contains(&s.len())
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
            && s != "."
            && s != ".."
    };
    (fine(owner) && fine(repo)).then(|| (owner.to_owned(), repo.to_owned()))
}

/// `30m`, `6h`, `1d`, at least 5 minutes, at most 7 days.
fn parse_every(every: &str) -> Result<Duration, ManifestError> {
    let err = || {
        bad(format!(
            "schedule every {every:?} must be like 30m, 6h or 1d, between 5m and 7d"
        ))
    };
    if !every.is_ascii() || every.len() < 2 || every.len() > 6 {
        return Err(err());
    }
    let (number, unit) = every.split_at(every.len() - 1);
    if !number.bytes().all(|b| b.is_ascii_digit()) {
        return Err(err());
    }
    let n: u64 = number.parse().map_err(|_| err())?;
    let secs = match unit {
        "m" => n.checked_mul(60),
        "h" => n.checked_mul(3600),
        "d" => n.checked_mul(86_400),
        _ => None,
    }
    .ok_or_else(err)?;
    if !(300..=7 * 86_400).contains(&secs) {
        return Err(err());
    }
    Ok(Duration::from_secs(secs))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)] // test code

    use super::*;

    // A throwaway minisign public key (base64 of "Ed", key id, 32 bytes).
    const KEY: &str = "RWQBAgMEBQYHCCo2i9XhGZdgQcPjPRZfEwD/sVbMxhw6zXk1Rv8UJvfO";

    #[test]
    fn renames_go_forward_on_upgrades_and_back_on_rollbacks() {
        let old = Manifest::parse(&manifest(
            "[permissions]\nview = \"See\"\nold = \"Old\"\nmanage = \"Manage\"\n",
        ))
        .unwrap();
        let new = Manifest::parse(&manifest(
            "[permissions]\nextractions_access = \"See\"\nbasic_access = \"Old\"\nmanage = \"Manage\"\n\
             [renamed_permissions]\nview = \"extractions_access\"\nold = \"basic_access\"\n",
        ))
        .unwrap();
        let pair = |a: &str, b: &str| (a.to_owned(), b.to_owned());
        assert_eq!(
            permission_renames(Some(&old), &new),
            vec![
                pair("old", "basic_access"),
                pair("view", "extractions_access")
            ]
        );
        // Rolling back reverses them.
        assert_eq!(
            permission_renames(Some(&new), &old),
            vec![
                pair("basic_access", "old"),
                pair("extractions_access", "view")
            ]
        );
        // Nothing to move between two versions that both have the new names.
        assert!(permission_renames(Some(&new), &new).is_empty());
        assert_eq!(permission_renames(None, &new).len(), 2);
        // Kept in the round trip, and left out when empty.
        let text = serde_json::to_string(&new).unwrap();
        assert!(text.contains("renamed_permissions"), "{text}");
        assert!(!serde_json::to_string(&old).unwrap().contains("renamed"));
    }

    #[test]
    fn renames_never_hand_out_adding_owners() {
        let esi = "[capabilities.esi]\ndata_source = [\"esi-corporations.read_contacts.v1\"]\n\
                   owner_permissions = [\"manage_contacts\"]\n";
        // Onto a named owner permission from one that never added owners:
        // refused outright.
        let onto = Manifest::parse(&manifest(
            "[permissions]\nmanage_contacts = \"x\"\nmanage_contacts2 = \"x\"\n\
             [renamed_permissions]\nnote = \"manage_contacts2\"\n\
             [capabilities.esi]\ndata_source = [\"esi-corporations.read_contacts.v1\"]\n\
             owner_permissions = [\"manage_contacts\", \"manage_contacts2\"]\n",
        ));
        assert!(onto.is_err(), "{onto:?}");
        // Away from one is fine going forward; rolling back over it would
        // move the new name's grants onto the owner permission, so they go.
        let old = Manifest::parse(&manifest(&format!(
            "[permissions]\nmanage_contacts = \"x\"\nnotes = \"x\"\n{esi}"
        )))
        .unwrap();
        let new = Manifest::parse(&manifest(&format!(
            "[permissions]\nmanage_contacts = \"x\"\nview_notes = \"x\"\n\
             [renamed_permissions]\nnotes = \"view_notes\"\n{esi}"
        )))
        .unwrap();
        assert_eq!(permission_renames(Some(&old), &new).len(), 1);
        assert_eq!(permission_renames(Some(&new), &old).len(), 1);
        let sneaky = Manifest::parse(&manifest(
            "[permissions]\nmanage_contacts2 = \"x\"\nview_notes = \"x\"\n\
             [renamed_permissions]\nmanage_contacts = \"view_notes\"\n\
             [capabilities.esi]\ndata_source = [\"esi-corporations.read_contacts.v1\"]\n\
             owner_permissions = [\"manage_contacts2\"]\n",
        ))
        .unwrap();
        let back = Manifest::parse(&manifest(&format!(
            "[permissions]\nmanage_contacts = \"x\"\nmanage_contacts2 = \"x\"\n{esi}"
        )))
        .unwrap();
        // Rolling back from `sneaky` would turn view_notes into
        // manage_contacts, an owner permission view_notes wasn't.
        assert!(permission_renames(Some(&sneaky), &back).is_empty());
        // Forward, an add_ permission may become a named owner permission.
        let was = Manifest::parse(&manifest(
            "[permissions]\nadd_owner = \"x\"\n[capabilities.esi]\n\
             data_source = [\"esi-corporations.read_contacts.v1\"]\n",
        ))
        .unwrap();
        let named = Manifest::parse(&manifest(&format!(
            "[permissions]\nmanage_contacts = \"x\"\n\
             [renamed_permissions]\nadd_owner = \"manage_contacts\"\n{esi}"
        )))
        .unwrap();
        assert_eq!(permission_renames(Some(&was), &named).len(), 1);
        // But not an add_ permission that didn't add owners there.
        let other = Manifest::parse(&manifest(&format!(
            "[permissions]\nadd_owner = \"x\"\nmanage_contacts = \"x\"\n{esi}"
        )))
        .unwrap();
        let renamed = Manifest::parse(&manifest(
            "[permissions]\nmanage_contacts = \"x\"\nmanage_contacts2 = \"x\"\n\
             [renamed_permissions]\nadd_owner = \"manage_contacts2\"\n\
             [capabilities.esi]\ndata_source = [\"esi-corporations.read_contacts.v1\"]\n\
             owner_permissions = [\"manage_contacts\", \"manage_contacts2\"]\n",
        ))
        .unwrap();
        assert!(permission_renames(Some(&other), &renamed).is_empty());
    }

    #[test]
    fn views_action_and_manage_are_checked() {
        let good = Manifest::parse(&manifest(
            "[[views]]\nlabel = \"Overview\"\npath = \"\"\n[[views]]\nlabel = \"Moons\"\npath = \"moons\"\n\
             [action]\nlabel = \"Upload surveys\"\npath = \"upload\"\n\
             [[manage]]\nlabel = \"Ore prices\"\npath = \"prices\"\n",
        ))
        .unwrap();
        assert_eq!(good.views.len(), 2);
        assert_eq!(good.action.as_ref().unwrap().path, "upload");
        assert_eq!(good.manage[0].label, "Ore prices");
        for (why, extra) in [
            (
                "first view",
                "[[views]]\nlabel = \"Moons\"\npath = \"moons\"\n",
            ),
            ("needs views", "[action]\nlabel = \"Go\"\npath = \"go\"\n"),
            (
                "twice",
                "[[views]]\nlabel = \"A\"\npath = \"\"\n[[views]]\nlabel = \"B\"\npath = \"\"\n",
            ),
            (
                "settings",
                "[[views]]\nlabel = \"A\"\npath = \"\"\n[[manage]]\nlabel = \"S\"\npath = \"settings\"\n",
            ),
            (
                "manage twice",
                "[[views]]\nlabel = \"A\"\npath = \"\"\n[[manage]]\nlabel = \"S\"\npath = \"\"\n",
            ),
            ("empty label", "[[views]]\nlabel = \"\"\npath = \"\"\n"),
            (
                "bad path",
                "[[views]]\nlabel = \"A\"\npath = \"\"\n[[views]]\nlabel = \"B\"\npath = \"../x\"\n",
            ),
        ] {
            assert!(Manifest::parse(&manifest(extra)).is_err(), "{why}");
        }
        // An app with pages must declare its views when installed; one
        // stored before still parses, so it loads and can be removed.
        assert!(!good.needs_views());
        for (why, extra) in [
            (
                "pages need views",
                "[permissions]\nview = \"See\"\n\n[[pages]]\npath = \"\"\npermission = \"view\"\n",
            ),
            (
                "navigation needs views",
                "[[navigation]]\nlabel = \"Moons\"\npath = \"moons\"\n",
            ),
        ] {
            let stored = Manifest::parse(&bare(extra)).unwrap();
            assert!(stored.needs_views(), "{why}");
        }
        // Tether's own pages in every app's space, or under them: no
        // app's, whichever way it reaches them.
        assert!(!good.claims_host_page());
        for extra in [
            "[permissions]\nview = \"See\"\n\n[[pages]]\npath = \"activity\"\npermission = \"view\"\n",
            "[[navigation]]\nlabel = \"Sources\"\npath = \"data-sources/mine\"\n",
            "[[views]]\nlabel = \"A\"\npath = \"\"\n[[manage]]\nlabel = \"Log\"\npath = \"activity\"\n",
        ] {
            let claims = Manifest::parse(&bare(extra)).unwrap();
            assert!(claims.claims_host_page(), "{extra}");
        }
        let look_alike = Manifest::parse(&bare(
            "[[views]]\nlabel = \"A\"\npath = \"\"\n[[manage]]\nlabel = \"Data Sources\"\npath = \"mine\"\n",
        ))
        .unwrap();
        assert!(look_alike.claims_host_page());
        let look_alike = Manifest::parse(&bare(
            "[[views]]\nlabel = \"A\"\npath = \"\"\n[[views]]\nlabel = \"activity\"\npath = \"log\"\n",
        ))
        .unwrap();
        assert!(look_alike.claims_host_page());
        let near = Manifest::parse(&bare(
            "[[navigation]]\nlabel = \"Fleet activity\"\npath = \"activity-log\"\n",
        ))
        .unwrap();
        assert!(!near.claims_host_page());
        // An app with no pages (jobs, Discord) declares none: nothing
        // written back.
        let plain = Manifest::parse(&manifest("")).unwrap();
        let text = serde_json::to_string(&plain).unwrap();
        assert!(
            !text.contains("views") && !text.contains("manage"),
            "{text}"
        );
    }

    #[test]
    fn an_icon_is_one_of_tethers() {
        let with = |icon: &str| {
            Manifest::parse(
                &manifest("").replace("repository =", &format!("icon = \"{icon}\"\nrepository =")),
            )
        };
        assert_eq!(with("moon").unwrap().plugin.icon.as_deref(), Some("moon"));
        for refused in ["", "rocket", "https://example.com/i.svg", "Moon"] {
            let err = with(refused).unwrap_err();
            assert!(err.0.contains("plugin.icon"), "{refused}: {err:?}");
        }
        // Left out: none, and none written back.
        let plain = Manifest::parse(&manifest("")).unwrap();
        assert_eq!(plain.plugin.icon, None);
        assert!(!serde_json::to_string(&plain).unwrap().contains("icon"));
    }

    /// A manifest with `extra`, and the main page as its one view when it
    /// has pages and names none (an app with pages must).
    fn manifest(extra: &str) -> String {
        let needs_views = (extra.contains("[[pages]]") || extra.contains("[[navigation]]"))
            && !extra.contains("[[views]]");
        if needs_views {
            bare(&format!(
                "{extra}\n[[views]]\nlabel = \"Overview\"\npath = \"\"\n"
            ))
        } else {
            bare(extra)
        }
    }

    /// A manifest with `extra` and nothing added.
    fn bare(extra: &str) -> String {
        format!(
            r#"
[plugin]
id = "acme.mining-ledger"
name = "Mining ledger"
version = "0.3.1"
host_api = "1"
repository = "https://github.com/example/mining-ledger"

[publisher]
key = "{KEY}"
{extra}"#
        )
    }

    #[test]
    fn a_full_manifest_parses() {
        let m = Manifest::parse(&manifest(
            r#"
[capabilities]
storage = true
discord = ["send_message"]
http = ["janice.e-351.com"]

[capabilities.secrets.janice_api_key]
host = "janice.e-351.com"
header = "X-ApiKey"

[capabilities.esi]
data_source = ["esi-industry.read_corporation_mining.v1"]

[[capabilities.schedules]]
name = "sync_mining"
every = "30m"

[permissions]
view = "View the mining ledger"
manage = "Manage the mining ledger"
"#,
        ))
        .unwrap();
        assert_eq!(m.plugin.id, "acme.mining-ledger");
        assert_eq!(
            m.capabilities.schedules[0].interval().unwrap(),
            Duration::from_secs(1800)
        );
        assert_eq!(m.permissions.len(), 2);
    }

    #[test]
    fn typos_and_bad_values_are_refused() {
        for (extra, needle) in [
            ("[capabilities]\nstorge = true", "unknown field"),
            (
                "[capabilities]\nhttp = [\"https://x.example\"]",
                "exact lowercase hostname",
            ),
            (
                "[capabilities]\nhttp = [\"10.0.0.1\"]",
                "exact lowercase hostname",
            ),
            (
                "[capabilities]\nhttp = [\"x.example:8443\"]",
                "exact lowercase hostname",
            ),
            (
                "[capabilities]\ndiscord = [\"ban_members\"]",
                "isn't one of",
            ),
            (
                "[capabilities]\ndiscord = [\"mention_groups\"]",
                "mention_groups needs send_message",
            ),
            (
                "[capabilities]\nhttp = [\"127.0.0.0x1\"]",
                "exact lowercase hostname",
            ),
            (
                "[capabilities]\nhttp = [\"a.example\"]\n\
                 [capabilities.secrets.key]\nhost = \"b.example\"\nheader = \"X-Key\"",
                "isn't in capabilities.http",
            ),
            (
                "[capabilities]\nhttp = [\"a.example\"]\n\
                 [capabilities.secrets.key]\nhost = \"a.example\"\nheader = \"Cookie\"",
                "can't carry a secret",
            ),
            (
                "[capabilities]\nhttp = [\"a.example\"]\n\
                 [capabilities.secrets.key]\nhost = \"a.example\"\nheader = \"X-Key: y\"",
                "can't carry a secret",
            ),
            ("[capabilities]\nsecrets = [\"key\"]", "invalid type"),
            (
                "[capabilities.esi]\nuser = [\"esi-..v1\"]",
                "isn't an ESI scope",
            ),
            (
                "[[capabilities.schedules]]\nname = \"x\"\nevery = \"5\u{e9}\"",
                "between 5m and 7d",
            ),
            (
                "[[capabilities.schedules]]\nname = \"x\"\nevery = \"\u{e9}\"",
                "between 5m and 7d",
            ),
            (
                "[[capabilities.schedules]]\nname = \"x\"\nevery = \"+30m\"",
                "between 5m and 7d",
            ),
            (
                "[permissions]\nview = \"View \u{202e}reggel\"",
                "invisible formatting",
            ),
            (
                "[capabilities.esi]\nuser = [\"read everything\"]",
                "isn't an ESI scope",
            ),
            (
                "[[capabilities.schedules]]\nname = \"x\"\nevery = \"1m\"",
                "between 5m and 7d",
            ),
            ("[permissions]\nView = \"x\"", "lowercase letters"),
            (
                "[permissions]\nview = \"x\"\n[renamed_permissions]\nold = \"gone\"",
                "doesn't declare",
            ),
            (
                "[permissions]\nview = \"x\"\n[renamed_permissions]\nview = \"view\"",
                "still in [permissions]",
            ),
            (
                "[permissions]\nview = \"x\"\n[renamed_permissions]\na = \"view\"\nb = \"view\"",
                "two permissions become",
            ),
            (
                "[permissions]\nview = \"x\"\n[renamed_permissions]\nOld = \"view\"",
                "lowercase letters",
            ),
            (
                "[permissions]\nadd_owner = \"x\"\n[renamed_permissions]\nview = \"add_owner\"",
                "starts with nobody holding it",
            ),
            (
                "[permissions]\nview = \"x\"\n[permission_notes]\nmanage = \"For officers\"",
                "isn't one of [permissions]",
            ),
            (
                "[permissions]\nview = \"x\"\n[permission_notes]\nview = \"\"",
                "a permission note is empty",
            ),
            ("[capabilities]\n\"a\\nb\\u001b[31m\" = 1", "unknown field"),
        ] {
            let err = Manifest::parse(&manifest(extra)).unwrap_err();
            assert!(err.0.contains(needle), "{extra}: {err}");
            // Plugin-chosen text never comes back raw.
            assert!(!err.0.contains(['\n', '\u{1b}']), "{extra}: {err:?}");
        }
    }

    #[test]
    fn identity_rules() {
        for id in ["abc", "acme.mining-ledger", "a1_b2"] {
            assert!(check_id(id).is_ok(), "{id}");
        }
        for id in [
            "ab",
            "Acme.x",
            "1abc",
            "a..b",
            "abc.",
            "a/b",
            "a b",
            "../x",
            &"a".repeat(65),
        ] {
            assert!(check_id(id).is_err(), "{id}");
        }
        let wrong_api = manifest("").replace("host_api = \"1\"", "host_api = \"2\"");
        assert!(
            Manifest::parse(&wrong_api)
                .unwrap_err()
                .0
                .contains("host API")
        );
        for version in ["0.3", "+0.3.1", "00.3.1", "0.3.1-beta", "0.3.1.0", ""] {
            let bad_version = manifest("").replace("0.3.1", version);
            assert!(Manifest::parse(&bad_version).is_err(), "{version}");
        }
        assert_eq!(parse_version("10.0.3"), Some((10, 0, 3)));
        assert!(check_id(&"a".repeat(MAX_ID)).is_ok());
        assert!(check_id(&"a".repeat(MAX_ID + 1)).is_err());
        let bad_repo = manifest("").replace("github.com/example", "gitlab.com/example");
        assert!(Manifest::parse(&bad_repo).is_err());
        let bad_key = manifest("").replace(KEY, "not-a-key");
        assert!(
            Manifest::parse(&bad_key)
                .unwrap_err()
                .0
                .contains("minisign")
        );
    }

    #[test]
    fn audited_pages_follow_their_rule() {
        let m = Manifest::parse(&manifest(
            "[permissions]\nview = \"See\"\n\n\
             [[pages]]\npath = \"\"\npermission = \"view\"\n\n\
             [[pages]]\npath = \"mail\"\npermission = \"view\"\naudit = true\n",
        ))
        .unwrap();
        assert!(!m.page_audited(""));
        assert!(!m.page_audited("mailbox"));
        assert!(m.page_audited("mail"));
        assert!(m.page_audited("mail/123"));
        // Left out, it isn't written back either.
        let written = serde_json::to_string(&m).unwrap();
        assert_eq!(written.matches("\"audit\"").count(), 1, "{written}");
        assert!(
            Manifest::parse(&manifest(
                "[permissions]\nview = \"See\"\n[[pages]]\npath = \"\"\npermission = \"view\"\naudit = \"yes\"\n",
            ))
            .is_err()
        );
    }

    #[test]
    fn page_rules_pick_the_longest_prefix() {
        let m = Manifest::parse(&manifest(
            "[permissions]\nview = \"See\"\nmanage = \"Manage\"\n\n\
             [[pages]]\npath = \"\"\npermission = \"view\"\n\n\
             [[pages]]\npath = \"admin\"\npermission = \"manage\"\n\n\
             [[navigation]]\nlabel = \"Moons\"\npath = \"\"\n",
        ))
        .unwrap();
        let view = PageAccess::Permission("plugin.acme.mining-ledger.view".to_owned());
        let manage = PageAccess::Permission("plugin.acme.mining-ledger.manage".to_owned());
        assert_eq!(m.page_access(""), view);
        assert_eq!(m.page_access("moons/1"), view);
        assert_eq!(m.page_access("admin"), manage);
        assert_eq!(m.page_access("admin/keys"), manage);
        // A prefix is whole segments only.
        assert_eq!(m.page_access("administrator"), view);

        let admins_only = Manifest::parse(&manifest(
            "[permissions]\nmanage = \"Manage\"\n\n[[pages]]\npath = \"admin\"\npermission = \"manage\"\n",
        ))
        .unwrap();
        assert_eq!(admins_only.page_access(""), PageAccess::Admins);
        assert_eq!(admins_only.page_access("other"), PageAccess::Admins);

        // Pages open to any signed-in pilot (AA's login_required views).
        let open = Manifest::parse(&manifest(
            "[permissions]\nview = \"See\"\n\n\
             [[pages]]\npath = \"\"\npermission = \"view\"\n\n\
             [[pages]]\npath = \"apply\"\nsigned_in = true\n",
        ))
        .unwrap();
        assert_eq!(open.page_access("apply/3"), PageAccess::SignedIn);
        assert_eq!(
            open.page_access("mine"),
            PageAccess::Permission("plugin.acme.mining-ledger.view".to_owned())
        );
        // Left out, it isn't written back either.
        let written = serde_json::to_string(&m).unwrap();
        assert!(!written.contains("signed_in"), "{written}");

        for bad in [
            "[[pages]]\npath = \"\"\npermission = \"undeclared\"\n",
            "[[pages]]\npath = \"\"\n",
            "[permissions]\nview = \"x\"\n[[pages]]\npath = \"\"\npermission = \"view\"\nsigned_in = true\n",
            "[[pages]]\npath = \"\"\nsigned_in = false\n",
            "[permissions]\nview = \"x\"\n[[pages]]\npath = \"/abs\"\npermission = \"view\"\n",
            "[[navigation]]\nlabel = \"\"\npath = \"\"\n",
            "[[navigation]]\nlabel = \"Go\"\npath = \"../core\"\n",
            "[[navigation]]\nlabel = \"Files\"\npath = \"downloads/wallet\"\n",
            "[permissions]\nview = \"x\"\n[capabilities.esi]\nowner_permissions = [\"view\"]\n",
            "[permissions]\nview = \"x\"\n[capabilities.esi]\ndata_source = [\"esi-corporations.read_contacts.v1\"]\nowner_permissions = [\"nope\"]\n",
            "[[navigation]]\nlabel = \"A\"\npath = \"\"\n[[navigation]]\nlabel = \"B\"\npath = \"\"\n",
            "[[navigation]]\nlabel = \"Go\"\npath = \"\"\nsection = \"mining\"\n",
            "[[navigation]]\nlabel = \"Go\"\npath = \"\"\nsection = \"Fleet\"\n",
        ] {
            assert!(Manifest::parse(&manifest(bad)).is_err(), "{bad}");
        }
        // Widgets are gone from the Dashboard; packages declaring them
        // still load.
        assert!(
            Manifest::parse(&manifest(
                "[[widgets]]\ntitle = \"Ore\"\npath = \"ledger\"\n",
            ))
            .is_ok()
        );
    }

    #[test]
    fn navigation_goes_in_apps_unless_it_names_a_section() {
        let m = Manifest::parse(&manifest(
            "[[navigation]]\nlabel = \"Moons\"\npath = \"\"\n\n\
             [[navigation]]\nlabel = \"Ledger\"\npath = \"ledger\"\nsection = \"industry\"\n",
        ))
        .unwrap();
        assert_eq!(m.navigation[0].section(), "apps");
        assert_eq!(m.navigation[1].section(), "industry");
    }

    #[test]
    fn github_repos_parse_strictly() {
        assert_eq!(
            github_repo("https://github.com/infktd/tether"),
            Some(("infktd".to_owned(), "tether".to_owned()))
        );
        for bad in [
            "http://github.com/a/b",
            "https://github.com/a",
            "https://github.com/a/b/c",
            "https://github.com.evil.example/a/b",
            "https://github.com/../b",
        ] {
            assert!(github_repo(bad).is_none(), "{bad}");
        }
    }
}
