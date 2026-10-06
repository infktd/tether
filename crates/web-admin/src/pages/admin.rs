//! Admin pages: groups and permissions (states are in `states`). Plain forms that work
//! without JavaScript (post, then redirect); htmx boosts them. Every action
//! goes through `crate::admin`, the same code as the JSON API.

use askama::Template;
use axum::Form;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use serde::Deserialize;
use tether_core::groups::Flags;
use tether_core::permissions::{ADMIN_GROUPS, ADMIN_PERMISSIONS};
use tether_db::accounts::{self, AccountId};
use tether_db::groups::{self, GroupId};
use tether_db::permissions::{self, Grantee};

use super::{PageError, Shell, load, render};
use crate::AppState;
use crate::admin;
use crate::auth::CurrentSession;
use crate::error::AppError;

/// Signed in (else the login page) and holding `permission` (else 403).
pub async fn guard(
    state: &AppState,
    session: Option<CurrentSession>,
    permission: &str,
    active: &'static str,
) -> Result<(CurrentSession, Shell), PageError> {
    let session = session.ok_or_else(AppError::unauthorized)?;
    session.require(state, permission).await?;
    let loaded = load(state, &session, active).await?;
    Ok((session, loaded.shell))
}

/// A state in a `<select>`.
pub struct StateOption {
    pub id: i64,
    pub name: String,
}

pub async fn state_options(state: &AppState) -> Result<Vec<StateOption>, AppError> {
    Ok(tether_db::states::all(&state.db)
        .await?
        .into_iter()
        .map(|s| StateOption {
            id: s.id.0,
            name: s.name,
        })
        .collect())
}

/// A state's name for display, from a list loaded once.
pub fn state_name(states: &[StateOption], id: tether_core::states::StateId) -> String {
    states
        .iter()
        .find(|s| s.id == id.0)
        .map_or_else(|| format!("state {}", id.0), |s| s.name.clone())
}

#[derive(Template)]
#[template(path = "admin_overview.html")]
struct OverviewPage {
    shell: Shell,
    groups: Vec<crate::admin_nav::Listed>,
    /// `admin.system` holders get the System panel first (loaded after
    /// the page).
    system_panel: bool,
}

/// `GET /admin`: Administration's overview, every admin page this account
/// may open, by group.
pub async fn index(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
) -> Result<Response, PageError> {
    let session = session.ok_or_else(AppError::unauthorized)?;
    let loaded = load(&state, &session, crate::admin_nav::OVERVIEW).await?;
    let groups = crate::admin_nav::listed(&loaded.shell.nav, "");
    if groups.is_empty() {
        return Err(AppError::forbidden().into());
    }
    Ok(render(
        StatusCode::OK,
        &OverviewPage {
            system_panel: loaded.shell.nav.system,
            shell: loaded.shell,
            groups,
        },
    ))
}

// ---- groups ----------------------------------------------------------------

/// A form's fields as pairs, so checkboxes that repeat a name (the allowed
/// states) all arrive.
type Fields = Vec<(String, String)>;

fn field<'a>(fields: &'a Fields, name: &str) -> &'a str {
    fields
        .iter()
        .find(|(k, _)| k == name)
        .map_or("", |(_, v)| v.as_str())
}

fn checked(fields: &Fields, name: &str) -> bool {
    fields.iter().any(|(k, _)| k == name)
}

fn flags_from(fields: &Fields) -> Flags {
    Flags {
        internal: checked(fields, "internal"),
        hidden: checked(fields, "hidden"),
        open: checked(fields, "open"),
        public: checked(fields, "public"),
        restricted: checked(fields, "restricted"),
    }
}

pub struct GroupRow {
    pub id: i64,
    pub name: String,
    pub description: String,
    pub label: &'static str,
    pub hidden: bool,
    pub public: bool,
    pub restricted: bool,
    pub compliance: bool,
    pub members: i64,
    pub pending: i64,
}

fn group_row(g: groups::GroupSummary) -> GroupRow {
    GroupRow {
        id: g.group.id.0,
        label: g.group.flags.label(),
        hidden: g.group.flags.hidden,
        public: g.group.flags.public,
        restricted: g.group.flags.restricted,
        compliance: g.group.compliance,
        name: g.group.name,
        description: g.group.description,
        members: g.members,
        pending: g.pending,
    }
}

pub struct ReservedRow {
    pub name: String,
    pub reason: String,
}

/// What the new-group form had, to show it again after an error.
pub struct NewGroupForm {
    pub name: String,
    pub description: String,
    pub flags: Flags,
    /// A Secure Group: pilots apply for it once they pass its filters.
    pub secure: bool,
}

impl Default for NewGroupForm {
    fn default() -> Self {
        Self {
            name: String::new(),
            description: String::new(),
            // AA's defaults.
            flags: Flags {
                internal: true,
                hidden: true,
                ..Flags::default()
            },
            secure: false,
        }
    }
}

impl NewGroupForm {
    /// A Secure Group's: pilots must see it to apply, so neither Internal
    /// nor Hidden.
    fn secure() -> Self {
        Self {
            flags: Flags::default(),
            secure: true,
            ..Self::default()
        }
    }
}

#[derive(Debug, Default, Deserialize)]
pub struct GroupsQuery {
    /// `on`: the new-group form starts as a Secure Group's (the Secure
    /// Groups page's New Secure Group).
    #[serde(default)]
    secure: String,
}

#[derive(Template)]
#[template(path = "admin_groups.html")]
struct GroupsPage {
    shell: Shell,
    groups: Vec<GroupRow>,
    reserved: Vec<ReservedRow>,
    options: crate::groups::Options,
    error: Option<String>,
    form: NewGroupForm,
}

async fn groups_page(
    state: &AppState,
    shell: Shell,
    form: NewGroupForm,
    error: Option<AppError>,
) -> Result<Response, PageError> {
    let groups = groups::summaries(&state.db)
        .await?
        .into_iter()
        .map(group_row)
        .collect();
    let reserved = groups::reserved(&state.db)
        .await?
        .into_iter()
        .map(|r| ReservedRow {
            name: r.name,
            reason: r.reason,
        })
        .collect();
    let status = error.as_ref().map_or(StatusCode::OK, AppError::status);
    let problem = error.as_ref().map(|e| e.message().to_owned());
    let page = GroupsPage {
        shell,
        groups,
        reserved,
        options: crate::groups::options(&state.db).await?,
        error: error.map(|e| e.message().to_owned()),
        form,
    };
    Ok(super::with_problem(problem, render(status, &page)))
}

/// `GET /admin/groups`
pub async fn groups(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Query(query): Query<GroupsQuery>,
) -> Result<Response, PageError> {
    let (_, shell) = guard(&state, session, ADMIN_GROUPS, "admin_groups").await?;
    let form = if query.secure == "on" {
        NewGroupForm::secure()
    } else {
        NewGroupForm::default()
    };
    groups_page(&state, shell, form, None).await
}

/// `POST /admin/groups`
pub async fn create_group(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Form(fields): Form<Fields>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_GROUPS, "admin_groups").await?;
    let secure = checked(&fields, "secure");
    let mut form = NewGroupForm {
        name: field(&fields, "name").to_owned(),
        description: field(&fields, "description").to_owned(),
        flags: flags_from(&fields),
        secure,
    };
    // Pilots must see a Secure Group to apply for it.
    if secure {
        form.flags.internal = false;
    }
    let id = match crate::groups::create(
        &state.db,
        session.account,
        &form.name,
        &form.description,
        form.flags,
    )
    .await
    {
        Ok(id) => id,
        Err(err) => return groups_page(&state, shell, form, Some(err)).await,
    };
    if !secure {
        return Ok(super::stay::back(
            &format!("/admin/groups/{}", id.0),
            "Group created.",
        ));
    }
    // AA's defaults: enabled, in the hourly updates, taking requests.
    match crate::smart_groups::set_settings(
        &state.db,
        session.account,
        id,
        Some(tether_db::smart_groups::Settings::default()),
    )
    .await
    {
        Ok(()) => Ok(super::stay::back(
            &format!("/admin/groups/{}", id.0),
            "Secure Group created: add its filters under Secure Group.",
        )),
        Err(err) => on_group(&state, shell, id.0, Err(err)).await,
    }
}

/// `POST /admin/groups/settings`: auto-leave and request notifications.
pub async fn group_options(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Form(fields): Form<Fields>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_GROUPS, "admin_groups").await?;
    let options = crate::groups::Options {
        auto_leave: checked(&fields, "auto_leave"),
        notify_requests: checked(&fields, "notify_requests"),
    };
    match crate::groups::set_options(&state.db, session.account, options).await {
        Ok(()) => Ok(super::stay::back("/admin/groups", "Settings saved.")),
        Err(err) => groups_page(&state, shell, NewGroupForm::default(), Some(err)).await,
    }
}

#[derive(Debug, Deserialize)]
pub struct ReserveForm {
    name: String,
    #[serde(default)]
    reason: String,
}

/// `POST /admin/groups/reserved`
pub async fn reserve(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Form(form): Form<ReserveForm>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_GROUPS, "admin_groups").await?;
    match crate::groups::reserve(&state.db, session.account, &form.name, &form.reason).await {
        Ok(()) => Ok(super::stay::back("/admin/groups", "Name reserved.")),
        Err(err) => groups_page(&state, shell, NewGroupForm::default(), Some(err)).await,
    }
}

/// `POST /admin/groups/reserved/remove`
pub async fn unreserve(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Form(form): Form<ReserveForm>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_GROUPS, "admin_groups").await?;
    match crate::groups::unreserve(&state.db, session.account, &form.name).await {
        Ok(()) => Ok(super::stay::back("/admin/groups", "Reservation removed.")),
        Err(err) => groups_page(&state, shell, NewGroupForm::default(), Some(err)).await,
    }
}

pub struct MemberRow {
    pub account_id: i64,
    pub main_id: i64,
    pub main_name: String,
    pub state: String,
    pub state_style: String,
}

pub struct LeaderRow {
    pub account_id: i64,
    pub main_id: i64,
    pub name: String,
}

pub struct StateChoice {
    pub id: i64,
    pub name: String,
    pub checked: bool,
}

#[derive(Template)]
#[template(path = "admin_group.html")]
struct GroupPage {
    shell: Shell,
    group: GroupRow,
    flags: Flags,
    states: Vec<StateChoice>,
    leaders: Vec<LeaderRow>,
    leader_groups: Vec<GroupOption>,
    other_groups: Vec<GroupOption>,
    members: Vec<MemberRow>,
    /// Secure Groups: its settings and filters, if it's a smart group.
    smart: Option<SmartView>,
    /// Filters running apps offer.
    app_filters: Vec<AppFilterView>,
    error: Option<String>,
    /// What Check now did.
    notice: Option<String>,
    /// One account checked against the filters (aa-securegroups' Check).
    check: Option<crate::smart_groups::Explained>,
    /// The name searched for, kept in the Check field.
    check_query: String,
}

/// An app's Secure Groups filter, for the add form.
pub struct AppFilterView {
    /// `plugin/filter`.
    pub value: String,
    pub label: String,
    pub sum: bool,
    /// `(name, label, number)`.
    pub fields: Vec<(String, String, bool)>,
}

pub struct SmartView {
    pub settings: tether_db::smart_groups::Settings,
    pub filters: Vec<SmartFilterView>,
    /// An app filter has no fresh values: sweeps leave the group alone.
    pub frozen: bool,
    /// When the sweep or Check now last judged it, EVE time.
    pub checked_at: Option<String>,
    /// Ping channels, for the run summaries (AA's group update webhook).
    pub channels: Vec<tether_db::pings::PingChannel>,
    /// Nobody holds `securegroups.access_sec_group`: no pilot can open
    /// Secure Groups to see or request it.
    pub unseen: bool,
}

impl SmartView {
    pub fn is_update_channel(&self, channel: &i64) -> bool {
        self.settings.update_channel == Some(*channel)
    }
}

/// One of a smart group's filters.
pub struct SmartFilterView {
    pub id: i64,
    /// What it asks.
    pub text: String,
    pub grace_days: i32,
    /// No longer reads: can only be deleted.
    pub broken: bool,
}

/// What a group's page shows besides the group.
#[derive(Default)]
struct Extra {
    error: Option<AppError>,
    notice: Option<String>,
    check: Option<crate::smart_groups::Explained>,
    check_query: String,
}

impl Extra {
    fn error(error: AppError) -> Self {
        Self {
            error: Some(error),
            ..Self::default()
        }
    }
}

async fn group_page(
    state: &AppState,
    shell: Shell,
    id: i64,
    extra: Extra,
) -> Result<Response, PageError> {
    let Extra {
        error,
        notice,
        check,
        check_query,
    } = extra;
    let all = groups::summaries(&state.db).await?;
    let found = all
        .iter()
        .find(|g| g.group.id.0 == id)
        .cloned()
        .ok_or_else(|| AppError::not_found("No such group."))?;
    let allowed = groups::allowed_states(&state.db, GroupId(id)).await?;
    let states = tether_db::states::all(&state.db)
        .await?
        .into_iter()
        .map(|s| StateChoice {
            checked: allowed.contains(&s.id),
            id: s.id.0,
            name: s.name,
        })
        .collect();
    let leading = groups::leader_groups(&state.db, GroupId(id)).await?;
    let (leader_groups, other_groups): (Vec<_>, Vec<_>) = all
        .iter()
        .filter(|g| g.group.id.0 != id)
        .map(|g| GroupOption {
            id: g.group.id.0,
            name: g.group.name.clone(),
        })
        .partition(|g| leading.contains(&GroupId(g.id)));
    let leaders = groups::leaders(&state.db, GroupId(id))
        .await?
        .into_iter()
        .map(|(account, main_id, name)| LeaderRow {
            account_id: account.0,
            main_id,
            name,
        })
        .collect();
    let members = groups::members(&state.db, GroupId(id))
        .await?
        .into_iter()
        .map(|m| MemberRow {
            account_id: m.account_id,
            main_id: m.main_id,
            main_name: m.main_name,
            state: m.state,
            state_style: m.state_style,
        })
        .collect();
    let smart = match tether_db::smart_groups::settings(&state.db, GroupId(id)).await? {
        Some(s) => {
            let (rules, broken) = tether_db::smart_groups::rules(&state.db, GroupId(id)).await?;
            let mut conn = state.db.acquire().await?;
            let names = crate::smart_groups::Names::load(&mut conn, &rules, false).await?;
            let mut filters: Vec<SmartFilterView> = rules
                .iter()
                .map(|r| SmartFilterView {
                    id: r.id,
                    text: names.describe(r),
                    grace_days: r.grace_days,
                    broken: false,
                })
                .collect();
            // Kept out of sweeps until deleted: shown so they can be.
            filters.extend(broken.into_iter().map(|id| SmartFilterView {
                id,
                text: "a filter that no longer reads (delete it)".to_owned(),
                grace_days: 0,
                broken: true,
            }));
            let known = tether_db::smart_groups::app_keys_known(&state.db).await?;
            let frozen = rules.iter().any(|r| {
                r.filter.leaves().iter().any(|leaf| match leaf {
                    tether_core::smart::Filter::App {
                        plugin,
                        name,
                        config,
                        ..
                    } => !known.contains(&tether_core::smart::app_key(plugin, name, config)),
                    _ => false,
                })
            });
            let checked_at = tether_db::smart_groups::swept_at(&state.db, GroupId(id))
                .await?
                .map(|at| at.format("%Y-%m-%d %H:%M").to_string());
            let channels = match crate::discord::config(state).await {
                Ok(config) => crate::pings::channels_for(state, &config).await?,
                Err(_) => Vec::new(),
            };
            let unseen = !tether_db::permissions::list(&state.db)
                .await?
                .iter()
                .any(|g| g.permission == tether_core::permissions::SECUREGROUPS_ACCESS);
            Some(SmartView {
                checked_at,
                settings: s,
                filters,
                frozen,
                channels,
                unseen,
            })
        }
        None => None,
    };
    let app_filters = state
        .plugins
        .all_running()
        .into_iter()
        .flat_map(|r| {
            let (id, name) = (r.manifest.plugin.id.clone(), r.manifest.plugin.name.clone());
            r.manifest
                .filters
                .iter()
                .map(|f| AppFilterView {
                    value: format!("{id}/{}", f.name),
                    label: format!("{name}: {}", f.label),
                    sum: f.combine == tether_plugins::manifest::Combine::Sum,
                    fields: f
                        .fields
                        .iter()
                        .map(|x| {
                            (
                                x.name.clone(),
                                x.label.clone(),
                                x.kind == tether_plugins::manifest::FieldKind::Number,
                            )
                        })
                        .collect(),
                })
                .collect::<Vec<_>>()
        })
        .collect();
    let status = error.as_ref().map_or(StatusCode::OK, AppError::status);
    let problem = error.as_ref().map(|e| e.message().to_owned());
    let page = GroupPage {
        shell,
        smart,
        app_filters,
        flags: found.group.flags,
        group: group_row(found),
        states,
        leaders,
        leader_groups,
        other_groups,
        members,
        error: error.map(|e| e.message().to_owned()),
        notice,
        check,
        check_query,
    };
    Ok(super::with_problem(problem, render(status, &page)))
}

/// `GET /admin/groups/{id}`. With `?check=<character>` (a name or id) or
/// `?account=<id>` (a member's row), one account checked against its
/// filters (aa-securegroups' Check); `?checked=<n>` is what Check now
/// just did. The query is read only once the viewer is let in.
pub async fn group(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(id): Path<i64>,
    Query(query): Query<std::collections::HashMap<String, String>>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_GROUPS, "admin_groups").await?;
    let check_query = query
        .get("check")
        .map(|c| c.trim().to_owned())
        .unwrap_or_default();
    let account = match (query.get("account"), check_query.is_empty()) {
        (Some(account), _) => Some(
            account
                .parse::<i64>()
                .map(AccountId)
                .map_err(|_| AppError::not_found("No such account.")),
        ),
        (None, false) => Some(find_account(&state, &check_query).await),
        (None, true) => None,
    };
    let checked = query
        .get("checked")
        .and_then(|n| n.parse::<usize>().ok())
        .map(checked_now);
    let extra = match account {
        None => Extra {
            notice: checked,
            ..Extra::default()
        },
        Some(Err(err)) => Extra {
            check_query,
            ..Extra::error(err)
        },
        Some(Ok(account)) => {
            let anyone = permissions::effective(&state.db, session.account)
                .await?
                .contains(tether_core::permissions::ADMIN_USERS);
            match crate::smart_groups::explain(
                &state.db,
                session.account,
                GroupId(id),
                account,
                anyone,
            )
            .await
            {
                Ok(explained) => Extra {
                    check: Some(explained),
                    check_query,
                    ..Extra::default()
                },
                Err(err) => Extra {
                    check_query,
                    ..Extra::error(err)
                },
            }
        }
    };
    group_page(&state, shell, id, extra).await
}

/// What Check now did, in words.
fn checked_now(changed: usize) -> String {
    match changed {
        0 => "Checked now: every member passes, and nobody else was due to join.".to_owned(),
        1 => "Checked now: 1 membership changed. The Audit Log names it.".to_owned(),
        n => format!("Checked now: {n} memberships changed. The Audit Log names each."),
    }
}

/// `POST /admin/groups/{id}/smart/check`: Check now, the hourly sweep for
/// this group alone (aa-securegroups' Run check); audited. Back to the
/// group's page, which says what it did: in a toast with htmx (the page
/// stays where it was), else on the page (`?checked=`).
pub async fn smart_check(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    headers: axum::http::HeaderMap,
    Path(id): Path<i64>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_GROUPS, "admin_groups").await?;
    match crate::smart_groups::check_now(&state.db, session.account, GroupId(id)).await {
        Ok(changed) if super::is_htmx(&headers) => Ok(super::stay::back(
            &format!("/admin/groups/{id}"),
            checked_now(changed),
        )),
        Ok(changed) => {
            Ok(Redirect::to(&format!("/admin/groups/{id}?checked={changed}")).into_response())
        }
        Err(err) => group_page(&state, shell, id, Extra::error(err)).await,
    }
}

/// Runs a change to one group and shows its page again.
async fn on_group(
    state: &AppState,
    shell: Shell,
    id: i64,
    result: Result<(), AppError>,
) -> Result<Response, PageError> {
    match result {
        Ok(()) => Ok(super::stay::back(&format!("/admin/groups/{id}"), "Saved.")),
        Err(err) => group_page(state, shell, id, Extra::error(err)).await,
    }
}

/// `POST /admin/groups/{id}/smart`: make it a smart group (Secure
/// Groups), change its settings (allianceauth-secure-groups'), or (`smart`
/// unticked) make it ordinary. Made smart from the plain form (no
/// `configured`), it takes AA's defaults.
pub async fn smart_settings(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(id): Path<i64>,
    Form(fields): Form<Fields>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_GROUPS, "admin_groups").await?;
    let result = if !checked(&fields, "smart") {
        crate::smart_groups::set_settings(&state.db, session.account, GroupId(id), None).await
    } else {
        let settings = if checked(&fields, "configured") {
            match field(&fields, "update_channel").trim() {
                "" => Ok(None),
                channel => channel
                    .parse::<i64>()
                    .map(Some)
                    .map_err(|_| AppError::bad_request("Choose a ping channel from the list.")),
            }
            .map(|update_channel| tether_db::smart_groups::Settings {
                auto_join: checked(&fields, "auto_join"),
                enabled: checked(&fields, "enabled"),
                include_in_updates: checked(&fields, "include_in_updates"),
                can_grace: checked(&fields, "can_grace"),
                notify_on_add: checked(&fields, "notify_on_add"),
                notify_on_remove: checked(&fields, "notify_on_remove"),
                notify_on_grace: checked(&fields, "notify_on_grace"),
                update_channel,
                update_message: field(&fields, "update_message").trim().to_owned(),
            })
        } else {
            Ok(tether_db::smart_groups::Settings {
                auto_join: checked(&fields, "auto_join"),
                ..Default::default()
            })
        };
        match settings {
            Ok(settings) => {
                crate::smart_groups::set_settings(
                    &state.db,
                    session.account,
                    GroupId(id),
                    Some(settings),
                )
                .await
            }
            Err(err) => Err(err),
        }
    };
    on_group(&state, shell, id, result).await
}

/// Faction ids from a comma-separated list of faction names or ids, each
/// resolved through ESI.
async fn factions(state: &AppState, text: &str) -> Result<Vec<i64>, AppError> {
    let parts: Vec<&str> = text
        .split(',')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect();
    if parts.is_empty() || parts.len() > 20 {
        return Err(AppError::bad_request(
            "Name 1 to 20 factions, separated by commas.",
        ));
    }
    let mut ids = Vec::new();
    for part in parts {
        let found: Vec<i64> = if let Ok(id) = part.parse::<i64>() {
            vec![id]
        } else {
            state
                .esi
                .resolve_names(&[part.to_owned()], tether_esi::Priority::Interactive)
                .await
                .map_err(crate::admin::esi_unavailable)?
                .factions
                .iter()
                .map(|e| e.id)
                .collect()
        };
        // Named through ESI (and kept for describing the filter).
        let named = tether_esi::names::resolve(
            &state.db,
            &state.esi,
            &found,
            tether_esi::Priority::Interactive,
        )
        .await
        .map_err(crate::admin::names_unavailable)?;
        let factions: Vec<i64> = found
            .into_iter()
            .filter(|id| {
                named
                    .get(id)
                    .is_some_and(|n| n.kind() == Some(tether_core::states::EntityKind::Faction))
            })
            .collect();
        if factions.is_empty() {
            return Err(AppError::bad_request(format!(
                "No faction is named exactly {part}."
            )));
        }
        ids.extend(factions);
    }
    ids.sort_unstable();
    ids.dedup();
    Ok(ids)
}

/// Ids from a comma-separated list of corporation and alliance names or
/// ids, each resolved through ESI (never trusted as typed).
async fn entities(state: &AppState, text: &str) -> Result<Vec<i64>, AppError> {
    let parts: Vec<&str> = text
        .split(',')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect();
    if parts.is_empty() || parts.len() > 20 {
        return Err(AppError::bad_request(
            "Name 1 to 20 corporations or alliances, separated by commas.",
        ));
    }
    let mut ids = Vec::new();
    for part in parts {
        let is_org = |kind: Option<tether_core::states::EntityKind>| {
            matches!(
                kind,
                Some(
                    tether_core::states::EntityKind::Corporation
                        | tether_core::states::EntityKind::Alliance
                )
            )
        };
        if let Ok(id) = part.parse::<i64>() {
            let named = tether_esi::names::resolve(
                &state.db,
                &state.esi,
                &[id],
                tether_esi::Priority::Interactive,
            )
            .await
            .map_err(crate::admin::names_unavailable)?;
            match named.get(&id) {
                Some(n) if is_org(n.kind()) => ids.push(id),
                _ => {
                    return Err(AppError::bad_request(format!(
                        "{id} isn't a corporation or alliance EVE knows."
                    )));
                }
            }
        } else {
            let resolved = state
                .esi
                .resolve_names(&[part.to_owned()], tether_esi::Priority::Interactive)
                .await
                .map_err(crate::admin::esi_unavailable)?;
            let found: Vec<i64> = resolved
                .corporations
                .iter()
                .chain(resolved.alliances.iter())
                .map(|e| e.id)
                .collect();
            if found.is_empty() {
                return Err(AppError::bad_request(format!(
                    "No corporation or alliance is named exactly {part}."
                )));
            }
            // Keep the names for describing the filter later.
            tether_esi::names::resolve(
                &state.db,
                &state.esi,
                &found,
                tether_esi::Priority::Interactive,
            )
            .await
            .map_err(crate::admin::names_unavailable)?;
            ids.extend(found);
        }
    }
    ids.sort_unstable();
    ids.dedup();
    Ok(ids)
}

/// `POST /admin/groups/{id}/smart/filters`: one filter; `kind` says which,
/// `reversed` flips it.
pub async fn smart_filter(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(id): Path<i64>,
    Form(fields): Form<Fields>,
) -> Result<Response, PageError> {
    use tether_core::smart::Filter;
    let (session, shell) = guard(&state, session, ADMIN_GROUPS, "admin_groups").await?;
    let ids = |name: &str| -> Result<Vec<i64>, AppError> {
        let list: Vec<i64> = fields
            .iter()
            .filter(|(k, _)| k == name)
            .map(|(_, v)| v.parse())
            .collect::<Result<_, _>>()
            .map_err(|_| AppError::bad_request("Choose from the list."))?;
        if list.is_empty() {
            return Err(AppError::bad_request("Choose at least one."));
        }
        Ok(list)
    };
    let filter = match field(&fields, "kind") {
        "state" => ids("states").map(|states| Filter::State { states }),
        "main_affiliation" => entities(&state, field(&fields, "entities"))
            .await
            .map(|entities| Filter::MainAffiliation { entities }),
        "any_affiliation" => match (
            entities(&state, field(&fields, "entities")).await,
            exempt(&state, &fields).await,
        ) {
            (Ok(entities), Ok(exempt)) => Ok(Filter::AnyAffiliation { entities, exempt }),
            (Err(err), _) | (_, Err(err)) => Err(err),
        },
        "faction" => factions(&state, field(&fields, "factions"))
            .await
            .map(|factions| Filter::Faction { factions }),
        "service" => match field(&fields, "service") {
            "discord" => Ok(Filter::Service {
                service: tether_core::smart::Service::Discord,
            }),
            _ => Err(AppError::bad_request("Choose a service.")),
        },
        "character_age" => field(&fields, "days")
            .trim()
            .parse::<u32>()
            .ok()
            .filter(|d| (1..=36_500).contains(d))
            .map(|days| Filter::CharacterAge { days })
            .ok_or_else(|| AppError::bad_request("Give an age in days.")),
        "groups" => match (ids("groups"), exempt(&state, &fields).await) {
            (Ok(groups), Ok(exempt)) => Ok(Filter::Groups {
                groups,
                all: field(&fields, "match") == "all",
                exempt,
            }),
            (Err(err), _) | (_, Err(err)) => Err(err),
        },
        "compliant" => Ok(Filter::Compliant {}),
        "app" => app_filter(&state, &fields),
        _ => Err(AppError::bad_request("Choose a filter.")),
    };
    let result = match (filter, grace_days(&fields)) {
        (Ok(filter), Ok(grace_days)) => {
            crate::smart_groups::add_filter(
                &state.db,
                session.account,
                GroupId(id),
                filter,
                checked(&fields, "reversed"),
                grace_days,
            )
            .await
        }
        (Err(err), _) | (_, Err(err)) => Err(err),
    };
    on_group(&state, shell, id, result).await
}

/// A filter's grace period from the form: AA's 5 days when left empty.
fn grace_days(fields: &Fields) -> Result<i32, AppError> {
    match field(fields, "grace_days").trim() {
        "" => Ok(crate::smart_groups::DEFAULT_GRACE_DAYS),
        days => days.parse::<i32>().map_err(|_| {
            AppError::bad_request(format!(
                "A grace period is 0 to {} days.",
                crate::smart_groups::MAX_GRACE_DAYS
            ))
        }),
    }
}

/// Exempt corporations and alliances (AA's exemptions on its alt and
/// group filters), if any are named.
async fn exempt(state: &AppState, fields: &Fields) -> Result<Vec<i64>, AppError> {
    match field(fields, "exempt").trim() {
        "" => Ok(Vec::new()),
        text => entities(state, text).await,
    }
}

/// `POST /admin/groups/{id}/smart/filters/{filter}/grace`: one filter's
/// grace period (AA's grace period per filter).
pub async fn smart_filter_grace(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path((id, filter)): Path<(i64, i64)>,
    Form(fields): Form<Fields>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_GROUPS, "admin_groups").await?;
    let result = match field(&fields, "grace_days").trim().parse::<i32>() {
        Ok(days) => {
            crate::smart_groups::set_filter_grace(
                &state.db,
                session.account,
                GroupId(id),
                filter,
                days,
            )
            .await
        }
        Err(_) => Err(AppError::bad_request(format!(
            "A grace period is 0 to {} days.",
            crate::smart_groups::MAX_GRACE_DAYS
        ))),
    };
    on_group(&state, shell, id, result).await
}

/// `POST /admin/groups/{id}/smart/filters/combine`: two filters become one
/// expression (AA's filter expression: AND, OR or XOR, maybe negated).
pub async fn smart_filter_combine(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(id): Path<i64>,
    Form(fields): Form<Fields>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_GROUPS, "admin_groups").await?;
    let first = field(&fields, "first").parse::<i64>();
    let second = field(&fields, "second").parse::<i64>();
    let operator = tether_core::smart::Operator::parse(field(&fields, "operator"));
    let result = match (first, second, operator) {
        (Ok(first), Ok(second), Some(operator)) => {
            crate::smart_groups::combine(
                &state.db,
                session.account,
                GroupId(id),
                first,
                second,
                operator,
                checked(&fields, "negate"),
            )
            .await
        }
        _ => Err(AppError::bad_request(
            "Choose two filters and how to combine them.",
        )),
    };
    on_group(&state, shell, id, result).await
}

/// An app's filter from the form: `app` is `plugin/filter`; each field is
/// `f_<name>`; `at_least` for filters that add up.
fn app_filter(state: &AppState, fields: &Fields) -> Result<tether_core::smart::Filter, AppError> {
    let (plugin, name) = field(fields, "app")
        .split_once('/')
        .ok_or_else(|| AppError::bad_request("Choose an app's filter."))?;
    let running = state
        .plugins
        .running(plugin)
        .ok_or_else(|| AppError::bad_request("That app isn't running."))?;
    let spec = running
        .manifest
        .filters
        .iter()
        .find(|f| f.name == name)
        .ok_or_else(|| AppError::bad_request("That app has no such filter."))?;
    let mut config = std::collections::BTreeMap::new();
    let mut shown = Vec::new();
    for f in &spec.fields {
        let raw = field(fields, &format!("f_{}", f.name)).trim();
        if raw.is_empty() || raw.chars().count() > 100 || raw.chars().any(char::is_control) {
            return Err(AppError::bad_request(format!(
                "Fill in {} (at most 100 characters).",
                f.label
            )));
        }
        let value = match f.kind {
            tether_plugins::manifest::FieldKind::Number => serde_json::Value::from(
                raw.parse::<i64>()
                    .map_err(|_| AppError::bad_request(format!("{} is a number.", f.label)))?,
            ),
            tether_plugins::manifest::FieldKind::Text => serde_json::Value::from(raw),
        };
        shown.push(format!("{}: {raw}", f.label));
        config.insert(f.name.clone(), value);
    }
    let sum = spec.combine == tether_plugins::manifest::Combine::Sum;
    let at_least = if sum {
        field(fields, "at_least")
            .trim()
            .parse::<i64>()
            .ok()
            .filter(|n| *n >= 1)
            .ok_or_else(|| AppError::bad_request("Give a total of at least 1."))?
    } else {
        0
    };
    let label = if shown.is_empty() {
        format!("{}: {}", running.manifest.plugin.name, spec.label)
    } else {
        format!(
            "{}: {} ({})",
            running.manifest.plugin.name,
            spec.label,
            shown.join(", ")
        )
    };
    Ok(tether_core::smart::Filter::App {
        plugin: plugin.to_owned(),
        name: name.to_owned(),
        config: serde_json::to_string(&config).map_err(AppError::internal)?,
        sum,
        at_least,
        label,
    })
}

/// `POST /admin/groups/{id}/smart/filters/{filter}/delete`
pub async fn smart_filter_delete(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path((id, filter)): Path<(i64, i64)>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_GROUPS, "admin_groups").await?;
    let result =
        crate::smart_groups::delete_filter(&state.db, session.account, GroupId(id), filter).await;
    on_group(&state, shell, id, result).await
}

/// `POST /admin/groups/{id}/settings`
pub async fn group_settings(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(id): Path<i64>,
    Form(fields): Form<Fields>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_GROUPS, "admin_groups").await?;
    let states = fields
        .iter()
        .filter(|(k, _)| k == "states")
        .map(|(_, v)| v.parse().map(tether_core::states::StateId))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| AppError::bad_request("Choose states from the list."));
    let result = match states {
        Ok(states) => {
            crate::groups::update(
                &state.db,
                session.account,
                GroupId(id),
                crate::groups::Settings {
                    description: field(&fields, "description").to_owned(),
                    flags: flags_from(&fields),
                    compliance: checked(&fields, "compliance"),
                    states,
                },
            )
            .await
        }
        Err(err) => Err(err),
    };
    on_group(&state, shell, id, result).await
}

/// `POST /admin/groups/{id}/delete`
pub async fn delete_group(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(id): Path<i64>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_GROUPS, "admin_groups").await?;
    match crate::groups::delete(&state.db, session.account, GroupId(id)).await {
        Ok(()) => Ok(super::stay::back("/admin/groups", "Group deleted.")),
        Err(err) => group_page(&state, shell, id, Extra::error(err)).await,
    }
}

#[derive(Debug, Deserialize)]
pub struct CharacterForm {
    character: String,
}

async fn find_account(state: &AppState, character: &str) -> Result<AccountId, AppError> {
    accounts::find(&state.db, character)
        .await?
        .ok_or_else(|| AppError::not_found("No account has a character with that name."))
}

/// `POST /admin/groups/{id}/members`: by character name or id.
pub async fn add_member(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(id): Path<i64>,
    Form(form): Form<CharacterForm>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_GROUPS, "admin_groups").await?;
    let result = match find_account(&state, &form.character).await {
        Ok(account) => {
            crate::groups::add_member(&state.db, session.account, GroupId(id), account).await
        }
        Err(err) => Err(err),
    };
    on_group(&state, shell, id, result).await
}

/// `POST /admin/groups/{id}/members/{account_id}/remove`
pub async fn remove_member(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path((id, account_id)): Path<(i64, i64)>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_GROUPS, "admin_groups").await?;
    let result = crate::groups::remove_member(
        &state.db,
        session.account,
        GroupId(id),
        AccountId(account_id),
    )
    .await;
    on_group(&state, shell, id, result).await
}

/// `POST /admin/groups/{id}/leaders`: by character name or id.
pub async fn add_leader(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(id): Path<i64>,
    Form(form): Form<CharacterForm>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_GROUPS, "admin_groups").await?;
    let result = match find_account(&state, &form.character).await {
        Ok(account) => {
            crate::groups::set_leader(&state.db, session.account, GroupId(id), account, true).await
        }
        Err(err) => Err(err),
    };
    on_group(&state, shell, id, result).await
}

/// `POST /admin/groups/{id}/leaders/{account_id}/remove`
pub async fn remove_leader(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path((id, account_id)): Path<(i64, i64)>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_GROUPS, "admin_groups").await?;
    let result = crate::groups::set_leader(
        &state.db,
        session.account,
        GroupId(id),
        AccountId(account_id),
        false,
    )
    .await;
    on_group(&state, shell, id, result).await
}

#[derive(Debug, Deserialize)]
pub struct LeaderGroupForm {
    group_id: i64,
}

/// `POST /admin/groups/{id}/leader-groups`
pub async fn add_leader_group(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(id): Path<i64>,
    Form(form): Form<LeaderGroupForm>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_GROUPS, "admin_groups").await?;
    let result = crate::groups::set_leader_group(
        &state.db,
        session.account,
        GroupId(id),
        GroupId(form.group_id),
        true,
    )
    .await;
    on_group(&state, shell, id, result).await
}

/// `POST /admin/groups/{id}/leader-groups/{leader_group_id}/remove`
pub async fn remove_leader_group(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path((id, leader_group_id)): Path<(i64, i64)>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_GROUPS, "admin_groups").await?;
    let result = crate::groups::set_leader_group(
        &state.db,
        session.account,
        GroupId(id),
        GroupId(leader_group_id),
        false,
    )
    .await;
    on_group(&state, shell, id, result).await
}

// ---- permissions -----------------------------------------------------------

pub struct GrantBadge {
    pub id: i64,
    pub label: String,
    /// `state`, `group` or `user`.
    pub kind: &'static str,
    /// A user grant's account, for its link.
    pub account_id: i64,
}

pub struct PermissionRow {
    pub name: String,
    pub description: String,
    pub grants: Vec<GrantBadge>,
    /// Its states and groups, as picker values (`state:<id>`,
    /// `group:<id>`).
    pub holders: Vec<String>,
    /// An admin permission (or another that mustn't go where anyone can
    /// be): not to Guest, public states or Open groups.
    pub sensitive: bool,
}

impl PermissionRow {
    pub fn holds(&self, value: &str) -> bool {
        self.holders.iter().any(|h| h == value)
    }

    /// The row's element id, for editing it in place: the name with every
    /// character but a lowercase letter or digit spelled `_<hex>_`, so two
    /// names never share one.
    pub fn dom_id(&self) -> String {
        let mut id = String::from("perm-");
        for c in self.name.chars() {
            if c.is_ascii_lowercase() || c.is_ascii_digit() {
                id.push(c);
            } else {
                id.push_str(&format!("_{:x}_", u32::from(c)));
            }
        }
        id
    }
}

pub struct GroupOption {
    pub id: i64,
    pub name: String,
}

/// A state or group in a row's picker.
pub struct GrantChoice {
    /// `state:<id>` or `group:<id>`.
    pub value: String,
    pub label: String,
    pub is_group: bool,
    /// Anyone can be in it (Guest, a public or blacklist state, an Open
    /// group): sensitive permissions can't go there.
    pub open_to_anyone: bool,
}

#[derive(Template)]
#[template(path = "admin_permissions.html")]
struct PermissionsPage {
    shell: Shell,
    rows: Vec<PermissionRow>,
    choices: Vec<GrantChoice>,
    /// The filter, as typed.
    q: String,
    /// How many permissions there are, filtered or not.
    total: usize,
    error: Option<String>,
}

impl PermissionsPage {
    fn has_groups(&self) -> bool {
        self.choices.iter().any(|c| c.is_group)
    }
}

fn grantee_value(grantee: Grantee) -> Option<String> {
    match grantee {
        Grantee::State(id) => Some(format!("state:{}", id.0)),
        Grantee::Group(id) => Some(format!("group:{}", id.0)),
        Grantee::Account(_) => None,
    }
}

async fn permissions_page(
    state: &AppState,
    shell: Shell,
    q: &str,
    error: Option<AppError>,
) -> Result<Response, PageError> {
    let grants = permissions::list(&state.db).await?;
    let all_states = tether_db::states::all(&state.db).await?;
    let states = state_options(state).await?;
    let all_groups = groups::summaries(&state.db).await?;
    let group_name = |id: GroupId| {
        all_groups
            .iter()
            .find(|g| g.group.id == id)
            .map_or_else(|| format!("group {}", id.0), |g| g.group.name.clone())
    };
    let mut user_names = std::collections::HashMap::new();
    for g in &grants {
        if let Grantee::Account(account) = g.grantee
            && !user_names.contains_key(&account)
        {
            let name = tether_db::accounts::main_name(&state.db, account)
                .await?
                .unwrap_or_else(|| format!("account {} (no main)", account.0));
            user_names.insert(account, name);
        }
    }
    let available = permissions::available(&state.db).await?;
    let total = available.len();
    let needle = q.trim().to_lowercase();
    let rows = available
        .into_iter()
        .filter(|(name, description)| {
            needle.is_empty()
                || name.to_lowercase().contains(&needle)
                || description.to_lowercase().contains(&needle)
        })
        .map(|(name, description)| {
            let mine: Vec<&permissions::Grant> =
                grants.iter().filter(|g| g.permission == name).collect();
            PermissionRow {
                grants: mine
                    .iter()
                    .map(|g| match g.grantee {
                        Grantee::State(id) => GrantBadge {
                            id: g.id,
                            label: state_name(&states, id),
                            kind: "state",
                            account_id: 0,
                        },
                        Grantee::Group(group) => GrantBadge {
                            id: g.id,
                            label: group_name(group),
                            kind: "group",
                            account_id: 0,
                        },
                        Grantee::Account(account) => GrantBadge {
                            id: g.id,
                            label: user_names.get(&account).cloned().unwrap_or_default(),
                            kind: "user",
                            account_id: account.0,
                        },
                    })
                    .collect(),
                holders: mine
                    .iter()
                    .filter_map(|g| grantee_value(g.grantee))
                    .collect(),
                sensitive: tether_core::permissions::is_sensitive(&name),
                name,
                description,
            }
        })
        .collect();
    let choices = all_states
        .iter()
        .map(|s| GrantChoice {
            value: format!("state:{}", s.id.0),
            label: s.name.clone(),
            is_group: false,
            open_to_anyone: s.is_guest() || s.is_blacklist() || s.public,
        })
        .chain(all_groups.iter().map(|g| GrantChoice {
            value: format!("group:{}", g.group.id.0),
            label: g.group.name.clone(),
            is_group: true,
            open_to_anyone: g.group.flags.anyone_can_join(),
        }))
        .collect();
    let status = error.as_ref().map_or(StatusCode::OK, AppError::status);
    let problem = error.as_ref().map(|e| e.message().to_owned());
    let page = PermissionsPage {
        shell,
        rows,
        choices,
        q: q.to_owned(),
        total,
        error: error.map(|e| e.message().to_owned()),
    };
    Ok(super::with_problem(problem, render(status, &page)))
}

#[derive(Debug, Deserialize)]
pub struct PermissionsQuery {
    #[serde(default)]
    q: String,
}

/// `GET /admin/permissions`
pub async fn permissions(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Query(query): Query<PermissionsQuery>,
) -> Result<Response, PageError> {
    let (_, shell) = guard(&state, session, ADMIN_PERMISSIONS, "permissions").await?;
    let q: String = query.q.chars().take(100).collect();
    permissions_page(&state, shell, &q, None).await
}

/// A `<select>` or picker value: `state:<id>` or `group:<id>`.
pub fn parse_grantee(value: &str) -> Result<Grantee, AppError> {
    let choose = || AppError::bad_request("Choose a state or a group.");
    match value.split_once(':') {
        Some(("state", id)) => id
            .parse()
            .map_err(|_| choose())
            .and_then(|id| admin::grantee(Some(id), None, None)),
        Some(("group", id)) => id
            .parse()
            .map_err(|_| choose())
            .and_then(|id| admin::grantee(None, Some(id), None)),
        _ => Err(choose()),
    }
}

/// `POST /admin/permissions/set`: one permission's states and groups, as
/// its row's picker saves them. What was ticked when the page was drawn
/// (`was`) is compared with what is ticked now (`grantee`): only those
/// changes are made, so a grant someone else made meanwhile stays. Each is
/// its own grant or revoke, checked and audited as ever.
pub async fn set_grants(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Form(fields): Form<Fields>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_PERMISSIONS, "permissions").await?;
    match apply_grants(&state, session.account, &fields).await {
        Ok(done) => Ok(super::stay::back("/admin/permissions", done)),
        Err(err) => permissions_page(&state, shell, "", Some(err)).await,
    }
}

async fn apply_grants(
    state: &AppState,
    actor: AccountId,
    fields: &Fields,
) -> Result<String, AppError> {
    let permission = field(fields, "permission");
    if permission.is_empty() || permission.len() > 200 {
        return Err(AppError::bad_request("Choose a permission."));
    }
    // Capped before anything else is done with them.
    let values = |name: &str| -> Result<Vec<Grantee>, AppError> {
        let posted: Vec<&str> = fields
            .iter()
            .filter(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
            .collect();
        if posted.len() > 200 {
            return Err(AppError::bad_request("Too many states and groups at once."));
        }
        let mut out: Vec<Grantee> = Vec::new();
        for value in posted {
            let grantee = parse_grantee(value)?;
            if !out.contains(&grantee) {
                out.push(grantee);
            }
        }
        Ok(out)
    };
    let (was, now) = (values("was")?, values("grantee")?);
    let states = state_options(state).await?;
    let all_groups = groups::summaries(&state.db).await?;
    let label = |grantee: Grantee| match grantee {
        Grantee::State(id) => state_name(&states, id),
        Grantee::Group(id) => all_groups
            .iter()
            .find(|g| g.group.id == id)
            .map_or_else(|| format!("group {}", id.0), |g| g.group.name.clone()),
        Grantee::Account(_) => String::new(),
    };
    let current = permissions::list(&state.db).await?;
    let mut revoked: Vec<String> = Vec::new();
    let mut granted: Vec<String> = Vec::new();
    // What was done before a refusal is said along with it: grants are
    // access, and the admin must know where things stand.
    let stopped = |err: AppError, granted: &[String], revoked: &[String]| {
        if granted.is_empty() && revoked.is_empty() {
            err
        } else {
            let done = summary(permission, granted, revoked);
            AppError::new(err.status(), format!("{done} Then: {}", err.message()))
        }
    };
    for gone in was.iter().filter(|g| !now.contains(g)) {
        // Already gone (someone else revoked it) is what was asked for.
        let Some(grant) = current
            .iter()
            .find(|g| g.permission == permission && g.grantee == *gone)
        else {
            continue;
        };
        match admin::revoke(state, actor, grant.id).await {
            Ok(()) => revoked.push(label(*gone)),
            Err(err) if err.status() == StatusCode::NOT_FOUND => {}
            Err(err) => return Err(stopped(err, &granted, &revoked)),
        }
    }
    for new in now.iter().filter(|g| !was.contains(g)) {
        match admin::grant(state, actor, permission, *new).await {
            Ok(_) => granted.push(label(*new)),
            // Someone else granted it meanwhile: it's what was asked for.
            Err(err) if err.status() == StatusCode::CONFLICT => {}
            Err(err) => return Err(stopped(err, &granted, &revoked)),
        }
    }
    Ok(summary(permission, &granted, &revoked))
}

fn summary(permission: &str, granted: &[String], revoked: &[String]) -> String {
    match (granted.is_empty(), revoked.is_empty()) {
        (true, true) => format!("No change to {permission}."),
        (false, true) => format!("{permission} granted to {}.", granted.join(", ")),
        (true, false) => format!("{permission} revoked from {}.", revoked.join(", ")),
        (false, false) => format!(
            "{permission} granted to {}; revoked from {}.",
            granted.join(", "),
            revoked.join(", ")
        ),
    }
}

/// `POST /admin/permissions/{grant_id}/revoke`
pub async fn revoke(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(grant_id): Path<i64>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_PERMISSIONS, "permissions").await?;
    match admin::revoke(&state, session.account, grant_id).await {
        Ok(()) => Ok(super::stay::back("/admin/permissions", "Revoked.")),
        Err(err) => permissions_page(&state, shell, "", Some(err)).await,
    }
}
