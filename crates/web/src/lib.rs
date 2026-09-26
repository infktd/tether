//! HTTP routes, auth middleware and the OpenAPI document.

// dev-login creates sessions without SSO, and dev-docs loads Scalar from a
// CDN. Neither may reach a release build; CI checks these guards fire.
#[cfg(all(feature = "dev-login", not(debug_assertions)))]
compile_error!("the dev-login feature must never be enabled in release builds");
#[cfg(all(feature = "dev-docs", not(debug_assertions)))]
compile_error!("the dev-docs feature must never be enabled in release builds");
// plugin-http-test sends every plugin HTTP request to a local stand-in
// (tests only).
#[cfg(all(feature = "plugin-http-test", not(debug_assertions)))]
compile_error!("the plugin-http-test feature must never be enabled in release builds");

pub mod admin;
pub mod admin_nav;
mod api;
pub mod auth;
pub mod autogroups;
pub mod backups;
pub mod blacklist;
pub mod compliance;
mod csrf;
#[cfg(feature = "dev-login")]
mod dev_login;
pub mod discord;
pub mod discord_sync;
mod error;
pub mod groups;
pub mod maintenance;
pub mod menu;
pub mod notifications;
pub mod openapi;
pub mod ownership;
pub mod pages;
pub mod personal_tokens;
pub mod pings;
pub mod plugin_consent;
pub mod plugin_github;
pub mod plugin_http;
pub mod plugin_jobs;
pub mod plugin_services;
pub mod plugin_shared;
pub mod plugins;
mod ratelimit;
pub mod setup;
pub mod smart_groups;
mod state;
pub mod state_admin;
pub mod states;
pub mod sync;
pub mod theme;
pub mod tokens;
pub mod updates;

use axum::extract::{DefaultBodyLimit, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, patch, post, put};
use axum::{Router, middleware};

pub use state::{AppState, Limits, Site};

/// Whether this build includes fixture logins (never true in release).
pub const DEV_LOGIN: bool = cfg!(feature = "dev-login");

pub fn router(state: AppState) -> Router {
    let router = Router::new();
    #[cfg(feature = "dev-login")]
    let router = router
        .route("/dev/login", get(dev_login::list))
        .route("/dev/login/{fixture}", get(dev_login::login));
    #[cfg(feature = "dev-docs")]
    let router = router.route("/docs", get(openapi::docs));
    router
        .route("/api/openapi.json", get(openapi::spec))
        .route("/", get(pages::home))
        .route("/login", get(pages::login))
        .route("/dashboard", get(pages::profile))
        .route("/dashboard/system", get(pages::system::dashboard_panel))
        .route(
            "/dashboard/access-tokens",
            get(pages::access_tokens::index).post(pages::access_tokens::create),
        )
        .route(
            "/dashboard/access-tokens/{id}/revoke",
            post(pages::access_tokens::revoke),
        )
        .route(
            "/dashboard/widgets/{plugin}/{index}",
            get(pages::plugin_pages::widget),
        )
        // The old name (before AA's): kept so bookmarks still work.
        .route("/profile", get(pages::to_dashboard))
        .route("/profile/main", post(pages::make_main))
        .route("/profile/main/login", post(pages::change_main_login))
        .route("/setup", get(pages::setup::page))
        .route("/setup/unlock", post(pages::setup::unlock))
        .route("/setup/sso", post(pages::setup::sso))
        .route("/setup/check", post(pages::setup::check))
        .route("/setup/alliance", post(pages::setup::choose_alliance))
        .route("/setup/alliance/search", post(pages::setup::search))
        .route("/static/{*path}", get(pages::assets::serve))
        .route("/theme.css", get(theme::stylesheet))
        .route("/admin", get(pages::admin::index))
        .route("/tokens", get(pages::tokens::index))
        .route(
            "/tokens/{character_id}/refresh",
            post(pages::tokens::refresh),
        )
        .route("/tokens/{character_id}/delete", post(pages::tokens::delete))
        .route("/notifications", get(pages::notifications::index))
        .route(
            "/notifications/read-all",
            post(pages::notifications::read_all),
        )
        .route(
            "/notifications/delete-read",
            post(pages::notifications::delete_read),
        )
        .route("/notifications/stream", get(pages::notifications::stream))
        .route("/notifications/{id}", get(pages::notifications::show))
        .route(
            "/notifications/{id}/delete",
            post(pages::notifications::delete),
        )
        .route("/corpstats", get(pages::corpstats::index))
        .route("/corpstats/{id}", get(pages::corpstats::show))
        .route("/corpstats/{id}/update", post(pages::corpstats::update))
        .route("/groups", get(pages::groups::index))
        .route("/groups/{id}", get(pages::groups::direct))
        .route("/groups/{id}/join", post(pages::groups::join))
        .route("/groups/{id}/leave", post(pages::groups::leave))
        .route("/groups/{id}/retract", post(pages::groups::retract))
        .route("/group-management", get(pages::groups::requests))
        .route(
            "/group-management/membership",
            get(pages::groups::membership),
        )
        .route("/group-management/{id}", get(pages::groups::members))
        .route("/group-management/{id}/audit", get(pages::groups::audit))
        .route(
            "/group-management/{id}/requests/{account_id}/accept",
            post(pages::groups::accept),
        )
        .route(
            "/group-management/{id}/requests/{account_id}/reject",
            post(pages::groups::reject),
        )
        .route(
            "/group-management/{id}/members/{account_id}/remove",
            post(pages::groups::remove),
        )
        .route(
            "/admin/groups",
            get(pages::admin::groups).post(pages::admin::create_group),
        )
        .route(
            "/admin/autogroups",
            get(pages::autogroups::index).post(pages::autogroups::create),
        )
        .route("/admin/autogroups/{id}", post(pages::autogroups::update))
        .route(
            "/admin/autogroups/{id}/delete",
            post(pages::autogroups::delete),
        )
        .route("/admin/groups/settings", post(pages::admin::group_options))
        .route("/admin/groups/reserved", post(pages::admin::reserve))
        .route(
            "/admin/groups/reserved/remove",
            post(pages::admin::unreserve),
        )
        .route("/admin/groups/{id}", get(pages::admin::group))
        .route(
            "/admin/groups/{id}/settings",
            post(pages::admin::group_settings),
        )
        .route(
            "/admin/groups/{id}/delete",
            post(pages::admin::delete_group),
        )
        .route("/admin/groups/{id}/members", post(pages::admin::add_member))
        .route(
            "/admin/groups/{id}/members/{account_id}/remove",
            post(pages::admin::remove_member),
        )
        .route("/admin/groups/{id}/leaders", post(pages::admin::add_leader))
        .route(
            "/admin/groups/{id}/leaders/{account_id}/remove",
            post(pages::admin::remove_leader),
        )
        .route(
            "/admin/groups/{id}/leader-groups",
            post(pages::admin::add_leader_group),
        )
        .route(
            "/admin/groups/{id}/leader-groups/{leader_group_id}/remove",
            post(pages::admin::remove_leader_group),
        )
        .route("/admin/permissions", get(pages::admin::permissions))
        .route("/admin/users", get(pages::users::index))
        .route("/admin/users/{id}", get(pages::users::show))
        .route(
            "/admin/users/{id}/deactivate",
            post(pages::users::deactivate),
        )
        .route(
            "/admin/users/{id}/reactivate",
            post(pages::users::reactivate),
        )
        .route(
            "/admin/permissions/audit",
            get(pages::permissions_audit::index),
        )
        .route(
            "/admin/permissions/audit/{permission}",
            get(pages::permissions_audit::show),
        )
        .route("/admin/permissions/grant", post(pages::admin::grant))
        .route(
            "/admin/permissions/{grant_id}/revoke",
            post(pages::admin::revoke),
        )
        .route(
            "/admin/states",
            get(pages::states::page).post(pages::states::create),
        )
        .route("/admin/states/search", post(pages::states::search))
        .route("/admin/states/{id}/rename", post(pages::states::rename))
        .route("/admin/states/{id}/delete", post(pages::states::delete))
        .route("/admin/states/{id}/move", post(pages::states::move_state))
        .route(
            "/admin/states/{id}/priority",
            post(pages::states::set_priority),
        )
        .route("/admin/states/{id}/covers", post(pages::states::add))
        .route(
            "/admin/states/{id}/covers/{entity_id}/remove",
            post(pages::states::remove),
        )
        .route(
            "/admin/discord",
            get(pages::discord::admin).post(pages::discord::save_settings),
        )
        .route(
            "/admin/discord/names",
            post(pages::discord::save_name_format),
        )
        .route("/admin/discord/options", post(pages::discord::save_options))
        .route("/admin/discord/mappings", post(pages::discord::add_mapping))
        .route(
            "/admin/discord/mappings/{id}/remove",
            post(pages::discord::remove_mapping),
        )
        .route("/admin/discord/channels", post(pages::discord::add_channel))
        .route(
            "/admin/discord/channels/{id}/remove",
            post(pages::discord::remove_channel),
        )
        .route(
            "/pings",
            get(pages::pings::pings_page).post(pages::pings::send),
        )
        .route("/pings/preview", post(pages::pings::preview))
        .route("/admin/pings", get(pages::pings::settings))
        .route("/admin/pings/settings", post(pages::pings::save_settings))
        .route("/admin/pings/options", post(pages::pings::add_option))
        .route(
            "/admin/pings/options/{id}/delete",
            post(pages::pings::delete_option),
        )
        .route("/admin/pings/restrictions", post(pages::pings::restrict))
        .route(
            "/admin/pings/restrictions/{id}/remove",
            post(pages::pings::unrestrict),
        )
        .route("/admin/system", get(pages::system::system))
        .route("/admin/system/updates", post(pages::system::set_updates))
        .route("/admin/system/theme", post(pages::system::set_theme))
        .route(
            "/blacklist",
            get(pages::blacklist::index).post(pages::blacklist::add),
        )
        .route(
            "/blacklist/{entity_id}/remove",
            post(pages::blacklist::remove),
        )
        .route("/blacklist/notes", post(pages::blacklist::add_note))
        .route(
            "/blacklist/notes/{id}/delete",
            post(pages::blacklist::delete_note),
        )
        .route(
            "/admin/groups/{id}/smart",
            post(pages::admin::smart_settings),
        )
        .route(
            "/admin/groups/{id}/smart/filters",
            post(pages::admin::smart_filter),
        )
        .route(
            "/admin/groups/{id}/smart/filters/{filter}/delete",
            post(pages::admin::smart_filter_delete),
        )
        .route("/admin/menu", get(pages::menu::index))
        .route("/admin/menu/sections", post(pages::menu::add_section))
        .route("/admin/menu/folders", post(pages::menu::add_folder))
        .route("/admin/menu/links", post(pages::menu::add_link))
        .route("/admin/menu/change", post(pages::menu::change))
        .route("/admin/menu/move", post(pages::menu::move_entry))
        .route("/admin/menu/delete", post(pages::menu::delete))
        .route("/admin/menu/reset", post(pages::menu::reset))
        .route(
            "/admin/system/updates/check",
            post(pages::system::check_updates),
        )
        .route("/admin/jobs/{id}/retry", post(pages::system::retry_job))
        .route("/admin/audit", get(pages::system::audit_log))
        // Uploads and keys live outside /admin/plugins/, so no plugin id
        // can collide with their routes.
        .route(
            "/admin/plugins",
            get(pages::plugins::list).merge(
                post(pages::plugins::upload)
                    .layer(DefaultBodyLimit::max(pages::plugins::UPLOAD_BODY_LIMIT)),
            ),
        )
        .route("/admin/plugin-github", post(pages::plugins::install_github))
        .route("/admin/plugin-uploads/{id}", get(pages::plugins::review))
        .route(
            "/admin/plugin-uploads/{id}/approve",
            post(pages::plugins::approve),
        )
        .route(
            "/admin/plugin-uploads/{id}/discard",
            post(pages::plugins::discard),
        )
        .route(
            "/admin/plugin-keys/{id}",
            get(pages::plugins::key).post(pages::plugins::repin),
        )
        .route(
            "/plugins/{id}",
            get(pages::plugin_pages::main_page).merge(
                post(pages::plugin_pages::post_main)
                    .layer(DefaultBodyLimit::max(pages::plugin_pages::MAX_FORM_BYTES)),
            ),
        )
        .route(
            "/plugins/{id}/{*path}",
            get(pages::plugin_pages::sub_page).merge(
                post(pages::plugin_pages::post_sub)
                    .layer(DefaultBodyLimit::max(pages::plugin_pages::MAX_FORM_BYTES)),
            ),
        )
        .route("/admin/plugins/{id}", get(pages::plugins::plugin))
        .route(
            "/admin/plugins/{id}/sources/{character}/approve",
            post(pages::plugins::approve_source),
        )
        .route(
            "/admin/plugins/{id}/sources/{character}/remove",
            post(pages::plugins::remove_source),
        )
        .route(
            "/admin/plugins/{id}/channels",
            post(pages::plugins::assign_channel),
        )
        .route(
            "/admin/plugins/{id}/channels/{channel}/remove",
            post(pages::plugins::remove_channel),
        )
        .route(
            "/admin/plugins/{id}/secrets/{name}",
            post(pages::plugins::set_secret),
        )
        .route("/admin/plugins/{id}/enable", post(pages::plugins::enable))
        .route("/admin/plugins/{id}/disable", post(pages::plugins::disable))
        .route("/admin/plugins/{id}/update", post(pages::plugins::update))
        .route(
            "/admin/plugins/{id}/source",
            post(pages::plugins::set_source),
        )
        .route(
            "/admin/plugins/{id}/rollback",
            post(pages::plugins::roll_back),
        )
        .route(
            "/admin/plugins/{id}/uninstall",
            post(pages::plugins::uninstall),
        )
        .route("/register", get(pages::compliance::register))
        .route("/register/start", post(pages::compliance::start))
        .route("/profile/corp-stats/offer", post(pages::compliance::offer))
        .route(
            "/profile/corp-stats/{character}/withdraw",
            post(pages::compliance::withdraw),
        )
        .route("/compliance", get(pages::compliance::page))
        .route(
            "/compliance/sources/{character}/approve",
            post(pages::compliance::approve_source),
        )
        .route(
            "/compliance/sources/{character}/remove",
            post(pages::compliance::remove_source),
        )
        .route("/admin/states/{id}/scopes", post(pages::states::add_scope))
        .route(
            "/admin/states/{id}/scopes/remove",
            post(pages::states::remove_scope),
        )
        .route(
            "/profile/plugins/{id}/offer",
            post(pages::plugin_access::offer),
        )
        .route(
            "/profile/plugins/{id}/offer/{character}/withdraw",
            post(pages::plugin_access::withdraw),
        )
        .route("/services", get(pages::discord::services))
        .route("/services/discord/link", post(pages::discord::link))
        .route("/services/discord/unlink", post(pages::discord::unlink))
        .route("/discord/callback", get(pages::discord::callback))
        .route("/health", get(health))
        .route("/ready", get(ready))
        .route("/auth/login", get(auth::login))
        .route("/auth/callback", get(auth::callback))
        .route("/auth/logout", post(auth::logout))
        .route("/api/me", get(api::me))
        .route("/api/me/main", post(api::set_main))
        .route("/api/tokens", get(api::tokens::list))
        .route(
            "/api/tokens/{character_id}/refresh",
            post(api::tokens::refresh),
        )
        .route("/api/tokens/{character_id}", delete(api::tokens::delete))
        .route("/api/notifications", get(api::notifications::list))
        .route("/api/notifications/unread", get(api::notifications::unread))
        .route(
            "/api/notifications/read-all",
            post(api::notifications::read_all),
        )
        .route(
            "/api/notifications/delete-read",
            post(api::notifications::delete_read),
        )
        .route(
            "/api/notifications/{id}/open",
            post(api::notifications::open),
        )
        .route(
            "/api/notifications/{id}",
            delete(api::notifications::delete),
        )
        .route("/api/groups", get(api::groups::list))
        .route("/api/groups/{id}/join", post(api::groups::join))
        .route("/api/groups/{id}/leave", post(api::groups::leave))
        .route("/api/groups/{id}/retract", post(api::groups::retract))
        .route(
            "/api/group-management/requests",
            get(api::group_management::requests),
        )
        .route(
            "/api/group-management/groups/{id}/requests/{account_id}/accept",
            post(api::group_management::accept),
        )
        .route(
            "/api/group-management/groups/{id}/requests/{account_id}/reject",
            post(api::group_management::reject),
        )
        .route(
            "/api/group-management/groups/{id}/members",
            get(api::group_management::members),
        )
        .route(
            "/api/group-management/groups/{id}/members/{account_id}",
            delete(api::group_management::remove_member),
        )
        .route(
            "/api/group-management/groups/{id}/audit-log",
            get(api::group_management::audit_log),
        )
        .route("/api/admin/groups", post(api::admin::create_group))
        .route(
            "/api/admin/groups/{id}",
            put(api::admin::update_group).delete(api::admin::delete_group),
        )
        .route(
            "/api/admin/groups/{id}/members",
            post(api::admin::add_member),
        )
        .route(
            "/api/admin/groups/{id}/members/{account_id}",
            delete(api::admin::remove_member),
        )
        .route(
            "/api/admin/groups/{id}/leaders/{account_id}",
            put(api::admin::add_leader).delete(api::admin::remove_leader),
        )
        .route(
            "/api/admin/groups/{id}/leader-groups/{leader_group_id}",
            put(api::admin::add_leader_group).delete(api::admin::remove_leader_group),
        )
        .route(
            "/api/admin/reserved-group-names",
            get(api::admin::reserved_names).post(api::admin::reserve_name),
        )
        .route(
            "/api/admin/reserved-group-names/{name}",
            delete(api::admin::unreserve_name),
        )
        .route("/api/admin/permissions", get(api::admin::list_permissions))
        .route("/api/admin/permissions/grants", post(api::admin::grant))
        .route(
            "/api/admin/permissions/grants/{id}",
            delete(api::admin::revoke),
        )
        .route("/api/admin/audit", get(api::admin::audit_log))
        .route(
            "/api/admin/accounts/{id}/deactivate",
            post(api::admin::deactivate_account),
        )
        .route(
            "/api/admin/accounts/{id}/reactivate",
            post(api::admin::reactivate_account),
        )
        .route(
            "/api/admin/states",
            get(api::admin::list_states).post(api::admin::create_state),
        )
        .route(
            "/api/admin/states/{id}",
            patch(api::admin::rename_state).delete(api::admin::delete_state),
        )
        .route("/api/admin/states/{id}/move", post(api::admin::move_state))
        .route("/api/admin/states/{id}/covers", post(api::admin::add_cover))
        .route(
            "/api/admin/states/{id}/covers/{entity_id}",
            delete(api::admin::remove_cover),
        )
        .route("/api/admin/states/{id}/scopes", post(api::admin::add_scope))
        .route(
            "/api/admin/states/{id}/scopes/{scope}",
            delete(api::admin::remove_scope),
        )
        .route("/api/admin/states/resolve", post(api::admin::resolve_names))
        .route("/api/setup", get(setup::status))
        .route("/api/setup/unlock", post(setup::unlock))
        .route("/api/setup/sso", post(setup::set_sso))
        .route("/api/setup/probe", get(setup::probe))
        .route("/api/setup/callback-check", post(setup::callback_check))
        .fallback(pages::not_found)
        // Inside the origin check: a cross-site post is refused first.
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth::sign_in_first,
        ))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            csrf::verify_origin,
        ))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth::token_layer,
        ))
        // Outermost, so every response carries them, rejections included.
        .layer(middleware::from_fn_with_state(
            state.clone(),
            pages::headers::security_headers,
        ))
        .with_state(state)
}

/// Liveness: the process is up and serving HTTP.
#[utoipa::path(get, path = "/health", tag = "health",
    responses((status = 200, body = String, content_type = "text/plain", example = "ok")))]
async fn health() -> &'static str {
    "ok"
}

/// Readiness: the database answers.
#[utoipa::path(get, path = "/ready", tag = "health",
    responses((status = 200, body = String, content_type = "text/plain", example = "ready"),
              (status = 503, description = "Database unavailable")))]
async fn ready(State(state): State<AppState>) -> (StatusCode, &'static str) {
    match tether_db::ping(&state.db).await {
        Ok(()) => (StatusCode::OK, "ready"),
        Err(err) => {
            tracing::warn!(error = %err, "readiness check failed");
            (StatusCode::SERVICE_UNAVAILABLE, "database unavailable")
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use axum::body::{Body, to_bytes};
    use axum::http::Request;
    use sqlx::postgres::PgPoolOptions;
    use tether_db::PgPool;
    use tower::ServiceExt;

    async fn get(app: Router, uri: &str) -> (StatusCode, String) {
        let response = app
            .oneshot(Request::get(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let body = to_bytes(response.into_body(), 1024).await.unwrap();
        (status, String::from_utf8(body.to_vec()).unwrap())
    }

    fn local_net() -> tether_net::Outbound {
        tether_net::Outbound::new(
            tether_net::Allowlist::production().with_local("127.0.0.1:9"),
            "tether tests",
            Duration::from_secs(1),
        )
        .unwrap()
    }

    fn state(db: PgPool) -> AppState {
        let sso: std::sync::Arc<dyn tether_esi::sso::Sso> =
            std::sync::Arc::new(tether_esi::sso::EveSso::new(
                tether_esi::jwt::JwtVerifier::new(local_net(), "http://127.0.0.1:9/jwks").unwrap(),
            ));
        let key =
            tether_core::crypto::EncryptionKey::from_hex(&tether_core::Secret::new("0".repeat(64)))
                .unwrap();
        let vault = std::sync::Arc::new(tether_esi::vault::TokenVault::new(
            db.clone(),
            key.clone(),
            sso.clone(),
            "https://tether.test/auth/callback".into(),
        ));
        let discord = std::sync::Arc::new(
            tether_discord::Discord::new(
                tether_discord::Endpoints::local("127.0.0.1:9"),
                local_net(),
            )
            .unwrap(),
        );
        let esi = tether_esi::Esi::new("tether tests", Some("http://127.0.0.1:9")).unwrap();
        let plugins = crate::plugins::Plugins::new(
            tether_plugins::host::Host::new(std::sync::Arc::new(
                tether_plugins::Runtime::new().unwrap(),
            ))
            .unwrap(),
            crate::plugin_services::Deps {
                db: db.clone(),
                esi: esi.clone(),
                vault: vault.clone(),
                discord: discord.clone(),
                key: key.clone(),
                public_url: "https://tether.test".to_owned(),
                snapshots: None,
                github: None,
            },
        );
        AppState {
            vault,
            key: key.clone(),
            discord,
            db,
            esi,
            sso,
            site: std::sync::Arc::new(Site::new("https://tether.test")),
            setup_token: std::sync::Arc::new(tether_core::Secret::new("t".repeat(32))),
            limits: std::sync::Arc::default(),
            plugins,
            notices: crate::notifications::Notices::idle(),
        }
    }

    fn unreachable_db() -> PgPool {
        PgPoolOptions::new()
            .acquire_timeout(Duration::from_millis(200))
            .connect_lazy("postgres://nobody@127.0.0.1:1/none")
            .unwrap()
    }

    #[tokio::test]
    async fn health_returns_ok_without_a_database() {
        let app = router(state(unreachable_db()));
        assert_eq!(get(app, "/health").await, (StatusCode::OK, "ok".into()));
    }

    #[sqlx::test]
    async fn ready_when_database_answers(db: PgPool) {
        let app = router(state(db));
        assert_eq!(get(app, "/ready").await, (StatusCode::OK, "ready".into()));
    }

    #[tokio::test]
    async fn not_ready_without_database() {
        let app = router(state(unreachable_db()));
        let (status, _) = get(app, "/ready").await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    }
}
