//! CSRF protection: state-changing requests must come from our own origin.
//!
//! Browsers send `Origin` on cross-site and same-site POSTs; when it is
//! absent we accept only `Sec-Fetch-Site: same-origin`. Session cookies are
//! also SameSite=Lax, so this is a second layer.

use axum::extract::{Request, State};
use axum::http::{Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::AppState;

pub async fn verify_origin(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    if matches!(
        *request.method(),
        Method::GET | Method::HEAD | Method::OPTIONS
    ) {
        return next.run(request).await;
    }
    let headers = request.headers();
    let allowed = match headers.get(header::ORIGIN) {
        Some(origin) => origin.to_str().is_ok_and(|o| o == state.site.origin()),
        None => headers
            .get("sec-fetch-site")
            .is_some_and(|v| v == "same-origin"),
    };
    if allowed {
        next.run(request).await
    } else {
        tracing::warn!(
            method = %request.method(),
            path = request.uri().path(),
            origin = ?headers.get(header::ORIGIN),
            "blocked cross-origin request"
        );
        (StatusCode::FORBIDDEN, "Cross-site request blocked.").into_response()
    }
}
