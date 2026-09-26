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
    pub publisher: Publisher,
    #[serde(default)]
    pub capabilities: Capabilities,
    /// Permission name to description, e.g. `view = "View the mining
    /// ledger"`. Granted like core ones, as `plugin.<id>.<name>`.
    #[serde(default)]
    pub permissions: BTreeMap<String, String>,
    /// Who may open which pages: a path prefix and the permission it needs.
    /// A page no rule covers is for admins only (`admin.plugins`).
    #[serde(default)]
    pub pages: Vec<PageRule>,
    /// Sidebar entries, shown to whoever may open their page.
    #[serde(default)]
    pub navigation: Vec<NavEntry>,
    /// Dashboard widgets, shown to whoever may open their page.
    #[serde(default)]
    pub widgets: Vec<Widget>,
    /// Secure Groups filters it offers.
    #[serde(default)]
    pub filters: Vec<FilterSpec>,
}

/// `[[pages]]`: pages under `path` (a page path; `""` for all) need
/// `permission`, one of `[permissions]`. The longest matching path wins.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PageRule {
    pub path: String,
    pub permission: String,
}

/// `[[navigation]]`: a sidebar link to one of the plugin's pages.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NavEntry {
    pub label: String,
    /// A page path; `""` for the plugin's main page.
    pub path: String,
}

/// `[[widgets]]`: a Dashboard card showing one of the plugin's pages (its
/// sections, not its tabs), with a link to the page.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Widget {
    pub title: String,
    /// A page path; `""` for the plugin's main page.
    pub path: String,
}

/// Most widgets a plugin may add.
pub const MAX_WIDGETS: usize = 3;

impl Manifest {
    /// The permission (full name, `plugin.<id>.<name>`) a page needs, or
    /// `None` if no rule covers it: admins only.
    pub fn page_permission(&self, path: &str) -> Option<String> {
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
            .map(|rule| format!("plugin.{}.{}", self.plugin.id, rule.permission))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    /// e.g. `nmu.mining-ledger`.
    pub id: String,
    pub name: String,
    pub version: String,
    pub host_api: String,
    pub description: Option<String>,
    /// `https://github.com/<owner>/<repo>`.
    pub repository: Option<String>,
}

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
}

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
    /// Scopes read from members' own characters. Member requires them,
    /// so every Member character is registered with them.
    #[serde(default)]
    pub user: Vec<String>,
    /// Scopes linked once by characters an admin designates (such as a
    /// Station Manager for corp mining data).
    #[serde(default)]
    pub data_source: Vec<String>,
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

pub const DISCORD_ACTIONS: &[&str] = &["send_message"];

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
        check_key(&self.publisher.key)?;

        let c = &self.capabilities;
        check_list("capabilities.esi.user", &c.esi.user, 50, check_scope)?;
        check_list(
            "capabilities.esi.data_source",
            &c.esi.data_source,
            50,
            check_scope,
        )?;
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
        if self.pages.len() > 20 {
            return Err(bad("more than 20 [[pages]] rules"));
        }
        let mut paths = std::collections::BTreeSet::new();
        for rule in &self.pages {
            check_page_path("[[pages]] path", &rule.path)?;
            if !self.permissions.contains_key(&rule.permission) {
                return Err(bad(format!(
                    "[[pages]] {:?} needs permission {:?}, which [permissions] doesn't declare",
                    rule.path, rule.permission
                )));
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
            if !nav_paths.insert(entry.path.as_str()) {
                return Err(bad(format!(
                    "[[navigation]] path {:?} appears twice",
                    entry.path
                )));
            }
        }
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
        if self.widgets.len() > MAX_WIDGETS {
            return Err(bad(format!("more than {MAX_WIDGETS} [[widgets]]")));
        }
        for widget in &self.widgets {
            check_text("a widget title", &widget.title, 40, true)?;
            check_page_path("[[widgets]] path", &widget.path)?;
        }
        Ok(())
    }
}

/// Longest plugin id: `plugin_<id>` names its Postgres schema and role,
/// and Postgres cuts identifiers at 63 bytes.
pub const MAX_ID: usize = 50;

/// `nmu.mining-ledger`: 3 to [`MAX_ID`] characters, lowercase letters, digits and
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

/// A page path as plugins write them: what link paths allow.
fn check_page_path(what: &str, path: &str) -> Result<(), ManifestError> {
    crate::page::check_link_path(path).map_err(|_| {
        bad(format!(
            "{what} {path:?} isn't a page path (like \"moons/old\")"
        ))
    })
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

    fn manifest(extra: &str) -> String {
        format!(
            r#"
[plugin]
id = "nmu.mining-ledger"
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
        assert_eq!(m.plugin.id, "nmu.mining-ledger");
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
        for id in ["abc", "nmu.mining-ledger", "a1_b2"] {
            assert!(check_id(id).is_ok(), "{id}");
        }
        for id in [
            "ab",
            "Nmu.x",
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
    fn page_rules_pick_the_longest_prefix() {
        let m = Manifest::parse(&manifest(
            "[permissions]\nview = \"See\"\nmanage = \"Manage\"\n\n\
             [[pages]]\npath = \"\"\npermission = \"view\"\n\n\
             [[pages]]\npath = \"admin\"\npermission = \"manage\"\n\n\
             [[navigation]]\nlabel = \"Moons\"\npath = \"\"\n",
        ))
        .unwrap();
        let view = Some("plugin.nmu.mining-ledger.view".to_owned());
        let manage = Some("plugin.nmu.mining-ledger.manage".to_owned());
        assert_eq!(m.page_permission(""), view);
        assert_eq!(m.page_permission("moons/1"), view);
        assert_eq!(m.page_permission("admin"), manage);
        assert_eq!(m.page_permission("admin/keys"), manage);
        // A prefix is whole segments only.
        assert_eq!(m.page_permission("administrator"), view);

        let admins_only = Manifest::parse(&manifest(
            "[permissions]\nmanage = \"Manage\"\n\n[[pages]]\npath = \"admin\"\npermission = \"manage\"\n",
        ))
        .unwrap();
        assert_eq!(admins_only.page_permission(""), None);
        assert_eq!(admins_only.page_permission("other"), None);

        for bad in [
            "[[pages]]\npath = \"\"\npermission = \"undeclared\"\n",
            "[permissions]\nview = \"x\"\n[[pages]]\npath = \"/abs\"\npermission = \"view\"\n",
            "[[navigation]]\nlabel = \"\"\npath = \"\"\n",
            "[[navigation]]\nlabel = \"Go\"\npath = \"../core\"\n",
            "[[navigation]]\nlabel = \"A\"\npath = \"\"\n[[navigation]]\nlabel = \"B\"\npath = \"\"\n",
            "[[widgets]]\ntitle = \"\"\npath = \"\"\n",
            "[[widgets]]\ntitle = \"Ore\"\npath = \"../core\"\n",
            "[[widgets]]\ntitle = \"Ore\"\npath = \"\"\nsize = \"big\"\n",
            &"[[widgets]]\ntitle = \"Ore\"\npath = \"\"\n".repeat(MAX_WIDGETS + 1),
        ] {
            assert!(Manifest::parse(&manifest(bad)).is_err(), "{bad}");
        }
        let widgets = Manifest::parse(&manifest(
            "[[widgets]]\ntitle = \"Ore\"\npath = \"ledger\"\n",
        ))
        .unwrap();
        assert_eq!(widgets.widgets[0].path, "ledger");
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
