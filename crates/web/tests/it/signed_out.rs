//! Signed out, every form endpoint sends you to log in before it looks
//! at the form: an empty or missing body is never a 415 or a 422.

use crate::common::*;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use sqlx::PgPool;

/// Every signed-in form route, with sample path ids.
const ROUTES: &[&str] = &[
    "/admin/autogroups/1/delete",
    "/tokens/1/delete",
    "/notifications/1/delete",
    "/notifications/read-all",
    "/notifications/delete-read",
    "/corpstats/1/update",
    "/groups/1/join",
    "/groups/1/leave",
    "/groups/1/retract",
    "/group-management/1/requests/1/accept",
    "/group-management/1/requests/1/reject",
    "/group-management/1/members/1/remove",
    "/admin/groups/1/leaders/1/remove",
    "/admin/groups/1/leader-groups/2/remove",
    "/pings/preview",
    "/admin/pings/settings",
    "/admin/pings/options",
    "/admin/pings/options/1/delete",
    "/admin/pings/restrictions",
    "/admin/pings/restrictions/1/remove",
    "/admin/system/theme",
    "/admin/menu/sections",
    "/admin/menu/folders",
    "/admin/menu/links",
    "/admin/menu/change",
    "/admin/menu/move",
    "/admin/menu/delete",
    "/admin/menu/reset",
    "/blacklist",
    "/blacklist/1/remove",
    "/blacklist/notes",
    "/blacklist/notes/1/delete",
    "/admin/groups/1/smart",
    "/admin/groups/1/smart/filters",
    "/admin/groups/1/smart/filters/1/delete",
    "/admin/users/1/deactivate",
    "/admin/users/1/reactivate",
    "/admin/discord/mappings/1/remove",
    "/services/discord/link",
    "/services/discord/unlink",
    "/admin/discord/channels/1/remove",
    "/admin/system/updates/check",
    "/admin/jobs/1/retry",
    "/admin/plugin-uploads/1/approve",
    "/admin/plugin-uploads/1/discard",
    "/admin/plugins/example.hello/enable",
    "/admin/plugins/example.hello/disable",
    "/admin/plugins/example.hello/update",
    "/admin/plugins/example.hello/source",
    "/admin/plugins/example.hello/rollback",
    "/admin/plugins/example.hello/uninstall",
    "/admin/plugin-github",
    "/register/start",
    "/profile/corp-stats/offer",
    "/profile/corp-stats/1/withdraw",
    "/compliance/sources/1/approve",
    "/compliance/sources/1/remove",
    "/profile/plugins/example.hello/offer",
    "/profile/plugins/example.hello/offer/1/withdraw",
    "/admin/plugins/example.hello/sources/1/approve",
    "/admin/plugins/example.hello/sources/1/remove",
    "/admin/plugins/example.hello/channels/1/remove",
];

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn signed_out_posts_go_to_login_first(db: PgPool) {
    let h = harness(db, true).await;
    let mut wrong = Vec::new();
    for uri in ROUTES {
        // No body and no content type, as a bare form post or a probe.
        let bare = Request::post(*uri)
            .header(header::ORIGIN, SITE)
            .body(Body::empty())
            .unwrap();
        let res = send(&h.app, bare).await;
        if res.status != StatusCode::SEE_OTHER || res.location() != "/login" {
            wrong.push(format!("{uri}: {}", res.status));
        }
    }
    assert!(wrong.is_empty(), "not sent to log in: {wrong:#?}");
}
