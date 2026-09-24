//! HTTP routes, auth middleware and the OpenAPI document.

mod api;
pub mod auth;
mod csrf;
mod error;
mod state;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Router, middleware};

pub use state::{AppState, Site};

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/ready", get(ready))
        .route("/auth/login", get(auth::login))
        .route("/auth/callback", get(auth::callback))
        .route("/auth/logout", post(auth::logout))
        .route("/api/me", get(api::me))
        .route("/api/me/main", post(api::set_main))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            csrf::verify_origin,
        ))
        .with_state(state)
}

/// Liveness: the process is up and serving HTTP.
async fn health() -> &'static str {
    "ok"
}

/// Readiness: the database answers.
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

    fn state(db: PgPool) -> AppState {
        AppState {
            db,
            sso: std::sync::Arc::new(tether_esi::sso::EveSso),
            site: std::sync::Arc::new(Site::new("https://tether.test")),
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
