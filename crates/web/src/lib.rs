//! HTTP routes, auth middleware and the OpenAPI document.

// dev-login creates sessions without SSO, and dev-docs loads Scalar from a
// CDN. Neither may reach a release build; CI checks these guards fire.
#[cfg(all(feature = "dev-login", not(debug_assertions)))]
compile_error!("the dev-login feature must never be enabled in release builds");
#[cfg(all(feature = "dev-docs", not(debug_assertions)))]
compile_error!("the dev-docs feature must never be enabled in release builds");

pub mod admin;
mod api;
pub mod auth;
mod csrf;
#[cfg(feature = "dev-login")]
mod dev_login;
pub mod discord;
pub mod discord_sync;
mod error;
pub mod maintenance;
pub mod openapi;
pub mod pages;
pub mod pings;
pub mod plugins;
mod ratelimit;
pub mod setup;
mod state;
pub mod sync;
pub mod tiers;
pub mod updates;

use axum::extract::{DefaultBodyLimit, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, post};
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
        .route("/profile", get(pages::profile))
        .route("/profile/main", post(pages::make_main))
        .route("/setup", get(pages::setup::page))
        .route("/setup/unlock", post(pages::setup::unlock))
        .route("/setup/sso", post(pages::setup::sso))
        .route("/setup/check", post(pages::setup::check))
        .route("/setup/alliance", post(pages::setup::choose_alliance))
        .route("/setup/alliance/search", post(pages::setup::search))
        .route("/static/{*path}", get(pages::assets::serve))
        .route("/admin", get(pages::admin::index))
        .route(
            "/admin/groups",
            get(pages::admin::groups).post(pages::admin::create_group),
        )
        .route("/admin/groups/{id}", get(pages::admin::group))
        .route(
            "/admin/groups/{id}/delete",
            post(pages::admin::delete_group),
        )
        .route("/admin/groups/{id}/members", post(pages::admin::add_member))
        .route(
            "/admin/groups/{id}/members/{account_id}/remove",
            post(pages::admin::remove_member),
        )
        .route(
            "/admin/groups/{id}/requests/{account_id}/approve",
            post(pages::admin::approve),
        )
        .route(
            "/admin/groups/{id}/requests/{account_id}/deny",
            post(pages::admin::deny),
        )
        .route("/admin/permissions", get(pages::admin::permissions))
        .route("/admin/permissions/grant", post(pages::admin::grant))
        .route(
            "/admin/permissions/{grant_id}/revoke",
            post(pages::admin::revoke),
        )
        .route(
            "/admin/tiers",
            get(pages::admin::tiers).post(pages::admin::set_rule),
        )
        .route(
            "/admin/tiers/{entity_id}/remove",
            post(pages::admin::remove_rule),
        )
        .route("/admin/tiers/search", post(pages::admin::search))
        .route(
            "/admin/discord",
            get(pages::discord::admin).post(pages::discord::save_settings),
        )
        .route(
            "/admin/discord/nickname",
            post(pages::discord::save_nickname),
        )
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
        .route("/admin/system", get(pages::system::system))
        .route("/admin/system/updates", post(pages::system::set_updates))
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
        .route("/admin/plugins/{id}", get(pages::plugins::plugin))
        .route("/admin/plugins/{id}/enable", post(pages::plugins::enable))
        .route("/admin/plugins/{id}/disable", post(pages::plugins::disable))
        .route(
            "/admin/plugins/{id}/uninstall",
            post(pages::plugins::uninstall),
        )
        .route("/profile/discord/link", post(pages::discord::link))
        .route("/profile/discord/unlink", post(pages::discord::unlink))
        .route("/discord/callback", get(pages::discord::callback))
        .route("/health", get(health))
        .route("/ready", get(ready))
        .route("/auth/login", get(auth::login))
        .route("/auth/callback", get(auth::callback))
        .route("/auth/logout", post(auth::logout))
        .route("/api/me", get(api::me))
        .route("/api/me/main", post(api::set_main))
        .route("/api/groups", get(api::groups::list))
        .route("/api/groups/{id}/join", post(api::groups::join))
        .route("/api/groups/{id}/leave", post(api::groups::leave))
        .route("/api/admin/groups", post(api::admin::create_group))
        .route("/api/admin/groups/{id}", delete(api::admin::delete_group))
        .route(
            "/api/admin/groups/{id}/members",
            post(api::admin::add_member),
        )
        .route(
            "/api/admin/groups/{id}/members/{account_id}",
            delete(api::admin::remove_member),
        )
        .route(
            "/api/admin/groups/{id}/requests",
            get(api::admin::list_requests),
        )
        .route(
            "/api/admin/groups/{id}/requests/{account_id}/approve",
            post(api::admin::approve_request),
        )
        .route(
            "/api/admin/groups/{id}/requests/{account_id}/deny",
            post(api::admin::deny_request),
        )
        .route("/api/admin/permissions", get(api::admin::list_permissions))
        .route("/api/admin/permissions/grants", post(api::admin::grant))
        .route(
            "/api/admin/permissions/grants/{id}",
            delete(api::admin::revoke),
        )
        .route("/api/admin/audit", get(api::admin::audit_log))
        .route(
            "/api/admin/tiers",
            get(api::admin::list_tier_rules).post(api::admin::set_tier_rule),
        )
        .route(
            "/api/admin/tiers/{entity_id}",
            delete(api::admin::remove_tier_rule),
        )
        .route("/api/admin/tiers/resolve", post(api::admin::resolve_names))
        .route("/api/setup", get(setup::status))
        .route("/api/setup/unlock", post(setup::unlock))
        .route("/api/setup/sso", post(setup::set_sso))
        .route("/api/setup/probe", get(setup::probe))
        .route("/api/setup/callback-check", post(setup::callback_check))
        .fallback(pages::not_found)
        .layer(middleware::from_fn_with_state(
            state.clone(),
            csrf::verify_origin,
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
        AppState {
            vault: std::sync::Arc::new(tether_esi::vault::TokenVault::new(
                db.clone(),
                key.clone(),
                sso.clone(),
                "https://tether.test/auth/callback".into(),
            )),
            key: key.clone(),
            discord: std::sync::Arc::new(
                tether_discord::Discord::new(
                    tether_discord::Endpoints::local("127.0.0.1:9"),
                    local_net(),
                )
                .unwrap(),
            ),
            db,
            esi: tether_esi::Esi::new("tether tests", Some("http://127.0.0.1:9")).unwrap(),
            sso,
            site: std::sync::Arc::new(Site::new("https://tether.test")),
            setup_token: std::sync::Arc::new(tether_core::Secret::new("t".repeat(32))),
            limits: std::sync::Arc::default(),
            plugins: crate::plugins::Plugins::new(
                tether_plugins::host::Host::new(std::sync::Arc::new(
                    tether_plugins::Runtime::new().unwrap(),
                ))
                .unwrap(),
            ),
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
