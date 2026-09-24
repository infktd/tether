//! Security headers on every response.

use axum::extract::{Request, State};
use axum::http::{HeaderValue, header};
use axum::middleware::Next;
use axum::response::Response;

use crate::AppState;

/// Only our own origin, plus character portraits and logos from CCP's image
/// server (the one external request browsers make, per DESIGN.md). `data:`
/// images are allowed because Basecoat draws select chevrons as inline SVG
/// images; images can't run script. Forms post only to us, but browsers
/// apply form-action to the redirect after a post too, so the "Link
/// Discord" form (post, then 303 to Discord's authorize page) needs
/// discord.com. No inline scripts, no eval, no framing.
const CSP: &str = "default-src 'self'; img-src 'self' data: https://images.evetech.net; \
    script-src 'self'; style-src 'self'; font-src 'self'; connect-src 'self'; \
    object-src 'none'; base-uri 'self'; form-action 'self' https://discord.com; \
    frame-ancestors 'none'";

pub async fn security_headers(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    // The dev-only Scalar page loads from a CDN (dev-docs feature).
    let exempt = request.uri().path() == "/docs";
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    if !exempt {
        headers.insert(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(CSP),
        );
    }
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("same-origin"),
    );
    if state.site.public_url().starts_with("https://") {
        headers.insert(
            header::STRICT_TRANSPORT_SECURITY,
            HeaderValue::from_static("max-age=31536000"),
        );
    }
    response
}
