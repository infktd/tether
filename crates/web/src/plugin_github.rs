//! Apps from GitHub (F15, F18): installing from a repository's releases,
//! and a daily check for newer versions of apps that name a repository.
//!
//! A repository publishes an app as two assets on a release: the package,
//! `<app id>-<version>.zip`, and its minisign signature,
//! `<app id>-<version>.zip.minisig` (what `scripts/package-plugin.sh`
//! makes). Tether reads the repository's recent releases (not drafts or
//! pre-releases), takes the newest version of the app, downloads both
//! through the allow list (GitHub's hosts only) and hands them to the same
//! checks as an upload: signature, pinned key, component. Nothing installs
//! or upgrades until an admin approves the review.
//!
//! The update check follows the same switch as the platform's (admins can
//! turn GitHub off), and never runs before an owner exists.

use std::collections::BTreeSet;
use std::time::Duration;

use serde::Deserialize;
use tether_db::PgPool;
use tether_db::accounts::AccountId;
use tether_db::plugin_sources;
use tether_jobs::schedule::ScheduleSpec;
use tether_jobs::{JobError, Registry};
use tether_net::{Allowlist, Outbound, OutboundError, Redirects};
use tether_plugins::manifest;
use tether_plugins::package::{MAX_PACKAGE_BYTES, MAX_SIGNATURE_BYTES};

use crate::error::AppError;

pub const CHECK_JOB: &str = "plugins.update_check";
/// Releases looked at, newest first: enough for a repository publishing
/// several apps.
const RELEASES: u32 = 30;
/// A page of releases with their assets.
const MAX_RELEASES_BODY: usize = 4 * 1024 * 1024;
/// A package download may take this long...
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(120);
/// ...and a call to the API this long: a stalled GitHub mustn't hold the
/// one upload slot for minutes.
const API_TIMEOUT: Duration = Duration::from_secs(15);
/// GitHub's hosts that downloads may be redirected between, and nothing
/// else (not Tether's other destinations). All are in
/// [`tether_net::ALLOWED`].
const DOWNLOAD_HOSTS: &[&str] = &[
    "github.com",
    "release-assets.githubusercontent.com",
    "objects.githubusercontent.com",
];

pub fn schedules() -> Vec<ScheduleSpec> {
    vec![ScheduleSpec::new(
        "plugins.update_check",
        CHECK_JOB,
        Duration::from_secs(24 * 60 * 60),
    )]
}

/// GitHub's API and download hosts. Tests point both at a mock.
#[derive(Debug, Clone)]
pub struct GitHub {
    /// The API: no redirects, a short timeout.
    api: Outbound,
    /// Downloads: redirects only between GitHub's hosts.
    downloads: Outbound,
    api_base: String,
    web_base: String,
}

impl GitHub {
    /// Through the allow list, with a User-Agent that names the software
    /// but not this instance.
    pub fn production() -> Result<Self, OutboundError> {
        let agent = format!("tether/{}", crate::updates::CURRENT);
        Ok(Self::new(
            Outbound::new(Allowlist::only(["api.github.com"]), &agent, API_TIMEOUT)?,
            Outbound::with_redirects(
                Allowlist::only(DOWNLOAD_HOSTS.iter().copied()),
                &agent,
                DOWNLOAD_TIMEOUT,
                Redirects::WithinAllowlist,
            )?,
            "https://api.github.com",
            "https://github.com",
        ))
    }

    pub fn new(api: Outbound, downloads: Outbound, api_base: &str, web_base: &str) -> Self {
        Self {
            api,
            downloads,
            api_base: api_base.trim_end_matches('/').to_owned(),
            web_base: web_base.trim_end_matches('/').to_owned(),
        }
    }

    /// `url` if it's a page in `repo`'s releases: checked again on the way
    /// out, whatever wrote it, before it's shown as a link.
    pub fn release_link(&self, repo: &str, url: &str) -> Option<String> {
        let pages = format!("{}/{repo}/releases/", self.web_base);
        (Repo::parse(repo).is_some()
            && url.starts_with(&pages)
            && !url
                .chars()
                .any(|c| c.is_whitespace() || c.is_control() || matches!(c, '"' | '<' | '>')))
        .then(|| url.to_owned())
    }

    /// The newest version of an app a repository publishes: of `app` if
    /// given, else of the only app it publishes.
    pub async fn newest(&self, repo: &Repo, app: Option<&str>) -> Result<Found, String> {
        let url = format!(
            "{}/repos/{}/releases?per_page={RELEASES}",
            self.api_base, repo.0
        );
        let response = self
            .api
            .get(&url)
            .map_err(|e| e.to_string())?
            .header("accept", "application/vnd.github+json")
            .header("x-github-api-version", "2022-11-28")
            .send()
            .await
            .map_err(|e| format!("GitHub couldn't be reached ({})", e.without_url()))?;
        match response.status().as_u16() {
            200 => {}
            404 => return Err(format!("GitHub has no public repository {repo}")),
            403 | 429 => {
                return Err(
                    "GitHub is limiting requests from this server; try again later".to_owned(),
                );
            }
            status => return Err(format!("GitHub answered HTTP {status}")),
        }
        let body = read_limited(response, MAX_RELEASES_BODY).await?;
        let releases: Vec<Release> = serde_json::from_slice(&body)
            .map_err(|_| "GitHub's list of releases couldn't be read".to_owned())?;
        find(&releases, repo, &self.web_base, app)
    }

    /// The package and its signature.
    pub async fn download(&self, found: &Found) -> Result<(Vec<u8>, String), String> {
        if found.package.size > MAX_PACKAGE_BYTES as u64 {
            return Err(format!(
                "{} is larger than an app package may be ({} MiB)",
                found.package.name,
                MAX_PACKAGE_BYTES / (1024 * 1024)
            ));
        }
        let package = self
            .get_asset(&found.package.browser_download_url, MAX_PACKAGE_BYTES)
            .await
            .map_err(|e| format!("{} couldn't be downloaded: {e}", found.package.name))?;
        let signature = self
            .get_asset(&found.signature.browser_download_url, MAX_SIGNATURE_BYTES)
            .await
            .map_err(|e| format!("{} couldn't be downloaded: {e}", found.signature.name))?;
        let signature = String::from_utf8(signature)
            .map_err(|_| format!("{} isn't a minisign signature", found.signature.name))?;
        Ok((package, signature))
    }

    async fn get_asset(&self, url: &str, max: usize) -> Result<Vec<u8>, String> {
        let response = self
            .downloads
            .get(url)
            .map_err(|e| e.to_string())?
            .header("accept", "application/octet-stream")
            .send()
            .await
            .map_err(|e| e.without_url().to_string())?;
        if !response.status().is_success() {
            return Err(format!(
                "GitHub answered HTTP {}",
                response.status().as_u16()
            ));
        }
        read_limited(response, max).await
    }
}

/// A body, refused once it passes `max` bytes.
async fn read_limited(mut response: tether_net::Response, max: usize) -> Result<Vec<u8>, String> {
    let too_big = || "GitHub sent more than expected".to_owned();
    if response.content_length().is_some_and(|n| n > max as u64) {
        return Err(too_big());
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| e.without_url().to_string())?
    {
        if body.len() + chunk.len() > max {
            return Err(too_big());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// A GitHub repository, `owner/name`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repo(String);

impl Repo {
    /// From `https://github.com/owner/name` (a trailing slash or `.git` is
    /// fine) or `owner/name`, with GitHub's rules for each part.
    pub fn parse(input: &str) -> Option<Self> {
        let s = input.trim();
        let s = ["https://github.com/", "http://github.com/", "github.com/"]
            .iter()
            .find_map(|prefix| s.strip_prefix(prefix))
            .unwrap_or(s);
        let s = s.trim_end_matches('/');
        let s = s.strip_suffix(".git").unwrap_or(s);
        let (owner, name) = s.split_once('/')?;
        let owner_ok = (1..=39).contains(&owner.len())
            && !owner.starts_with('-')
            && owner
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-');
        let name_ok = (1..=100).contains(&name.len())
            && name != "."
            && name != ".."
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
        (owner_ok && name_ok).then(|| Self(format!("{owner}/{name}")))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Repo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Deserialize)]
struct Release {
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    html_url: String,
    #[serde(default)]
    assets: Vec<Asset>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Asset {
    name: String,
    size: u64,
    browser_download_url: String,
}

/// An app package on a release.
#[derive(Debug, Clone)]
pub struct Found {
    pub plugin_id: String,
    pub version: String,
    /// The release's page, when it's in the repository's releases.
    pub release_url: Option<String>,
    package: Asset,
    signature: Asset,
}

/// Whether `url`, once parsed (dot segments resolved), is under
/// `downloads` (`<web base>/<repo>/releases/download/`).
fn in_downloads(url: &str, downloads: &str) -> bool {
    match reqwest::Url::parse(url) {
        Ok(parsed) => {
            parsed.query().is_none()
                && parsed.fragment().is_none()
                && parsed.username().is_empty()
                && parsed.as_str().starts_with(downloads)
        }
        Err(_) => false,
    }
}

/// The newest version of `app` (or of the only app) among `releases`.
fn find(
    releases: &[Release],
    repo: &Repo,
    web_base: &str,
    app: Option<&str>,
) -> Result<Found, String> {
    let downloads = format!("{web_base}/{repo}/releases/download/");
    let pages = format!("{web_base}/{repo}/releases/");
    let mut apps = BTreeSet::new();
    let mut best: Option<((u64, u64, u64), Found)> = None;
    for release in releases.iter().filter(|r| !r.draft && !r.prerelease) {
        for package in &release.assets {
            let Some((id, version)) = package
                .name
                .strip_suffix(".zip")
                .and_then(|stem| stem.rsplit_once('-'))
            else {
                continue;
            };
            let Some(parsed) = manifest::parse_version(version) else {
                continue;
            };
            if manifest::check_id(id).is_err() {
                continue;
            }
            let signature_name = format!("{}.minisig", package.name);
            let Some(signature) = release.assets.iter().find(|a| a.name == signature_name) else {
                continue;
            };
            // Only from this repository's own downloads.
            if !in_downloads(&package.browser_download_url, &downloads)
                || !in_downloads(&signature.browser_download_url, &downloads)
            {
                continue;
            }
            apps.insert(id.to_owned());
            if app.is_some_and(|want| want != id) {
                continue;
            }
            if best.as_ref().is_none_or(|(v, _)| parsed > *v) {
                let release_url = (release.html_url.starts_with(&pages)
                    && !release
                        .html_url
                        .chars()
                        .any(|c| c.is_whitespace() || c == '"'))
                .then(|| release.html_url.clone());
                best = Some((
                    parsed,
                    Found {
                        plugin_id: id.to_owned(),
                        version: version.to_owned(),
                        release_url,
                        package: package.clone(),
                        signature: signature.clone(),
                    },
                ));
            }
        }
    }
    if app.is_none() && apps.len() > 1 {
        let listed: Vec<&str> = apps.iter().map(String::as_str).collect();
        return Err(format!(
            "{repo} publishes several apps ({}): enter the id of the one to install",
            listed.join(", ")
        ));
    }
    best.map(|(_, found)| found).ok_or_else(|| match app {
        Some(id) => format!(
            "No release of {repo} has a package of {id} ({id}-<version>.zip with its .minisig)"
        ),
        None => format!(
            "No release of {repo} has an app package (<app id>-<version>.zip with its .minisig)"
        ),
    })
}

/// Fetches the newest version of an app from GitHub and uploads it for
/// review, recording where it came from. `app` is the app expected (an
/// upgrade, or the id the admin entered); the package must be that app.
/// Refusals are audited like an upload's.
pub async fn fetch(
    state: &crate::AppState,
    actor: AccountId,
    repo: &Repo,
    app: Option<&str>,
    newer_than: Option<&str>,
) -> Result<i64, AppError> {
    let reject = |why: String| async move {
        crate::plugins::reject(state, actor, app, 0, AppError::bad_request(why)).await
    };
    let Some(github) = state.plugins.github() else {
        return Err(reject("Installing from GitHub isn't set up on this server.".to_owned()).await);
    };
    // Before contacting GitHub: the download would be refused anyway.
    if tether_db::plugins::count_uploads(&state.db).await? >= crate::plugins::MAX_PENDING_UPLOADS {
        return Err(reject(
            "Too many uploads are waiting for approval. Approve or discard some first.".to_owned(),
        )
        .await);
    }
    let found = match github.newest(repo, app).await {
        Ok(found) => found,
        Err(why) => return Err(reject(format!("{why}.")).await),
    };
    if let Some(installed) = newer_than {
        let newer = match (
            manifest::parse_version(&found.version),
            manifest::parse_version(installed),
        ) {
            (Some(found), Some(installed)) => found > installed,
            _ => false,
        };
        if !newer {
            return Err(reject(format!(
                "Version {installed} is installed, and the newest {repo} publishes is {}.",
                found.version
            ))
            .await);
        }
    }
    let (package, signature) = match github.download(&found).await {
        Ok(downloaded) => downloaded,
        Err(why) => return Err(reject(format!("{why}.")).await),
    };
    crate::plugins::upload_from(
        state,
        actor,
        package,
        signature,
        Some(crate::plugins::Source {
            repo: repo.clone(),
            plugin_id: found.plugin_id,
            version: found.version,
        }),
    )
    .await
}

/// Sets (or with an empty `input`, clears) the repository an installed
/// app's updates are looked for in, and checks it soon if checks are on.
pub async fn set_source(
    state: &crate::AppState,
    actor: AccountId,
    plugin_id: &str,
    input: &str,
) -> Result<(), AppError> {
    let repo = if input.trim().is_empty() {
        None
    } else {
        Some(Repo::parse(input).ok_or_else(|| {
            AppError::bad_request(
                "That isn't a GitHub repository: use https://github.com/<owner>/<name>.",
            )
        })?)
    };
    let bundled_here = state.plugins.bundled().reserves(plugin_id);
    let mut tx = state.db.begin().await?;
    let Some(origin) = tether_db::plugins::origin(&mut *tx, plugin_id).await? else {
        return Err(AppError::not_found("No app with that id is installed."));
    };
    if repo.is_some() && (bundled_here || origin == tether_db::plugins::Origin::Bundled) {
        return Err(AppError::bad_request(
            "This app comes with Tether: its updates come with Tether's, not from GitHub.",
        ));
    }
    let source = repo.as_ref().map(Repo::as_str);
    if plugin_sources::set_source(&mut *tx, plugin_id, source).await? {
        tether_db::audit::record(
            &mut *tx,
            tether_db::audit::Actor::Account(actor),
            "plugin.source_set",
            Some(&format!("plugin:{plugin_id}")),
            serde_json::json!({ "source": source }),
        )
        .await?;
        if source.is_some() && crate::updates::enabled(&state.db).await? {
            crate::updates::queue_job(&mut tx, CHECK_JOB).await?;
        }
    }
    tx.commit().await?;
    Ok(())
}

/// Looks for newer versions of every app that names a repository, and
/// records what it found. Does nothing while update checks are off.
pub async fn check_all(db: &PgPool, github: &GitHub) -> Result<(), JobError> {
    if !crate::updates::enabled(db).await.map_err(JobError::retry)? {
        return Ok(());
    }
    if !tether_db::accounts::owner_exists(db)
        .await
        .map_err(JobError::retry)?
    {
        return Ok(());
    }
    for app in plugin_sources::with_source(db)
        .await
        .map_err(JobError::retry)?
    {
        // Switched off meanwhile: stop contacting GitHub.
        if !crate::updates::enabled(db).await.map_err(JobError::retry)? {
            return Ok(());
        }
        // Stored ones were checked when set; skip anything else.
        let Some(repo) = Repo::parse(&app.source) else {
            continue;
        };
        let found = github.newest(&repo, Some(&app.plugin_id)).await;
        let recorded = match &found {
            Ok(found) => {
                plugin_sources::record_check(
                    db,
                    &app.plugin_id,
                    &app.source,
                    Some(&found.version),
                    found.release_url.as_deref(),
                    None,
                )
                .await
            }
            Err(why) => {
                tracing::warn!(plugin = app.plugin_id, repo = %repo, error = why, "app update check");
                plugin_sources::record_check(db, &app.plugin_id, &app.source, None, None, Some(why))
                    .await
            }
        };
        recorded.map_err(JobError::retry)?;
    }
    Ok(())
}

pub fn register_jobs(registry: &mut Registry, db: PgPool, github: GitHub) {
    registry.register(CHECK_JOB, move |_job| {
        let (db, github) = (db.clone(), github.clone());
        async move { check_all(&db, &github).await }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repositories_parse_strictly() {
        for (input, repo) in [
            ("https://github.com/infktd/tether", "infktd/tether"),
            ("https://github.com/infktd/tether/", "infktd/tether"),
            ("https://github.com/infktd/tether.git", "infktd/tether"),
            ("github.com/a-b/c.d_e", "a-b/c.d_e"),
            (" infktd/moon-mining ", "infktd/moon-mining"),
        ] {
            assert_eq!(
                Repo::parse(input).map(|r| r.0),
                Some(repo.to_owned()),
                "{input}"
            );
        }
        for input in [
            "",
            "infktd",
            "https://github.com/infktd",
            "https://gitlab.com/a/b",
            "https://github.com/a/b/releases",
            "a/..",
            "-a/b",
            "a b/c",
            "a/b?x=1",
            "a/b#c",
        ] {
            assert_eq!(Repo::parse(input), None, "{input}");
        }
    }

    fn asset(repo: &str, tag: &str, name: &str) -> Asset {
        Asset {
            name: name.to_owned(),
            size: 10,
            browser_download_url: format!(
                "https://github.com/{repo}/releases/download/{tag}/{name}"
            ),
        }
    }

    fn release(repo: &str, tag: &str, names: &[&str]) -> Release {
        Release {
            draft: false,
            prerelease: false,
            html_url: format!("https://github.com/{repo}/releases/tag/{tag}"),
            assets: names.iter().map(|n| asset(repo, tag, n)).collect(),
        }
    }

    #[test]
    fn finds_the_newest_signed_package() {
        let repo = Repo::parse("nmu/apps").unwrap();
        let web = "https://github.com";
        let releases = vec![
            release("nmu/apps", "v3", &["nmu.moon-mining-1.10.0.zip"]),
            release(
                "nmu/apps",
                "v2",
                &[
                    "nmu.moon-mining-1.9.0.zip",
                    "nmu.moon-mining-1.9.0.zip.minisig",
                ],
            ),
            release(
                "nmu/apps",
                "v1",
                &[
                    "nmu.moon-mining-1.2.0.zip",
                    "nmu.moon-mining-1.2.0.zip.minisig",
                    "nmu.srp-2.0.0.zip",
                    "nmu.srp-2.0.0.zip.minisig",
                ],
            ),
        ];
        // 1.10.0 has no signature, so 1.9.0 is the newest.
        let found = find(&releases, &repo, web, Some("nmu.moon-mining")).unwrap();
        assert_eq!(
            (found.plugin_id.as_str(), found.version.as_str()),
            ("nmu.moon-mining", "1.9.0")
        );
        assert_eq!(
            found.release_url.as_deref(),
            Some("https://github.com/nmu/apps/releases/tag/v2")
        );
        // Two apps: which one must be said.
        let several = find(&releases, &repo, web, None).unwrap_err();
        assert!(several.contains("nmu.moon-mining, nmu.srp"), "{several}");
        assert!(find(&releases, &repo, web, Some("nmu.other")).is_err());
    }

    #[test]
    fn downloads_stay_in_the_repository() {
        let downloads = "https://github.com/nmu/apps/releases/download/";
        assert!(in_downloads(
            "https://github.com/nmu/apps/releases/download/v1/a-1.0.0.zip",
            downloads
        ));
        for url in [
            "https://github.com/nmu/apps/releases/download/../../../evil/apps/x.zip",
            "https://github.com/nmu/apps/releases/download/%2e%2e/%2e%2e/%2e%2e/evil/x.zip",
            "https://github.com/nmu/apps/releases/download/v1/a.zip?x=1",
            "https://user@github.com/nmu/apps/releases/download/v1/a.zip",
            "https://github.com.evil.com/nmu/apps/releases/download/v1/a.zip",
        ] {
            assert!(!in_downloads(url, downloads), "{url}");
        }
    }

    #[test]
    fn ignores_drafts_prereleases_and_other_hosts() {
        let repo = Repo::parse("nmu/apps").unwrap();
        let web = "https://github.com";
        let mut draft = release(
            "nmu/apps",
            "v2",
            &["nmu.srp-2.0.0.zip", "nmu.srp-2.0.0.zip.minisig"],
        );
        draft.draft = true;
        let mut pre = release(
            "nmu/apps",
            "v3",
            &["nmu.srp-3.0.0.zip", "nmu.srp-3.0.0.zip.minisig"],
        );
        pre.prerelease = true;
        let elsewhere = release(
            "evil/apps",
            "v4",
            &["nmu.srp-4.0.0.zip", "nmu.srp-4.0.0.zip.minisig"],
        );
        let good = release(
            "nmu/apps",
            "v1",
            &["nmu.srp-1.0.0.zip", "nmu.srp-1.0.0.zip.minisig"],
        );
        let found = find(&[draft, pre, elsewhere, good], &repo, web, None).unwrap();
        assert_eq!(found.version, "1.0.0");
    }
}
