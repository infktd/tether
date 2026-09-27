//! Plugin pages: who may open them (404 for anyone who may not), how the
//! host draws them, plugin navigation, forms checked before the plugin
//! sees them, and plugin permissions granted like core ones.

use std::sync::OnceLock;

use crate::common::*;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use sqlx::PgPool;
use tether_core::states::StateId;
use tether_db::permissions::Grantee;
use tether_plugins::testing::{self, Key};

const ID: &str = "acme.pages";

fn component() -> Vec<u8> {
    static COMPONENT: OnceLock<Vec<u8>> = OnceLock::new();
    COMPONENT
        .get_or_init(|| build_guest("tether-plugins-test-guest-pages"))
        .clone()
}

/// Every page but `admin/...` needs `view`; `admin/...` has no rule, so
/// it's for admins only.
async fn install(h: &Harness, owner: &str) {
    let key = Key::new(1);
    let mut manifest = format!(
        "[plugin]\nid = \"{ID}\"\nname = \"Pages\"\nversion = \"1.0.0\"\nhost_api = \"1\"\n\n\
         [publisher]\nkey = \"{}\"\n\n[permissions]\nview = \"See the pages\"\n\n\
         [[navigation]]\nlabel = \"Values\"\npath = \"values\"\n\n\
         [[navigation]]\nlabel = \"Secret\"\npath = \"admin/secret\"\n",
        key.public()
    );
    for path in [
        "values",
        "form",
        "failed",
        "crash",
        "query",
        "missing",
        "blocks",
        "live",
        "live-form",
        "groups",
    ] {
        manifest.push_str(&format!(
            "\n[[pages]]\npath = \"{path}\"\npermission = \"view\"\n"
        ));
    }
    // Mail is audited: every view is in the audit log.
    manifest.push_str("\n[[pages]]\npath = \"mail\"\npermission = \"view\"\naudit = true\n");
    let component = component();
    let bytes = testing::zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("plugin.wasm", &component),
    ]);
    let at = install_package(h, owner, &bytes, &key.sign(&bytes)).await;
    assert_eq!(at, format!("/admin/plugins/{ID}"));
}

async fn grant_view(db: &PgPool) {
    for state in [MEMBER_STATE, BLUE_STATE, GUEST_STATE] {
        tether_db::permissions::grant(db, "plugin.acme.pages.view", Grantee::State(StateId(state)))
            .await
            .unwrap();
    }
}

async fn setup(db: PgPool) -> (Harness, String, String) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    let pilot = log_in_as(&h, "443630591:The Mittani", None).await;
    install(&h, &owner).await;
    (h, owner, pilot)
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn only_those_allowed_learn_a_page_exists(db: PgPool) {
    let (h, owner, pilot) = setup(db).await;
    let uri = "/plugins/acme.pages/values";
    assert_eq!(send(&h.app, get(uri, &[])).await.location(), "/login");

    // Without the permission, the same 404 as for nothing at all.
    let denied = page(&h, uri, &pilot).await;
    let nothing = page(&h, "/plugins/no.such.plugin", &pilot).await;
    assert_eq!(denied.status, StatusCode::NOT_FOUND);
    assert_eq!(nothing.status, StatusCode::NOT_FOUND);
    assert_eq!(denied.body, nothing.body);
    assert!(
        !page(&h, "/dashboard", &pilot).await.body.contains(uri),
        "no sidebar link"
    );

    grant_view(&h.db).await;
    assert_eq!(page(&h, uri, &pilot).await.status, StatusCode::OK);
    assert!(
        page(&h, "/dashboard", &pilot).await.body.contains(uri),
        "sidebar link"
    );

    // A page no [[pages]] rule covers is for admins only.
    let secret = "/plugins/acme.pages/admin/secret";
    assert_eq!(page(&h, secret, &pilot).await.status, StatusCode::NOT_FOUND);
    assert!(!page(&h, "/dashboard", &pilot).await.body.contains(secret));
    assert_eq!(page(&h, secret, &owner).await.status, StatusCode::OK);
    assert!(page(&h, "/dashboard", &owner).await.body.contains(secret));

    // Posting is checked the same way.
    let res = send(
        &h.app,
        form("/plugins/acme.pages/admin/secret", "_form=x", &pilot),
    )
    .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);

    // Paths that aren't link paths never reach the plugin.
    for bad in [
        "/plugins/acme.pages/bad%20path",
        "/plugins/acme.pages/a//b",
        "/plugins/Not.An.Id/values",
    ] {
        assert_eq!(
            page(&h, bad, &owner).await.status,
            StatusCode::NOT_FOUND,
            "{bad}"
        );
    }
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn the_host_draws_what_the_plugin_describes(db: PgPool) {
    let (h, owner, _) = setup(db).await;
    let res = page(&h, "/plugins/acme.pages/values", &owner).await;
    assert_eq!(res.status, StatusCode::OK);
    for part in [
        "1.24b",
        "1,240,000,000 ISK",
        "2026-09-24 18:00",
        r#"href="/plugins/acme.pages/moons/old""#,
        "Viewing as Chribba",
        "first tab",
        r#"href="/plugins/acme.pages/values?_tab=1""#,
    ] {
        assert!(res.body.contains(part), "{part}: {}", res.body);
    }
    assert!(!res.body.contains("second tab"));
    let second = page(&h, "/plugins/acme.pages/values?_tab=1", &owner).await;
    assert!(second.body.contains("second tab") && !second.body.contains("first tab"));

    // The query reaches the plugin, without the host's own parameters.
    let query = page(&h, "/plugins/acme.pages/query?moon=1&_tab=0", &owner).await;
    assert!(query.body.contains("moon"), "{}", query.body);
    assert!(!query.body.contains("_tab"), "{}", query.body);
    let long = format!("/plugins/acme.pages/query?q={}", "x".repeat(3000));
    assert_eq!(
        page(&h, &long, &owner).await.status,
        StatusCode::BAD_REQUEST
    );

    // What went wrong goes to the plugin's log, not to the user.
    assert_eq!(
        page(&h, "/plugins/acme.pages/missing", &owner).await.status,
        StatusCode::NOT_FOUND
    );
    for path in ["failed", "crash"] {
        let res = page(&h, &format!("/plugins/acme.pages/{path}"), &owner).await;
        assert_eq!(
            res.status,
            StatusCode::INTERNAL_SERVER_ERROR,
            "{path}: {}",
            res.body
        );
        assert!(res.body.contains("be shown. The app"), "{path}");
        assert!(!res.body.contains("on fire"), "{path}");
    }
    let logged: Vec<String> = sqlx::query_scalar(
        "SELECT message FROM core.plugin_logs WHERE plugin_id = $1 AND level = 'error'",
    )
    .bind(ID)
    .fetch_all(&h.db)
    .await
    .unwrap();
    assert!(logged.iter().any(|l| l.contains("on fire")), "{logged:?}");
}

fn post(uri: &str, body: &str, token: &str) -> Request<Body> {
    form(uri, body, token)
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn forms_are_checked_before_the_plugin_sees_them(db: PgPool) {
    let (h, owner, pilot) = setup(db).await;
    let uri = "/plugins/acme.pages/form";
    let shown = page(&h, uri, &owner).await;
    assert!(
        shown.body.contains(r#"name="_form" value="note""#),
        "{}",
        shown.body
    );
    assert!(shown.body.contains(r#"maxlength="20""#));

    let ok = send(
        &h.app,
        post(uri, "_form=note&body=hi&count=3&kind=ore", &owner),
    )
    .await;
    assert_eq!(ok.status, StatusCode::OK, "{}", ok.body);
    assert!(ok.body.contains("got"), "{}", ok.body);
    // One value per field, in order; the checkbox is filled in.
    assert!(ok.body.contains("go"), "{}", ok.body);

    for (body, status) in [
        (
            "_form=note&body=hi&extra=1",
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        ("_form=note&body=", StatusCode::UNPROCESSABLE_ENTITY),
        (
            "_form=note&body=hi&count=11",
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            "_form=note&body=hi&count=2.5",
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            "_form=note&body=hi&kind=gas",
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            "_form=note&body=hi&body=again",
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            "_form=note&body=this%20is%20far%20more%20than%20twenty%20characters",
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        ("body=hi", StatusCode::BAD_REQUEST),
        ("_form=other&body=hi", StatusCode::CONFLICT),
    ] {
        let res = send(&h.app, post(uri, body, &owner)).await;
        assert_eq!(res.status, status, "{body}");
        assert!(!res.body.contains("got ["), "the plugin never saw {body}");
    }

    // Numbers arrive written one way, whatever was typed.
    let res = send(&h.app, post(uri, "_form=note&body=hi&count=%2B3.0", &owner)).await;
    assert!(
        res.body.contains("&#34;count&#34;, &#34;3&#34;"),
        "{}",
        res.body
    );
    // Too big a post is refused before anything reads it.
    let huge = format!("_form=note&body={}", "x".repeat(70 * 1024));
    let res = send(&h.app, post(uri, &huge, &owner)).await;
    assert_eq!(res.status, StatusCode::PAYLOAD_TOO_LARGE);

    // A plugin can send the user on to another of its pages.
    let res = send(&h.app, post(uri, "_form=note&body=hi&go=on", &owner)).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER);
    assert_eq!(res.location(), "/plugins/acme.pages/values");

    // Other sites can't post (the Origin check), and posting is rate-limited.
    let no_origin = Request::post(uri)
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header(header::COOKIE, format!("{SESSION}={owner}"))
        .body(Body::from("_form=note&body=hi"))
        .unwrap();
    assert_eq!(send(&h.app, no_origin).await.status, StatusCode::FORBIDDEN);
    grant_view(&h.db).await;
    let mut limited = false;
    for _ in 0..31 {
        let res = send(&h.app, post(uri, "_form=note&body=hi", &pilot)).await;
        if res.status == StatusCode::TOO_MANY_REQUESTS {
            limited = true;
            break;
        }
        assert_eq!(res.status, StatusCode::OK);
    }
    assert!(limited, "30 a minute per account and plugin");

    let logged: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM core.plugin_logs WHERE plugin_id = $1 AND message = 'submitted note'",
    )
    .bind(ID)
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert!(logged >= 2);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn plugin_permissions_are_granted_like_core_ones(db: PgPool) {
    let (h, owner, _) = setup(db).await;
    let listed = page(&h, "/admin/permissions", &owner).await.body;
    assert!(listed.contains("plugin.acme.pages.view") && listed.contains("See the pages"));

    let res = send(
        &h.app,
        form(
            "/admin/permissions/grant",
            &format!("permission=plugin.acme.pages.view&grantee=state%3A{MEMBER_STATE}"),
            &owner,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let res = send(
        &h.app,
        form(
            "/admin/permissions/grant",
            &format!("permission=plugin.acme.pages.nope&grantee=state%3A{MEMBER_STATE}"),
            &owner,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);

    // The owner has every permission, plugins' included.
    let me = me(&h, &owner).await;
    assert!(
        me["permissions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p == "plugin.acme.pages.view"),
        "{me}"
    );

    // Uninstalling takes the permission and its grants with it.
    let res = send(
        &h.app,
        form(
            "/admin/plugins/acme.pages/uninstall",
            "confirmation=acme.pages",
            &owner,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER);
    let left: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM core.permission_grants WHERE permission LIKE 'plugin.%'",
    )
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert_eq!(left, 0);
    assert!(
        !page(&h, "/admin/permissions", &owner)
            .await
            .body
            .contains("plugin.acme.pages")
    );
}

/// A live page reloading its content, as htmx asks for it.
fn reload(uri: &str, token: Option<&str>) -> Request<Body> {
    let mut req = Request::get(uri)
        .header("hx-request", "true")
        .header("hx-trigger", "plugin-content");
    if let Some(token) = token {
        req = req.header(header::COOKIE, format!("{SESSION}={token}"));
    }
    req.body(Body::empty()).unwrap()
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn the_newer_blocks_are_drawn_and_escaped(db: PgPool) {
    let (h, owner, _) = setup(db).await;
    let res = page(&h, "/plugins/acme.pages/blocks", &owner).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    let body = &res.body;
    // Every string the plugin gave is escaped.
    assert!(!body.contains("<script>alert"), "{body}");
    assert!(!body.contains("<b>bold"), "{body}");
    for part in [
        // Page links beside the title, the page shown marked; the primary
        // one a button.
        r#"<a href="/plugins/acme.pages/blocks" aria-current="page">Blocks</a>"#,
        r#"<a href="/plugins/acme.pages/values">Values &#60;script&#62;"#,
        r#"<a class="btn" data-variant="primary" href="/plugins/acme.pages/form">Create &#60;script&#62;"#,
        // The profile: a 64px portrait built from the id, name, subtitle,
        // badges, corporation, alliance (no id: initials) and facts.
        r#"<img src="https://images.evetech.net/characters/90000001/portrait?size=128" alt="" loading="lazy" class="entity-img" data-size="lg">"#,
        r#"<h2 class="profile-name">Pilot &#60;script&#62;"#,
        "Main &#60;script&#62;",
        r#"data-variant="secondary">Badge &#60;script&#62;"#,
        r#"src="https://images.evetech.net/corporations/98000001/logo?size=64""#,
        "Corp &#34;quoted&#34; &#38; co",
        r#"<span class="entity-initials" data-size="sm" aria-hidden="true">A&#60;</span>"#,
        "Fact &#60;script&#62;",
        r#"<span class="num">48,210,332</span>"#,
        // Types' 32px icons; factions' logos as corporations'.
        r#"src="https://images.evetech.net/types/587/icon?size=32""#,
        r#"src="https://images.evetech.net/corporations/500001/logo?size=64""#,
        // Countdowns: the instant in UTC as the host writes it, the EVE
        // time on hover, "done" once passed.
        r#"<time class="num" datetime="2098-12-31T22:00:00Z" title="2098-12-31 22:00:00 EVE" data-countdown>"#,
        r#"data-countdown>done</time>"#,
        // Fixture ids get initials.
        r#"aria-hidden="true">FP</span><span class="entity-name">Fixture Pilot</span>"#,
        // Progress: live between two instants, or fixed.
        r#"data-from="2000-01-01T00:00:00Z" data-to="2099-01-01T00:00:00Z" aria-label="Skill &#60;script&#62;"#,
        r#"<progress class="meter" max="1" value="0.4200">42%</progress>"#,
        // Text to copy, kept as it was and escaped, with its button.
        "[Rifter, &#60;script&#62;alert(1)&#60;/script&#62;]\n  Damage Control II\n</pre>",
        r#"data-copy data-copied-label="Copied">Copy &#60;script&#62;"#,
        r#"data-copy data-copied-label="Copied">Copy</button>"#,
        // Row actions: forms posting to the page, hidden values escaped; the
        // destructive one asks first, in a popover stating the consequence.
        r#"<form method="post" action="/plugins/acme.pages/blocks" class="inline-flex"><input type="hidden" name="_form" value="decide"><input type="hidden" name="id" value="7"><input type="hidden" name="verdict" value="approve"><button type="submit" class="btn" data-size="sm" data-variant="primary">Approve</button></form>"#,
        r#"popovertarget="page-confirm-0">Reject &#60;script&#62;"#,
        r#"<div id="page-confirm-0" popover class="confirm-popover" role="dialog""#,
        "Pilot 7 is told no &#60;script&#62;",
        r#"name="verdict" value="reject &#34;&#60;script&#62;alert(1)&#60;/script&#62;&#34;""#,
        r#"data-variant="destructive">Reject &#60;script&#62;"#,
        // A card grid: each card's portrait, logos and name, the name
        // opening its page; a card without a page is just its name.
        r#"<section class="card-grid">"#,
        r#"src="https://images.evetech.net/characters/90000002/portrait?size=128""#,
        r#"<span title="Ally &#60;script&#62;alert(1)&#60;/script&#62;"><img src="https://images.evetech.net/alliances/99000001/logo?size=64""#,
        r#"<a class="hover:underline underline-offset-4" href="/plugins/acme.pages/values">Card &#60;script&#62;"#,
        r#"<h3 class="grid-card-name">Unlinked Pilot</h3>"#,
        // A link to share: the site's address and the app's page, never
        // one the app wrote, with its Copy button.
        r#"<input class="input num text-xs min-w-0 flex-1" value="https://tether.test/plugins/acme.pages/values" readonly aria-label="Link to share"><button type="button" class="btn" data-variant="outline" data-size="sm" data-copy data-copied-label="Copied">Copy</button>"#,
        // The script that keeps them live, bundled.
        r#"<script src="/static/live.js" defer></script>"#,
    ] {
        assert!(body.contains(part), "{part}\n\n{body}");
    }
    // Not a live page.
    assert!(!body.contains("hx-trigger=\"every"), "{body}");
    // No Add owner: this app has no data sources.
    assert!(!body.contains("owners/add"), "{body}");
    // No Register Character card: this app reads no members' characters.
    assert!(!body.contains("Register Character"), "{body}");

    let js = send(&h.app, get("/static/live.js", &[])).await;
    assert_eq!(js.status, StatusCode::OK);
    assert!(js.body.contains("data-countdown"));
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn actions_post_only_what_the_page_offers(db: PgPool) {
    let (h, owner, pilot) = setup(db).await;
    let uri = "/plugins/acme.pages/blocks";
    // Exactly a button the page drew: the plugin gets its values.
    let res = send(
        &h.app,
        post(uri, "_form=decide&verdict=approve&id=7", &owner),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert!(res.body.contains("Saved"), "{}", res.body);
    assert!(
        res.body
            .contains("got [(&#34;id&#34;, &#34;7&#34;), (&#34;verdict&#34;, &#34;approve&#34;)]"),
        "{}",
        res.body
    );
    let res = send(&h.app, post(uri, "_form=close&id=8", &owner)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);

    // Anything else is refused before the plugin sees it: another id, a
    // value missing, one added, a name twice, a form nobody drew.
    for body in [
        "_form=decide&id=8&verdict=approve",
        "_form=decide&id=7",
        "_form=close&id=8&extra=1",
        "_form=close&id=8&id=8",
        "_form=nothing&id=8",
    ] {
        let res = send(&h.app, post(uri, body, &owner)).await;
        assert_eq!(res.status, StatusCode::CONFLICT, "{body}: {}", res.body);
        assert!(!res.body.contains("Saved"), "{body}");
    }
    // Posting needs the page's permission, as forms do.
    let res = send(&h.app, post(uri, "_form=close&id=8", &pilot)).await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
    let submitted: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM core.plugin_logs WHERE plugin_id = $1 AND message LIKE 'submitted %'",
    )
    .bind(ID)
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert_eq!(submitted, 2);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn live_pages_reload_their_content(db: PgPool) {
    let (h, owner, pilot) = setup(db).await;
    // Asked for every second: brought up to 5.
    let res = page(&h, "/plugins/acme.pages/live", &owner).await;
    assert!(
        res.body.contains(
            r#"<div id="plugin-content" class="plugin-content" hx-get="/plugins/acme.pages/live" hx-trigger="every 5s" hx-swap="outerHTML" data-live>"#
        ),
        "{}",
        res.body
    );
    // The query comes along.
    let res = page(&h, "/plugins/acme.pages/live?x=1&_tab=0", &owner).await;
    assert!(
        res.body
            .contains(r#"hx-get="/plugins/acme.pages/live?x=1&#38;_tab=0""#),
        "{}",
        res.body
    );
    // A page with a form never reloads under someone typing.
    let res = page(&h, "/plugins/acme.pages/live-form", &owner).await;
    assert_eq!(res.status, StatusCode::OK);
    assert!(!res.body.contains("hx-trigger"), "{}", res.body);

    // A reload gets the content alone.
    let res = send(&h.app, reload("/plugins/acme.pages/live", Some(&owner))).await;
    assert_eq!(res.status, StatusCode::OK);
    assert!(
        res.body
            .trim_start()
            .starts_with(r#"<div id="plugin-content""#),
        "{}",
        res.body
    );
    assert!(!res.body.contains("<html") && !res.body.contains("app-sidebar"));
    assert!(res.body.contains("syncing"));
    // Caches tell the two apart, and don't keep the content alone.
    assert_eq!(res.headers[header::VARY], "HX-Request, HX-Trigger");
    assert_eq!(res.headers[header::CACHE_CONTROL], "no-store");
    let whole = page(&h, "/plugins/acme.pages/live", &owner).await;
    assert_eq!(whole.headers[header::VARY], "HX-Request, HX-Trigger");
    assert!(whole.headers.get(header::CACHE_CONTROL).is_none());
    // One that fails leaves what's shown (and tries again later)...
    let res = send(&h.app, reload("/plugins/acme.pages/failed", Some(&owner))).await;
    assert_eq!(res.status, StatusCode::NO_CONTENT);
    assert!(res.headers.get("hx-refresh").is_none());
    // ...but a page that's gone for this viewer loads again whole, which
    // then says so.
    for token in [Some(pilot.as_str()), None] {
        let res = send(&h.app, reload("/plugins/acme.pages/live", token)).await;
        assert_eq!(res.status, StatusCode::NO_CONTENT);
        assert_eq!(res.headers["hx-refresh"], "true");
        assert!(res.body.is_empty());
    }
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn audited_pages_record_every_view(db: PgPool) {
    let (h, owner, pilot) = setup(db).await;
    grant_view(&h.db).await;
    let res = page(&h, "/plugins/acme.pages/mail/1?folder=inbox", &owner).await;
    assert_eq!(res.status, StatusCode::OK);
    assert!(res.body.contains("mail [(&#34;folder&#34;"), "{}", res.body);
    // It asks to reload, but audited pages never do (each would be an
    // entry), and browsers don't keep them.
    assert!(!res.body.contains("hx-trigger"), "{}", res.body);
    assert_eq!(res.headers[header::CACHE_CONTROL], "no-store");
    assert_eq!(res.headers[header::VARY], "HX-Request, HX-Trigger");
    assert_eq!(
        send(&h.app, reload("/plugins/acme.pages/mail/1", Some(&pilot)))
            .await
            .status,
        StatusCode::OK
    );
    // Pages under other rules aren't.
    assert_eq!(
        page(&h, "/plugins/acme.pages/values", &owner).await.status,
        StatusCode::OK
    );
    // Nor are views refused before the plugin is called.
    let res = page(&h, "/plugins/acme.pages/mail/1", "not-a-session").await;
    assert_eq!(res.location(), "/login");

    let rows: Vec<(Option<String>, Option<String>, serde_json::Value)> = sqlx::query_as(
        "SELECT actor_name, target, details FROM core.audit_log \
         WHERE action = 'plugin.page_view' ORDER BY id",
    )
    .fetch_all(&h.db)
    .await
    .unwrap();
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert_eq!(rows[0].0.as_deref(), Some("Chribba"));
    assert_eq!(rows[0].1.as_deref(), Some("plugin:acme.pages"));
    assert_eq!(
        rows[0].2,
        serde_json::json!({"path": "mail/1", "query": [["folder", "inbox"]], "via": "page"})
    );
    assert_eq!(rows[1].0.as_deref(), Some("The Mittani"));
    assert_eq!(rows[1].2["via"], "reload");

    // A view that can't be recorded isn't shown: not as a page, a reload
    // or a post.
    sqlx::query(
        "CREATE FUNCTION public.refuse_page_views() RETURNS trigger LANGUAGE plpgsql AS \
         $$ BEGIN IF NEW.action = 'plugin.page_view' THEN RAISE EXCEPTION 'no'; END IF; \
         RETURN NEW; END $$",
    )
    .execute(&h.db)
    .await
    .unwrap();
    sqlx::query(
        "CREATE TRIGGER refuse BEFORE INSERT ON core.audit_log \
         FOR EACH ROW EXECUTE FUNCTION public.refuse_page_views()",
    )
    .execute(&h.db)
    .await
    .unwrap();
    let res = page(&h, "/plugins/acme.pages/mail/1?folder=inbox", &owner).await;
    assert_eq!(res.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(!res.body.contains("mail ["), "{}", res.body);
    let res = send(&h.app, reload("/plugins/acme.pages/mail/1", Some(&owner))).await;
    assert_eq!(res.status, StatusCode::NO_CONTENT);
    let res = send(
        &h.app,
        post("/plugins/acme.pages/mail/1", "_form=x", &owner),
    )
    .await;
    assert_eq!(res.status, StatusCode::INTERNAL_SERVER_ERROR);
    let submitted: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM core.plugin_logs WHERE plugin_id = $1 AND message LIKE 'submitted %'",
    )
    .bind(ID)
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert_eq!(submitted, 0);
}

/// Only apps approved for `groups` learn any. `identity.groups` is the
/// viewer's own; `identity.all-groups` offers every group but Hidden and
/// Internal ones (unless they're the viewer's own), all but Internal ones
/// to `group_management`, and every group to `admin.groups`.
#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn apps_see_the_viewers_groups_and_only_listed_ones_to_offer(db: PgPool) {
    use tether_core::groups::Flags;
    let (h, owner, pilot) = setup(db).await;
    grant_view(&h.db).await;
    let pilot_id =
        tether_db::accounts::AccountId(me(&h, &pilot).await["account_id"].as_i64().unwrap());
    let group = |name: &'static str, flags: Flags| {
        let db = h.db.clone();
        async move {
            tether_db::groups::create(&db, name, "", flags)
                .await
                .unwrap()
        }
    };
    let hidden = Flags {
        hidden: true,
        ..Default::default()
    };
    let internal = Flags {
        internal: true,
        ..Default::default()
    };
    let alpha = group("Alpha", Flags::default()).await;
    let beta = group("Beta", Flags::default()).await;
    let crew = group("Hidden Crew", hidden).await;
    let inner = group("Inner", internal).await;
    let own_hidden = group("Own Hidden", hidden).await;
    let own_internal = group("Own Internal", internal).await;
    for g in [beta, own_hidden, own_internal] {
        tether_db::groups::add_member(&h.db, g, pilot_id)
            .await
            .unwrap();
    }
    let list = |groups: &[(tether_db::groups::GroupId, &str)]| {
        groups
            .iter()
            .map(|(id, name)| format!("{}={name}", id.0))
            .collect::<Vec<_>>()
            .join(",")
    };
    // Without the `groups` capability, an app learns nothing.
    let res = page(&h, "/plugins/acme.pages/groups", &pilot).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert!(res.body.contains("mine[]"), "{}", res.body);
    assert!(res.body.contains("offered[]"), "{}", res.body);

    // The same component, approved for it (the review says so).
    let key = Key::new(2);
    let manifest = format!(
        "[plugin]\nid = \"acme.grouped\"\nname = \"Grouped\"\nversion = \"1.0.0\"\n\
         host_api = \"1\"\n\n[publisher]\nkey = \"{}\"\n\n[capabilities]\ngroups = true\n\n\
         [permissions]\nview = \"See the pages\"\n\n[[pages]]\npath = \"groups\"\n\
         permission = \"view\"\n",
        key.public()
    );
    let component = component();
    let bytes = testing::zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("plugin.wasm", &component),
    ]);
    let review = upload(&h, &owner, &bytes, &key.sign(&bytes)).await;
    assert_eq!(review.status, StatusCode::SEE_OTHER, "{}", review.body);
    let shown = page(&h, review.location(), &owner).await;
    assert!(
        shown.body.contains("Hidden and Internal ones included"),
        "{}",
        shown.body
    );
    let res = send(
        &h.app,
        form(&format!("{}/approve", review.location()), "", &owner),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    for state in [MEMBER_STATE, BLUE_STATE, GUEST_STATE] {
        tether_db::permissions::grant(
            &h.db,
            "plugin.acme.grouped.view",
            Grantee::State(StateId(state)),
        )
        .await
        .unwrap();
    }
    let uri = "/plugins/acme.grouped/groups";

    let res = page(&h, uri, &pilot).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    let mine = list(&[
        (beta, "Beta"),
        (own_hidden, "Own Hidden"),
        (own_internal, "Own Internal"),
    ]);
    assert!(res.body.contains(&format!("mine[{mine}]")), "{}", res.body);
    let offered = list(&[
        (alpha, "Alpha"),
        (beta, "Beta"),
        (own_hidden, "Own Hidden"),
        (own_internal, "Own Internal"),
    ]);
    assert!(
        res.body.contains(&format!("offered[{offered}]")),
        "{}",
        res.body
    );

    // Group management (through a group) covers every group but Internal
    // ones, as in core: Hidden ones are offered too.
    let managing = tether_db::permissions::grant(&h.db, "group_management", Grantee::Group(beta))
        .await
        .unwrap()
        .unwrap();
    let res = page(&h, uri, &pilot).await;
    let groups = |sql: &'static str| {
        let db = h.db.clone();
        async move {
            let rows: Vec<(i64, String)> = sqlx::query_as(sql).fetch_all(&db).await.unwrap();
            rows.iter()
                .map(|(id, name)| format!("{id}={name}"))
                .collect::<Vec<_>>()
                .join(",")
        }
    };
    let managed = groups(
        "SELECT id, name FROM core.groups WHERE NOT internal OR name = 'Own Internal' \
         ORDER BY name, id",
    )
    .await;
    assert!(managed.contains(&format!("{}=Hidden Crew", crew.0)));
    assert!(!managed.contains(&format!("{}=Inner", inner.0)));
    assert!(
        res.body.contains(&format!("offered[{managed}]")),
        "{}",
        res.body
    );
    // Every group there is, the ones Tether makes itself included.
    let every = groups("SELECT id, name FROM core.groups ORDER BY name, id").await;
    assert!(every.contains(&format!("{}=Inner", inner.0)));

    // admin.groups offers every group.
    tether_db::permissions::revoke(&h.db, managing)
        .await
        .unwrap();
    let res = page(&h, uri, &pilot).await;
    assert!(
        res.body.contains(&format!("offered[{offered}]")),
        "{}",
        res.body
    );
    tether_db::permissions::grant(&h.db, "admin.groups", Grantee::Group(beta))
        .await
        .unwrap();
    let res = page(&h, uri, &pilot).await;
    assert!(
        res.body.contains(&format!("offered[{every}]")),
        "{}",
        res.body
    );

    // The owner holds everything, and is in no group.
    let res = page(&h, uri, &owner).await;
    assert!(res.body.contains("mine[]"), "{}", res.body);
    assert!(
        res.body.contains(&format!("offered[{every}]")),
        "{}",
        res.body
    );
}
