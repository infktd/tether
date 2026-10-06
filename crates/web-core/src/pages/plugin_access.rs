//! An app's data sources (AA's owners, added with its Add Owner): Tether's
//! Data sources page under the app's Manage (DESIGN.md, App shell), for
//! app admins (`admin.plugins`), who see every source with Remove, and
//! those who may add one (`plugin_consent::may_offer`), who see their own
//! with Withdraw and the Add data source button. Each source says how it
//! is doing, from the app's access log; app admins see which member
//! corporations the app can read, and a link to send a Director. A source
//! that stopped working puts a notice on the app's other pages. Data
//! sources are in use once added, as in AA: nobody approves them.
//! Plugins never see any of it.

use std::collections::BTreeSet;

use axum::extract::{Path, Query, State};
use axum::response::Response;
use axum_extra::extract::CookieJar;
use serde::Deserialize;
use tether_core::permissions::ADMIN_PLUGINS;
use tether_db::accounts::AccountId;
use tether_db::plugin_esi;

use super::PageError;
use crate::AppState;
use crate::auth::CurrentSession;
use crate::error::AppError;
use crate::plugin_consent;
use tether_plugins::manifest::Manifest;

/// One data source, as the Data sources page shows it.
pub struct OwnerRow {
    pub character_id: i64,
    pub name: String,
    pub corporation_id: i64,
    pub corporation: String,
    pub offered_by: String,
    pub when: String,
    /// active, suspended or moved.
    pub state: &'static str,
    /// On the viewer's own account (they may withdraw it).
    pub own: bool,
    /// When the app last read ESI through it (`4m ago`), and the full EVE
    /// time; empty if it never has.
    pub last_read: String,
    pub last_read_at: String,
    /// How it's doing, as a status line (`ok`, `warn`, `danger`, `off`),
    /// and why in a few words.
    pub tone: &'static str,
    pub status: &'static str,
    pub why: String,
}

/// A member corporation, and whether the app can read it.
pub struct CoverageRow {
    pub corporation_id: i64,
    pub corporation: String,
    pub tone: &'static str,
    pub status: String,
}

/// Which of the member corporations the app has a working data source in
/// (corporation data only).
pub struct Coverage {
    /// Read through a working source.
    pub read: usize,
    /// Without one that works (none, refused, or broken): the ones to
    /// send a Director the link for.
    pub missing: usize,
    pub rows: Vec<CoverageRow>,
}

/// A source withdrawn or removed lately.
pub struct GoneRow {
    pub name: String,
    pub what: String,
    pub when: String,
}

/// What an app's pages know of its data sources, for apps with any.
pub struct Owners {
    pub plugin_id: String,
    /// The app's name.
    pub app: String,
    /// May add one: Add data source on the Data sources page.
    pub can_offer: bool,
    /// Sees every source, with Remove (app admins).
    pub can_manage: bool,
    pub scopes: Vec<String>,
    /// The scopes read a corporation's data (through an in-game role),
    /// rather than the character's own (a fleet boss's fleet).
    pub corporate: bool,
    /// The app page Add data source comes back to (a link path; empty for
    /// the main page).
    pub back: String,
    /// The sources this viewer sees (every one for app admins, their own
    /// for others), and how each is doing.
    pub rows: Vec<OwnerRow>,
    /// The Data sources page alone: sources withdrawn or removed lately,
    /// the member corporations covered, and the page's own address to
    /// send a Director.
    pub gone: Vec<GoneRow>,
    pub coverage: Option<Coverage>,
    pub share: String,
    /// One line for the app's other pages when a source this viewer sees
    /// isn't working: its tone (`danger` when one reads nothing, else
    /// `signal`) and sentence.
    pub notice: Option<(&'static str, String)>,
}

fn time(at: chrono::DateTime<chrono::Utc>) -> String {
    super::eve_time(at, chrono::Utc::now())
}

/// An endpoint's catalogue name in words: `corporation-mining-observers`
/// is "mining observers".
fn endpoint_words(name: &str) -> String {
    [
        "corporation-",
        "character-",
        "alliance-",
        "source-",
        "universe-",
    ]
    .iter()
    .find_map(|prefix| name.strip_prefix(prefix))
    .unwrap_or(name)
    .replace('-', " ")
}

/// How a source is doing: its state, then the app's calls through it since
/// it was added (the access log's, by endpoint). A login that failed after
/// its last good call is the source's login, whatever the endpoint; a 403
/// is the endpoint's, and only a corporation's data source's trouble (an
/// in-game role): a fleet boss's 403 is a fleet they no longer lead.
/// `stale`: added before the window read, so "not read" isn't "not yet".
fn health(
    state: &str,
    reads: &[&plugin_esi::SourceReads],
    corporate: bool,
    stale: bool,
) -> (&'static str, &'static str, String) {
    match state {
        "moved" => (
            "danger",
            "Changed corporation",
            "Add it again from its new corporation".to_owned(),
        ),
        "suspended" => (
            "danger",
            "Not used",
            "Its account is deactivated or blacklisted, or the character left the account that \
             added it"
                .to_owned(),
        ),
        _ => {
            let last_ok = reads.iter().filter_map(|r| r.last_ok).max();
            let login = reads.iter().any(|r| {
                matches!(r.last_outcome.as_str(), "no usable token" | "ESI 401")
                    && last_ok.is_none_or(|ok| r.last_at > ok)
            });
            let refused: Vec<&str> = reads
                .iter()
                .filter(|r| corporate && r.last_outcome == "ESI 403")
                .map(|r| r.endpoint.as_str())
                .collect();
            let newest = reads
                .iter()
                .filter(|r| r.last_outcome == "ok")
                .max_by_key(|r| r.last_at);
            if login {
                (
                    "danger",
                    "Login stopped working",
                    "Its EVE login was revoked or expired: add it again".to_owned(),
                )
            } else if let [first, rest @ ..] = refused.as_slice() {
                let what = match rest.len() {
                    0 => endpoint_words(first),
                    n => format!("{} and {n} more", endpoint_words(first)),
                };
                (
                    "warn",
                    "Refused by ESI",
                    format!("ESI refused {what} (403): the character may lack the in-game role"),
                )
            } else if let Some(r) = newest {
                (
                    "ok",
                    "Working",
                    format!("Read {}", endpoint_words(&r.endpoint)),
                )
            } else if stale {
                (
                    "off",
                    "Not read lately",
                    format!(
                        "Nothing read through it in {} days",
                        crate::plugin_services::ACCESS_LOG_DAYS
                    ),
                )
            } else {
                (
                    "off",
                    "Not read yet",
                    "The app reads through it on its next run".to_owned(),
                )
            }
        }
    }
}

/// One line for the app's other pages when a source isn't working: what,
/// why, and (when it reads nothing) what the app can't read.
fn notice(app: &str, corporate: bool, rows: &[OwnerRow]) -> Option<(&'static str, String)> {
    let broken: Vec<&OwnerRow> = rows
        .iter()
        .filter(|r| r.tone == "warn" || r.tone == "danger")
        .collect();
    let tone = if broken.iter().any(|r| r.tone == "danger") {
        "danger"
    } else {
        "signal"
    };
    let text = match broken.as_slice() {
        [] => None,
        [one] if one.tone == "warn" => Some(format!(
            "A data source isn't working fully: {}. {}.",
            one.name, one.why
        )),
        [one] => Some(format!(
            "A data source isn't working: {}. {}. {app} can't read {} through it until it's \
             fixed or another is added.",
            one.name,
            one.why,
            if corporate {
                format!("{}'s data", one.corporation)
            } else {
                "ESI".to_owned()
            }
        )),
        many => Some(format!(
            "{} data sources aren't working, {} among them. {app} can't read everything through \
             them until they're fixed or others are added.",
            many.len(),
            many[0].name
        )),
    };
    text.map(|text| (tone, text))
}

/// `active` (in use), `suspended` (its account is deactivated or
/// blacklisted, or the character left the account that added it), or
/// `moved` (it changed corporation since it was added, or its corporation
/// wasn't known: adding it again brings it up to date).
pub fn source_state(d: &plugin_esi::DataSource) -> &'static str {
    if d.in_use() {
        "active"
    } else if !d.account_ok {
        "suspended"
    } else {
        "moved"
    }
}

/// What an app's pages know of its data sources before reading any, or
/// `None` if it has none.
fn skeleton(
    state: &AppState,
    manifest: &Manifest,
    can_offer: bool,
    can_manage: bool,
) -> Option<Owners> {
    if manifest.capabilities.esi.data_source.is_empty() {
        return None;
    }
    let id = manifest.plugin.id.clone();
    Some(Owners {
        app: manifest.plugin.name.clone(),
        can_offer,
        can_manage,
        corporate: manifest.capabilities.esi.data_source.iter().any(|s| {
            tether_core::scopes::info(s)
                .is_some_and(|i| i.kind == tether_core::scopes::ScopeKind::Corporation)
        }),
        back: String::new(),
        scopes: manifest.capabilities.esi.data_source.clone(),
        rows: Vec::new(),
        gone: Vec::new(),
        coverage: None,
        share: format!("{}/plugins/{id}/data-sources", state.site.origin()),
        notice: None,
        plugin_id: id,
    })
}

/// Who may see the data sources of `manifest`'s app (`perms` the viewer's
/// effective permissions), from those alone: `None` if the app has none.
/// Nothing is read until `load`, after the page's rate limit.
pub fn owners(
    state: &AppState,
    session: &CurrentSession,
    manifest: &Manifest,
    perms: &BTreeSet<String>,
) -> Option<Owners> {
    let holds = |p: &str| {
        perms.contains(p)
            && session
                .token_scopes
                .as_ref()
                .is_none_or(|scopes| scopes.contains(p))
    };
    skeleton(
        state,
        manifest,
        session.token_scopes.is_none() && plugin_consent::may_offer(manifest, |p| holds(p)),
        holds(ADMIN_PLUGINS),
    )
}

/// A stopped app's data sources, for its admin page (Apps › the app):
/// every one, with Remove, and no link to send (its pages aren't there).
/// It checks nothing: only for a caller that has checked `admin.plugins`.
pub fn for_admin(state: &AppState, manifest: &Manifest) -> Option<Owners> {
    skeleton(state, manifest, false, true).map(|mut owners| {
        owners.share = String::new();
        owners
    })
}

/// Reads the sources this viewer (`viewer`, none on an admin page) sees,
/// and how each is doing: every one for app admins, their own characters
/// for those who may add one. On the Data sources page (`page`) also what
/// was removed lately and, for corporation data, the coverage; elsewhere,
/// the notice. A character that came to the viewer's account after
/// another account added it shows nothing of that account's: not who
/// added it, nor its reads.
pub async fn load(
    state: &AppState,
    owners: &mut Owners,
    viewer: Option<AccountId>,
    page: bool,
) -> Result<(), AppError> {
    if !owners.can_manage && !owners.can_offer {
        return Ok(());
    }
    let mine: Vec<i64> = match viewer {
        Some(account) => plugin_esi::account_characters(&state.db, account)
            .await?
            .into_iter()
            .map(|c| c.id)
            .collect(),
        None => Vec::new(),
    };
    let sources: Vec<plugin_esi::DataSource> =
        plugin_esi::data_sources(&state.db, &owners.plugin_id)
            .await?
            .into_iter()
            .filter(|d| owners.can_manage || mine.contains(&d.character.id))
            .collect();
    let ids: Vec<i64> = sources
        .iter()
        .filter_map(|d| d.character.corporation_id)
        .collect();
    let names = tether_db::compliance::cached_names(&state.db, &ids).await?;
    let now = chrono::Utc::now();
    // The page reads all the access log keeps; the notice on the app's
    // other pages, the last day: how it's doing now.
    let days = if page {
        i64::from(crate::plugin_services::ACCESS_LOG_DAYS)
    } else {
        1
    };
    let since = now - chrono::Duration::days(days);
    let reads =
        plugin_esi::source_reads(&state.db, std::slice::from_ref(&owners.plugin_id), since).await?;
    owners.rows = sources
        .into_iter()
        .map(|d| {
            let corporation_id = d.character.corporation_id.unwrap_or(0);
            let state = source_state(&d);
            let transferred = !owners.can_manage && d.offered_by_account.map(AccountId) != viewer;
            let read: Vec<&plugin_esi::SourceReads> = if transferred {
                Vec::new()
            } else {
                reads
                    .iter()
                    .filter(|r| r.character_id == d.character.id)
                    .collect()
            };
            let (tone, status, why) = if transferred {
                ("off", "Not used", "Added from another account".to_owned())
            } else {
                health(state, &read, owners.corporate, page && d.offered_at < since)
            };
            let last_ok = read.iter().filter_map(|r| r.last_ok).max();
            OwnerRow {
                last_read: last_ok
                    .map_or_else(String::new, |at| super::ago((now - at).num_seconds())),
                last_read_at: last_ok.map_or_else(String::new, |at| {
                    format!("{} EVE", at.format("%Y-%m-%d %H:%M:%S"))
                }),
                tone,
                status,
                why,
                state,
                own: mine.contains(&d.character.id),
                character_id: d.character.id,
                corporation: names
                    .get(&corporation_id)
                    .cloned()
                    .unwrap_or_else(|| "Unknown corporation".to_owned()),
                corporation_id,
                name: d.character.name,
                offered_by: if transferred {
                    "another account".to_owned()
                } else {
                    d.offered_by.unwrap_or_else(|| "Someone".to_owned())
                },
                when: if transferred {
                    String::new()
                } else {
                    time(d.offered_at)
                },
            }
        })
        .collect();
    if !page {
        owners.notice = notice(&owners.app, owners.corporate, &owners.rows);
        return Ok(());
    }
    if owners.can_manage {
        owners.gone = plugin_esi::gone_data_sources(&state.db, &owners.plugin_id)
            .await?
            .into_iter()
            .map(gone_row)
            .collect();
        if owners.corporate {
            owners.coverage = Some(coverage(state, &owners.rows).await?);
        }
    }
    Ok(())
}

/// Every app's data sources, working and not, by the Data sources page's
/// rules on the last day's calls, as its notice judges them: the
/// sidebar's foot and the System page. A stopped app's 403s aren't
/// counted (whether its data is a corporation's is in its manifest).
pub async fn source_health(state: &AppState) -> Result<(i64, i64), AppError> {
    let sources = plugin_esi::all_data_sources(&state.db).await?;
    if sources.is_empty() {
        return Ok((0, 0));
    }
    let mut ids: Vec<String> = sources.iter().map(|(id, _)| id.clone()).collect();
    ids.sort();
    ids.dedup();
    let corporate: Vec<String> = state
        .plugins
        .all_running()
        .into_iter()
        .filter(|r| skeleton(state, &r.manifest, false, false).is_some_and(|o| o.corporate))
        .map(|r| r.manifest.plugin.id.clone())
        .collect();
    let since = chrono::Utc::now() - chrono::Duration::days(1);
    let reads = plugin_esi::source_reads(&state.db, &ids, since).await?;
    let broken = sources
        .iter()
        .filter(|(id, d)| {
            let read: Vec<&plugin_esi::SourceReads> = reads
                .iter()
                .filter(|r| &r.plugin_id == id && r.character_id == d.character.id)
                .collect();
            let (tone, _, _) = health(source_state(d), &read, corporate.contains(id), false);
            tone == "warn" || tone == "danger"
        })
        .count();
    let working = sources.len() - broken;
    Ok((
        i64::try_from(working).unwrap_or(i64::MAX),
        i64::try_from(broken).unwrap_or(i64::MAX),
    ))
}

/// The member corporations and how the app reads each: through a working
/// source, one ESI partly refuses, one not read yet, only ones that don't
/// work, or none. The ones to act on first.
async fn coverage(state: &AppState, rows: &[OwnerRow]) -> Result<Coverage, AppError> {
    let through = |id: i64, tone: &str| -> Option<String> {
        let names: Vec<&str> = rows
            .iter()
            .filter(|r| r.corporation_id == id && r.tone == tone)
            .map(|r| r.name.as_str())
            .collect();
        match names.as_slice() {
            [] => None,
            [one] => Some((*one).to_owned()),
            [one, rest @ ..] => Some(format!("{one} and {} more", rest.len())),
        }
    };
    let mut list: Vec<CoverageRow> = plugin_esi::member_corporations(&state.db)
        .await?
        .into_iter()
        .map(|(id, name)| {
            let (tone, status) = if let Some(who) = through(id, "ok") {
                ("ok", format!("Read through {who}"))
            } else if let Some(who) = through(id, "warn") {
                ("warn", format!("Refused by ESI through {who}"))
            } else if let Some(who) = through(id, "off") {
                ("off", format!("Not read yet through {who}"))
            } else if rows.iter().any(|r| r.corporation_id == id) {
                ("danger", "Its data sources aren't working".to_owned())
            } else {
                ("warn", "No data source".to_owned())
            };
            CoverageRow {
                corporation_id: id,
                corporation: name.unwrap_or_else(|| "Unknown corporation".to_owned()),
                tone,
                status,
            }
        })
        .collect();
    let rank = |tone: &str| match tone {
        "danger" => 0,
        "warn" => 1,
        "off" => 2,
        _ => 3,
    };
    list.sort_by(|a, b| {
        rank(a.tone)
            .cmp(&rank(b.tone))
            .then(a.corporation.cmp(&b.corporation))
    });
    Ok(Coverage {
        read: list.iter().filter(|r| r.tone == "ok").count(),
        missing: list
            .iter()
            .filter(|r| r.tone == "warn" || r.tone == "danger")
            .count(),
        rows: list,
    })
}

pub fn gone_row(g: plugin_esi::GoneSource) -> GoneRow {
    let who = g.actor_name.unwrap_or_else(|| "Someone".to_owned());
    GoneRow {
        name: g
            .character_name
            .unwrap_or_else(|| "Unknown character".to_owned()),
        what: if g.action.ends_with("withdrawn") {
            format!("Withdrawn by {who}")
        } else {
            format!("Removed by {who}")
        },
        when: time(g.at),
    }
}

/// A plugin id from the path, checked (anything else can't exist).
fn plugin_id(id: &str) -> Result<&str, AppError> {
    tether_plugins::manifest::check_id(id)
        .map(|()| id)
        .map_err(|_| AppError::not_found("No such app."))
}

/// Back to the app's Data sources page, or, if it isn't running, to its
/// admin page for app admins (where a stopped app's sources are) and the
/// Dashboard for others, with a toast saying what happened.
fn back(state: &AppState, id: &str, admin: bool, message: &str) -> Response {
    if state.plugins.running(id).is_some() {
        super::stay::back(
            &format!(
                "/plugins/{id}/{}",
                tether_plugins::manifest::DATA_SOURCES_PATH
            ),
            message,
        )
    } else if admin {
        super::stay::back(&format!("/admin/plugins/{id}"), message)
    } else {
        super::stay::back("/dashboard", message)
    }
}

/// Add owner's form: the app page it was on (`back`, a link path), which
/// the login comes back to; empty for the app's main page.
#[derive(Debug, Default, Deserialize)]
pub struct AddForm {
    #[serde(default)]
    back: String,
}

/// `POST /apps/{id}/owners/add`: Add owner, off to EVE SSO to log in with
/// the character to add; back on the app's page afterwards.
pub async fn add(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    jar: CookieJar,
    Path(id): Path<String>,
    axum::Form(form): axum::Form<AddForm>,
) -> Result<Response, PageError> {
    let session = session.ok_or_else(AppError::unauthorized)?;
    // A browser's login: an access token can't go to EVE.
    if session.token_scopes.is_some() {
        return Err(AppError::forbidden().into());
    }
    let id = plugin_id(&id)?;
    Ok(plugin_consent::start_offer(&state, jar, session.account, id, &form.back).await?)
}

/// `POST /apps/{id}/owners/{character}/withdraw`: the character's owner
/// withdraws it. Always allowed for
/// one's own characters.
pub async fn withdraw(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path((id, character)): Path<(String, i64)>,
    Query(from): Query<WithdrawFrom>,
) -> Result<Response, PageError> {
    let session = session.ok_or_else(AppError::unauthorized)?;
    let id = plugin_id(&id)?;
    plugin_consent::withdraw_offer(&state, session.account, id, character).await?;
    if from.from.as_deref() == Some("tokens") {
        return Ok(super::stay::back("/tokens", "Withdrawn."));
    }
    Ok(back(&state, id, false, "Withdrawn."))
}

/// Where a withdrawal came from: `?from=tokens` is Token Management.
#[derive(Debug, Default, Deserialize)]
pub struct WithdrawFrom {
    #[serde(default)]
    from: Option<String>,
}

/// One of the account's characters that is an app's owner, for Token
/// Management: wherever else they are, pilots always see what their
/// characters are used for, and can withdraw them.
pub struct OwnSource {
    pub plugin_id: String,
    pub plugin_name: String,
    pub character_id: i64,
    pub name: String,
    /// active, suspended, moved, or stopped (the app doesn't run).
    pub state: &'static str,
}

/// Every installed app's sources that are the account's characters, so
/// they can be withdrawn while the app is stopped too (then not used).
pub async fn own_sources(state: &AppState, account: AccountId) -> Result<Vec<OwnSource>, AppError> {
    let mine: Vec<i64> = plugin_esi::account_characters(&state.db, account)
        .await?
        .into_iter()
        .map(|c| c.id)
        .collect();
    let mut out = Vec::new();
    for app in tether_db::plugins::list(&state.db).await? {
        let running = state.plugins.running(&app.id).is_some();
        for d in plugin_esi::data_sources(&state.db, &app.id).await? {
            if mine.contains(&d.character.id) {
                out.push(OwnSource {
                    plugin_id: app.id.clone(),
                    plugin_name: app.name.clone(),
                    character_id: d.character.id,
                    state: if running { source_state(&d) } else { "stopped" },
                    name: d.character.name,
                });
            }
        }
    }
    Ok(out)
}

/// An app admin (`admin.plugins`).
async fn app_admin(
    state: &AppState,
    session: Option<CurrentSession>,
) -> Result<AccountId, AppError> {
    let session = session.ok_or_else(AppError::unauthorized)?;
    session.require(state, ADMIN_PLUGINS).await?;
    Ok(session.account)
}

/// `POST /apps/{id}/owners/{character}/remove`
pub async fn remove(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path((id, character)): Path<(String, i64)>,
) -> Result<Response, PageError> {
    let admin = app_admin(&state, session).await?;
    let id = plugin_id(&id)?;
    plugin_consent::remove_source_as_admin(&state, admin, id, character).await?;
    Ok(back(&state, id, true, "Removed."))
}
