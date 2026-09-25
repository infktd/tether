//! The OpenAPI document for the JSON API, and (dev builds only) Scalar.

use axum::Json;
use utoipa::openapi::security::{ApiKey, ApiKeyValue, SecurityScheme};
use utoipa::{Modify, OpenApi};

use crate::auth::SESSION_COOKIE;
use crate::{api, setup};

#[derive(OpenApi)]
#[openapi(
    info(
        title = "Tether API",
        description = "JSON API for bots, scripts and testing. The browser UI uses HTML over htmx instead.",
    ),
    paths(
        crate::health,
        crate::ready,
        api::me,
        api::set_main,
        api::groups::list,
        api::groups::join,
        api::groups::leave,
        api::admin::create_group,
        api::admin::delete_group,
        api::admin::add_member,
        api::admin::remove_member,
        api::admin::list_requests,
        api::admin::approve_request,
        api::admin::deny_request,
        api::admin::list_permissions,
        api::admin::grant,
        api::admin::revoke,
        api::admin::audit_log,
        api::admin::deactivate_account,
        api::admin::reactivate_account,
        api::admin::list_states,
        api::admin::create_state,
        api::admin::rename_state,
        api::admin::delete_state,
        api::admin::move_state,
        api::admin::add_cover,
        api::admin::remove_cover,
        api::admin::add_scope,
        api::admin::remove_scope,
        api::admin::resolve_names,
        setup::status,
        setup::unlock,
        setup::set_sso,
        setup::probe,
        setup::callback_check,
    ),
    modifiers(&SessionCookie),
    tags(
        (name = "health", description = "Liveness and readiness"),
        (name = "account", description = "The signed-in account"),
        (name = "groups", description = "Joining and leaving groups"),
        (name = "admin", description = "Groups, permissions, states and the audit log"),
        (name = "setup", description = "First-run wizard"),
    )
)]
pub struct ApiDoc;

/// Signed-in endpoints use the browser session cookie, set by EVE SSO login.
/// Personal access tokens for bots arrive with F19.
struct SessionCookie;

impl Modify for SessionCookie {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        let components = openapi.components.get_or_insert_with(Default::default);
        components.add_security_scheme(
            "session",
            SecurityScheme::ApiKey(ApiKey::Cookie(ApiKeyValue::with_description(
                SESSION_COOKIE,
                "Session cookie from logging in with EVE SSO at /auth/login",
            ))),
        );
    }
}

/// `GET /api/openapi.json`
pub async fn spec() -> Json<utoipa::openapi::OpenApi> {
    Json(ApiDoc::openapi())
}

/// Scalar, pinned and integrity-checked. Loading it from a CDN is allowed
/// only because this page exists solely in dev builds (CLAUDE.md opsec).
#[cfg(feature = "dev-docs")]
pub const SCALAR_SCRIPT: &str = r#"<script src="https://cdn.jsdelivr.net/npm/@scalar/api-reference@1.71.0/dist/browser/standalone.js" integrity="sha256-CO91oLRQPBYwMW+P/OHelf1KkP14IBo9gRtRy5CsIIU=" crossorigin="anonymous"></script>"#;

/// `GET /docs` (`dev-docs` feature): Scalar over the spec.
#[cfg(feature = "dev-docs")]
pub async fn docs() -> axum::response::Html<String> {
    axum::response::Html(format!(
        r#"<!doctype html>
<html lang="en">
<head><meta charset="utf-8"><title>Tether API</title></head>
<body>
<script id="api-reference" data-url="/api/openapi.json"></script>
{SCALAR_SCRIPT}
</body>
</html>"#
    ))
}
