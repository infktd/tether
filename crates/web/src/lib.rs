//! HTTP routes, auth middleware and the OpenAPI document.

use axum::extract::State;
use axum::http::StatusCode;
use axum::{Router, routing::get};
use tether_db::PgPool;

#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/ready", get(ready))
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

    fn unreachable_db() -> PgPool {
        PgPoolOptions::new()
            .acquire_timeout(Duration::from_millis(200))
            .connect_lazy("postgres://nobody@127.0.0.1:1/none")
            .unwrap()
    }

    #[tokio::test]
    async fn health_returns_ok_without_a_database() {
        let app = router(AppState {
            db: unreachable_db(),
        });
        assert_eq!(get(app, "/health").await, (StatusCode::OK, "ok".into()));
    }

    #[sqlx::test]
    async fn ready_when_database_answers(db: PgPool) {
        let app = router(AppState { db });
        assert_eq!(get(app, "/ready").await, (StatusCode::OK, "ready".into()));
    }

    #[tokio::test]
    async fn not_ready_without_database() {
        let app = router(AppState {
            db: unreachable_db(),
        });
        let (status, _) = get(app, "/ready").await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    }
}
