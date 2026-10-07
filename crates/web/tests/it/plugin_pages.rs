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
         [[navigation]]\nlabel = \"Secret\"\npath = \"admin/secret\"\n\n\
         [[views]]\nlabel = \"Overview\"\npath = \"\"\n",
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
        "tabbed-form",
        "list",
        "searched",
        "panel",
        "settings",
        "big-settings",
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

/// An app nobody may use yet says so on its admin page, until one of
/// its permissions is granted.
#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn an_app_nobody_may_use_says_so(db: PgPool) {
    let (h, owner, _) = setup(db).await;
    let admin = page(&h, &format!("/admin/plugins/{ID}"), &owner).await.body;
    assert!(admin.contains("Nobody may use it yet"), "{admin}");
    grant_view(&h.db).await;
    let admin = page(&h, &format!("/admin/plugins/{ID}"), &owner).await.body;
    assert!(!admin.contains("Nobody may use it yet"), "{admin}");
}

/// An app whose main page is open to everyone signed in (as Timezones,
/// Contacts and HR Applications) isn't "nobody may use it", and the
/// admin isn't told to hand its manage rights to states.
#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn an_app_open_to_everyone_signed_in_isnt_nobodys(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    let pilot = log_in_as(&h, "443630591:The Mittani", None).await;
    let key = Key::new(1);
    let manifest = format!(
        "[plugin]\nid = \"acme.open\"\nname = \"Open\"\nversion = \"1.0.0\"\nhost_api = \"1\"\n\n\
         [publisher]\nkey = \"{}\"\n\n[permissions]\nmanage = \"Configure it\"\n\n\
         [[views]]\nlabel = \"Overview\"\npath = \"\"\n\n\
         [[pages]]\npath = \"\"\nsigned_in = true\n",
        key.public()
    );
    let component = component();
    let bytes = testing::zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("plugin.wasm", &component),
    ]);
    install_package(&h, &owner, &bytes, &key.sign(&bytes)).await;
    assert_eq!(
        page(&h, "/plugins/acme.open", &pilot).await.status,
        StatusCode::OK
    );
    let admin = page(&h, "/admin/plugins/acme.open", &owner).await.body;
    assert!(!admin.contains("Nobody may use it yet"), "{admin}");
    assert!(!admin.contains("Grant them to states"), "{admin}");
    assert!(
        admin.contains("Every signed-in pilot may open it"),
        "{admin}"
    );
    tether_db::permissions::grant(
        &h.db,
        "plugin.acme.open.manage",
        Grantee::State(StateId(MEMBER_STATE)),
    )
    .await
    .unwrap();
    let admin = page(&h, "/admin/plugins/acme.open", &owner).await.body;
    assert!(
        !admin.contains("Every signed-in pilot may open it"),
        "{admin}"
    );
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
        "first tab",
        r#"href="/plugins/acme.pages/values?_tab=1""#,
    ] {
        assert!(res.body.contains(part), "{part}: {}", res.body);
    }
    assert!(!res.body.contains("second tab"));
    // No other pilot on it: no watermark (DESIGN.md, Watermark).
    assert!(!res.body.contains("Viewing as"), "{}", res.body);
    let second = page(&h, "/plugins/acme.pages/values?_tab=1", &owner).await;
    assert!(second.body.contains("second tab") && !second.body.contains("first tab"));

    // The query reaches the plugin, without the host's own parameters.
    let query = page(&h, "/plugins/acme.pages/query?moon=1&_tab=0", &owner).await;
    assert!(query.body.contains("moon"), "{}", query.body);
    // The host's own refresh keeps the tab; the app's page doesn't see it.
    let shown = query.body.replace(
        r#"data-href="/plugins/acme.pages/query?moon=1&#38;_tab=0""#,
        "",
    );
    assert!(!shown.contains("_tab"), "{}", query.body);
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
    let huge = format!("_form=note&body={}", "x".repeat(600 * 1024));
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
            "/admin/permissions/set",
            &format!("permission=plugin.acme.pages.view&grantee=state%3A{MEMBER_STATE}"),
            &owner,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let res = send(
        &h.app,
        form(
            "/admin/permissions/set",
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
async fn instruments_are_drawn_without_inline_styles(db: PgPool) {
    let (h, owner, _) = setup(db).await;
    let res = page(&h, "/plugins/acme.pages/instruments", &owner).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    let body = &res.body;
    // Escaped everywhere, and no inline styles: the CSP allows none, so
    // colours and positions are the stylesheet's classes.
    assert!(!body.contains("<script>alert"), "{body}");
    assert!(!body.contains("style="), "{body}");
    for part in [
        // Five squares: four trained, the fifth in training.
        r#"<span class="levels" role="img" aria-label="Level 4 of 5, training 5" title="Level 4 of 5, training 5"><span data-trained></span><span data-trained></span><span data-trained></span><span data-trained></span><span data-training></span></span>"#,
        // A small ring, its parts by grade class, labelled for readers.
        r#"aria-label="Xenotime &#60;script&#62;alert(1)&#60;/script&#62; 31%, Sylvite 69%""#,
        r#"class="grade-4""#,
        r#"class="grade-0""#,
        // The large one: its center words and a legend.
        r#"class="composition composition-large""#,
        "1.84B &#60;script&#62;",
        r#"<span class="mark grade-2"></span><span>Chromite</span><span class="num">34%</span>"#,
        // Shield gone and armor hit show red; the core pulses.
        r#"aria-label="Shield 0%, armor 62%, hull 100%, alarm""#,
        r#"class="arc-danger alarm""#,
        // The timeline: title, a lane, a window, day ticks, a linked bar
        // that needs attention, and a dashed proposal.
        "Next days &#60;script&#62;",
        "Fleets &#60;script&#62;",
        r#"<span class="timeline-window tl-l38 tl-w6"></span>"#,
        r#"<span class="timeline-tick num tl-l50">MON 28</span>"#,
        r#"<a href="/plugins/acme.pages/values" class="timeline-item tl-l65 tl-w6" data-row="0" data-tone="signal" data-bar"#,
        r#"data-tone="" data-planned"#,
    ] {
        assert!(body.contains(part), "{part}\n\n{body}");
    }
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
        // A badge with a success tone is a status line, its word escaped.
        r#"<span class="status-line" data-tone="ok">Badge &#60;script&#62;alert(1)&#60;/script&#62;</span>"#,
        // Another pilot shows: the watermark names who looked.
        "Viewing as Chribba",
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
        r#"<progress class="sr-only" max="1" value="0.4200">42%</progress>"#,
        // The segmented bar: 42% of 24 cells is 10 lit.
        r#"<span class="segbar" aria-hidden="true"><span data-lit></span>"#,
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
    // No Add data source: this app has no data sources.
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
            r#"<div id="plugin-content" class="plugin-content" hx-get="/plugins/acme.pages/live" hx-trigger="every 5s" hx-swap="outerHTML" data-live data-app="acme.pages" data-href="/plugins/acme.pages/live">"#
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
    assert!(!res.body.contains("hx-trigger=\"every"), "{}", res.body);

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
    assert_eq!(
        res.headers[header::VARY],
        "HX-Request, HX-Target, HX-Trigger"
    );
    assert_eq!(res.headers[header::CACHE_CONTROL], "no-store");
    let whole = page(&h, "/plugins/acme.pages/live", &owner).await;
    assert_eq!(
        whole.headers[header::VARY],
        "HX-Request, HX-Target, HX-Trigger"
    );
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
    assert!(!res.body.contains("hx-trigger=\"every"), "{}", res.body);
    assert_eq!(res.headers[header::CACHE_CONTROL], "no-store");
    // Nor does htmx's own history cache: back and forward ask again.
    assert!(res.body.contains(r#"hx-history="false""#), "{}", res.body);
    assert_eq!(
        res.headers[header::VARY],
        "HX-Request, HX-Target, HX-Trigger"
    );
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
         [permissions]\nview = \"See the pages\"\n\n[[views]]\nlabel = \"Overview\"\npath = \"\"\n\n[[pages]]\npath = \"groups\"\n\
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

/// A tab, as htmx asks for it from the page at `current`: its links
/// target the content.
fn tab(uri: &str, current: &str, token: &str) -> Request<Body> {
    let mut req = boosted(get(uri, &[(SESSION, token)]), current);
    req.headers_mut()
        .insert("hx-target", "plugin-content".parse().unwrap());
    req
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn tabs_swap_the_content_alone(db: PgPool) {
    let (h, owner, _) = setup(db).await;
    let uri = "/plugins/acme.pages/values?moon=1";
    // Tab links swap the content, keep the scroll and go in the history,
    // and carry the page's query (a search stays across tabs).
    let whole = page(&h, uri, &owner).await;
    assert!(
        whole.body.contains(
            r##"hx-target="#plugin-content" hx-swap="outerHTML show:none" hx-push-url="true""##
        ),
        "{}",
        whole.body
    );
    assert!(
        whole
            .body
            .contains(r#"href="/plugins/acme.pages/values?moon=1&#38;_tab=1""#),
        "{}",
        whole.body
    );

    // The tab: the content alone, not the page.
    let res = send(
        &h.app,
        tab("/plugins/acme.pages/values?moon=1&_tab=1", uri, &owner),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert!(
        res.body
            .trim_start()
            .starts_with(r#"<div id="plugin-content""#),
        "{}",
        res.body
    );
    assert!(res.body.contains("second tab"), "{}", res.body);
    assert!(!res.body.contains("<html") && !res.body.contains("app-sidebar"));
    assert_eq!(
        res.headers[header::VARY],
        "HX-Request, HX-Target, HX-Trigger"
    );
    assert_eq!(res.headers[header::CACHE_CONTROL], "no-store");

    // Back and forward: a history restore gets the whole page.
    let mut restore = tab("/plugins/acme.pages/values?moon=1&_tab=1", uri, &owner);
    restore
        .headers_mut()
        .insert("hx-history-restore-request", "true".parse().unwrap());
    let res = send(&h.app, restore).await;
    assert!(res.body.contains("app-sidebar"), "{}", res.body);
    assert!(res.body.contains("second tab"), "{}", res.body);

    // A tab that can't be shown: the whole page says so, not a page
    // inside the content.
    let res = send(
        &h.app,
        tab("/plugins/acme.pages/missing?_tab=1", uri, &owner),
    )
    .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
    assert_eq!(res.headers["hx-retarget"], "body");
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn actions_answer_in_place_with_a_toast(db: PgPool) {
    let (h, owner, _) = setup(db).await;
    let uri = "/plugins/acme.pages/blocks?x=1";
    // Row actions post to the page's own address, query and all.
    let shown = page(&h, uri, &owner).await;
    assert!(
        shown
            .body
            .contains(r#"action="/plugins/acme.pages/blocks?x=1""#),
        "{}",
        shown.body
    );

    // From the page: the content in place (what the app answered with,
    // under the same address), and a toast.
    let res = send(
        &h.app,
        boosted(post(uri, "_form=decide&verdict=approve&id=7", &owner), uri),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert_eq!(res.headers["hx-retarget"], "#plugin-content");
    assert_eq!(res.headers["hx-reswap"], "outerHTML show:none");
    assert_eq!(res.headers["hx-push-url"], "false");
    assert!(
        res.body
            .trim_start()
            .starts_with(r#"<div id="plugin-content""#),
        "{}",
        res.body
    );
    assert!(res.body.contains("Saved") && !res.body.contains("<html"));
    assert_eq!(
        toast(&res),
        Some(("Approve · done".to_owned(), "done".to_owned()))
    );

    // Refused (not a button the page offers): the page as it is, the
    // reason on it and in a toast. The host's check is unchanged.
    let res = send(
        &h.app,
        boosted(post(uri, "_form=decide&id=8&verdict=approve", &owner), uri),
    )
    .await;
    assert_eq!(res.status, StatusCode::CONFLICT);
    assert_eq!(res.headers["hx-retarget"], "#plugin-content");
    assert!(
        res.body.contains("isn&#39;t on this page") || res.body.contains("isn't on this page"),
        "{}",
        res.body
    );
    let (message, tone) = toast(&res).unwrap();
    assert!(message.contains("on this page any more"), "{message}");
    assert_eq!(tone, "problem");

    // From another page (the Dashboard, say): back there, in place.
    let res = send(
        &h.app,
        boosted(post(uri, "_form=close&id=8", &owner), "/dashboard"),
    )
    .await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.body);
    let to = hx_location(&res).unwrap();
    assert_eq!(to["path"], "/dashboard");
    assert_eq!(to["push"], "false");
    assert_eq!(
        toast(&res),
        Some(("Close · done".to_owned(), "done".to_owned()))
    );

    // Without JavaScript: the whole page, as ever.
    let res = send(&h.app, post(uri, "_form=close&id=8", &owner)).await;
    assert_eq!(res.status, StatusCode::OK);
    assert!(res.body.contains("app-sidebar"), "{}", res.body);
    assert!(res.headers.get("hx-trigger").is_none());
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn an_apps_redirect_back_keeps_the_tab(db: PgPool) {
    let (h, owner, _) = setup(db).await;
    let uri = "/plugins/acme.pages/tabbed-form?_tab=1";
    let shown = page(&h, uri, &owner).await;
    assert!(
        shown
            .body
            .contains(r#"action="/plugins/acme.pages/tabbed-form?_tab=1""#),
        "{}",
        shown.body
    );
    // Without JavaScript: redirected to the page, under its tab.
    let res = send(&h.app, post(uri, "_form=note&body=hi", &owner)).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER);
    assert_eq!(res.location(), uri);
    // With it: the page reloaded in place, no new history entry, a toast.
    let res = send(
        &h.app,
        boosted(post(uri, "_form=note&body=hi", &owner), uri),
    )
    .await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.body);
    let to = hx_location(&res).unwrap();
    assert_eq!(to["path"], uri);
    assert_eq!(to["swap"], "innerHTML show:none");
    assert_eq!(to["push"], "false");
    assert_eq!(toast(&res).unwrap().0, "Save · done");
}

/// The app's frame (DESIGN.md, App shell): from its manifest, Tether draws
/// its views bar, its one action (never on its own page) and its Manage
/// menu, each entry only for those who may open its page.
#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn the_manifest_draws_the_apps_frame(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    let pilot = log_in_as(&h, "443630591:The Mittani", None).await;
    let key = Key::new(2);
    let manifest = format!(
        "[plugin]\nid = \"acme.frame\"\nname = \"Frame\"\nversion = \"1.0.0\"\nhost_api = \"1\"\n\n\
         [publisher]\nkey = \"{}\"\n\n[permissions]\nview = \"See it\"\nmanage = \"Run it\"\n\n\
         [[pages]]\npath = \"\"\npermission = \"view\"\n\n\
         [[pages]]\npath = \"admin\"\npermission = \"manage\"\n\n\
         [[pages]]\npath = \"settings\"\npermission = \"manage\"\n\n\
         [[views]]\nlabel = \"Overview\"\npath = \"\"\n\n\
         [[views]]\nlabel = \"Values\"\npath = \"values\"\n\n\
         [action]\nlabel = \"New block\"\npath = \"blocks\"\n\n\
         [[manage]]\nlabel = \"Secret\"\npath = \"admin/secret\"\n\n\
         [[navigation]]\nlabel = \"Frame\"\npath = \"\"\n",
        key.public()
    );
    let component = component();
    let bytes = testing::zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("plugin.wasm", &component),
    ]);
    install_package(&h, &owner, &bytes, &key.sign(&bytes)).await;
    for state in [MEMBER_STATE, BLUE_STATE, GUEST_STATE] {
        tether_db::permissions::grant(
            &h.db,
            "plugin.acme.frame.view",
            Grantee::State(StateId(state)),
        )
        .await
        .unwrap();
    }

    // A member: the views and the action, no Manage, and the app's sidebar
    // link marked on every page of it.
    let values = page(&h, "/plugins/acme.frame/values", &pilot).await;
    assert_eq!(values.status, StatusCode::OK, "{}", values.body);
    let body = &values.body;
    assert!(
        body.contains(r#"<a href="/plugins/acme.frame" class="nav-item" aria-current="page">"#),
        "{body}"
    );
    assert!(
        body.contains(r#"<nav class="views-bar" aria-label="Views">"#),
        "{body}"
    );
    assert!(
        body.contains(r#"<a href="/plugins/acme.frame">Overview</a>"#),
        "{body}"
    );
    assert!(
        body.contains(r#"<a href="/plugins/acme.frame/values" aria-current="page">Values</a>"#),
        "{body}"
    );
    assert!(
        body.contains(r#"<a class="btn" data-variant="primary" href="/plugins/acme.frame/blocks">New block</a>"#),
        "{body}"
    );
    assert!(
        !body.contains("manage-link"),
        "no Manage for a member: {body}"
    );
    // The action isn't offered on its own page, and a page under no view
    // marks none (the main page covers only itself).
    let blocks = page(&h, "/plugins/acme.frame/blocks", &pilot).await;
    assert!(!blocks.body.contains(">New block</a>"), "{}", blocks.body);
    assert!(
        blocks
            .body
            .contains(r#"<a href="/plugins/acme.frame">Overview</a>"#),
        "{}",
        blocks.body
    );

    // Whoever runs it: Manage opens the first manage page, Settings.
    let values = page(&h, "/plugins/acme.frame/values", &owner).await;
    assert!(
        values.body.contains(
            r#"<a class="btn manage-link" data-variant="outline" href="/plugins/acme.frame/settings">"#
        ),
        "{}",
        values.body
    );
    // On a Manage page: the eyebrow says so, and the bar is the Manage pages.
    let secret = page(&h, "/plugins/acme.frame/admin/secret", &owner).await;
    let body = &secret.body;
    assert!(
        body.contains(r#"<a href="/plugins/acme.frame">Frame</a> · Manage"#),
        "{body}"
    );
    assert!(
        body.contains(r#"<nav class="views-bar" aria-label="Manage">"#),
        "{body}"
    );
    assert!(
        body.contains(
            r#"<a href="/plugins/acme.frame/admin/secret" aria-current="page">Secret</a>"#
        ),
        "{body}"
    );
    // Settings first, then the app's own pages; no Manage button here.
    let bar = body.split(r#"aria-label="Manage">"#).nth(1).unwrap();
    assert!(
        bar.find(">Settings</a>").unwrap() < bar.find(">Secret</a>").unwrap(),
        "{bar}"
    );
    assert!(!body.contains("manage-link"), "{body}");
}

/// The toolbar (DESIGN.md, Toolbar): a long list gets Tether's search box
/// over it, kept in the address, finding rows among those the page shows
/// (every page of them).
#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_long_list_gets_a_search_kept_in_the_address(db: PgPool) {
    let (h, owner, _) = setup(db).await;
    let list = page(&h, "/plugins/acme.pages/list", &owner).await;
    assert_eq!(list.status, StatusCode::OK, "{}", list.body);
    let toolbar = list
        .body
        .find(r#"<div class="toolbar""#)
        .unwrap_or_else(|| panic!("{}", list.body));
    // Over the list, after what comes before it (the stats).
    let stats = list.body.find(r#"class="readouts""#).unwrap();
    let table = list.body.find(r#"<table class="table""#).unwrap();
    assert!(stats < toolbar && toolbar < table, "{}", list.body);
    assert!(
        list.body.contains(
            r#"<form class="toolbar-search" method="get" action="/plugins/acme.pages/list" role="search" data-instant>"#
        ),
        "{}",
        list.body
    );
    // 30 rows, 25 to a page.
    assert!(!list.body.contains("Alpha 29<"), "{}", list.body);

    // Found among all the rows, in one page, with the words in the box.
    let found = page(&h, "/plugins/acme.pages/list?q=BETA", &owner)
        .await
        .body;
    assert!(
        found.contains("Beta 2<") && found.contains("Beta 30<") && !found.contains("Alpha "),
        "{found}"
    );
    assert!(found.contains(r#"name="q" value="BETA""#), "{found}");
    let none = page(&h, "/plugins/acme.pages/list?q=gamma", &owner)
        .await
        .body;
    assert!(
        none.contains("Nothing matches \u{201c}gamma\u{201d}."),
        "{none}"
    );
    // A short list gets none; nor does a page without one.
    let short = page(&h, "/plugins/acme.pages/panel", &owner).await.body;
    assert!(!short.contains("toolbar-search"), "{short}");
    let plain = page(&h, "/plugins/acme.pages/blocks", &owner).await.body;
    assert!(!plain.contains(r#"class="toolbar""#), "{plain}");
}

/// A page searching its own data reads the search and its filters from
/// its address; Tether hides none of its rows, and draws its filters as
/// chips (the one applied, with a × taking it off) and "+ Filter".
#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn an_apps_own_search_and_filters_are_its_own(db: PgPool) {
    let (h, owner, _) = setup(db).await;
    let res = page(&h, "/plugins/acme.pages/searched?q=zzz&kind=ore", &owner)
        .await
        .body;
    assert!(
        res.contains("asked q=&#34;zzz&#34; kind=&#34;ore&#34;"),
        "{res}"
    );
    for name in ["x", "y", "z", "w", "v", "u", "t", "s"] {
        assert!(res.contains(&format!("<td>{name}</td>")), "{name}: {res}");
    }
    assert!(
        res.contains(r#"placeholder="Search things""#) && !res.contains("data-instant"),
        "{res}"
    );
    // The filter applied: a chip whose × keeps the search.
    assert!(
        res.contains(
            r#"<a href="/plugins/acme.pages/searched?q=zzz" aria-label="Take off Kind: Ore">"#
        ),
        "{res}"
    );
    // "+ Filter": each value, the search kept, the applied one marked.
    assert!(
        res.contains(r#"<a href="/plugins/acme.pages/searched?q=zzz&#38;kind=ice">Ice</a>"#),
        "{res}"
    );
    assert!(
        res.contains(
            r#"<a href="/plugins/acme.pages/searched?q=zzz&#38;kind=ore" aria-current="true">Ore</a>"#
        ),
        "{res}"
    );
    // The search box keeps the filter.
    assert!(
        res.contains(r#"<input type="hidden" name="kind" value="ore">"#),
        "{res}"
    );
    // On a phone the chips and "+ Filter" fold behind Filters, which
    // counts the filters applied and opens them as a sheet.
    let button = res
        .find(r#"class="btn toolbar-filters""#)
        .unwrap_or_else(|| panic!("{res}"));
    let sheet = res.find(r#"class="toolbar-rest""#).unwrap();
    let chip = res.find(r#"aria-label="Take off Kind: Ore""#).unwrap();
    assert!(button < sheet && sheet < chip, "{res}");
    assert!(
        res[button..sheet].contains(r#"<span class="num text-highlight">1</span>"#),
        "{res}"
    );
    // A list with nothing to filter has no Filters.
    let list = page(&h, "/plugins/acme.pages/list", &owner).await.body;
    assert!(!list.contains("toolbar-filters"), "{list}");
}

/// A row's name opens its record panel beside the list (DESIGN.md, Record
/// panel): the link joins the page's address, the row is marked, Close
/// goes back to the list as it was, and the panel's action posts like any
/// the page offers.
#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_rows_name_opens_its_record_panel_beside_the_list(db: PgPool) {
    let (h, owner, _) = setup(db).await;
    let list = page(&h, "/plugins/acme.pages/panel?q=item", &owner)
        .await
        .body;
    assert!(
        list.contains(r#"href="/plugins/acme.pages/panel?q=item&#38;item=2""#),
        "{list}"
    );
    assert!(!list.contains("record-panel"), "{list}");
    let shown = page(&h, "/plugins/acme.pages/panel?q=item&item=2", &owner)
        .await
        .body;
    assert!(
        shown.contains(r#"<aside class="record-panel bk""#),
        "{shown}"
    );
    assert!(
        shown.contains(r#"class="record-panel-title">Item 2</h2>"#)
            && shown.contains("&#60;b&#62;context&#60;/b&#62;")
            && shown.contains(">1.2b</text>"),
        "{shown}"
    );
    assert_eq!(
        shown
            .matches(r#"<tr data-selected aria-current="true">"#)
            .count(),
        1,
        "{shown}"
    );
    assert!(
        shown.contains(r#"href="/plugins/acme.pages/panel?q=item" aria-label="Close""#),
        "{shown}"
    );
    assert!(
        shown.contains(r#"href="/plugins/acme.pages/values">Open item</a>"#),
        "{shown}"
    );
    // Its action posts as the page offers it, and no other row's.
    let res = send(
        &h.app,
        post(
            "/plugins/acme.pages/panel?item=2",
            "_form=pin&item=2",
            &owner,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert!(res.body.contains("pinned 2"), "{}", res.body);
    let res = send(
        &h.app,
        post(
            "/plugins/acme.pages/panel?item=2",
            "_form=pin&item=3",
            &owner,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::CONFLICT, "{}", res.body);
}

/// Tabs are the toolbar's view chips, keeping the search; the tables'
/// pages and a selected row are left behind.
#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn tabs_are_the_toolbars_view_chips(db: PgPool) {
    let (h, owner, _) = setup(db).await;
    let res = page(&h, "/plugins/acme.pages/values?q=moon", &owner)
        .await
        .body;
    let toolbar = res
        .find(r#"<div class="toolbar""#)
        .unwrap_or_else(|| panic!("{res}"));
    let chips = res
        .find(r#"<nav class="view-chips" aria-label="Tabs">"#)
        .unwrap();
    assert!(toolbar < chips, "{res}");
    assert!(
        res.contains(r#"href="/plugins/acme.pages/values?q=moon&#38;_tab=1""#),
        "{res}"
    );
}

/// A settings page (DESIGN.md, Save bar): its groups in one form, an
/// index of them, and the bar that saves them all; posted, every field's
/// value is checked as a form's, then the app gets them at once.
/// A value's fingerprint in a settings form's `_drawn`, as the host makes
/// it.
fn fingerprint(value: &str) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(value.as_bytes())
        .iter()
        .take(8)
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_settings_form_of_120_fields_saves(db: PgPool) {
    let (h, owner, _) = setup(db).await;
    let uri = "/plugins/acme.pages/big-settings";
    let shown = page(&h, uri, &owner).await;
    assert_eq!(shown.status, StatusCode::OK, "{}", shown.body);
    let body = form_body(&shown.body, "big", &[("n_3_29", "7")]);
    let res = send(&h.app, post(uri, &body, &owner)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert!(res.body.contains("120 values"), "{}", res.body);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_settings_page_saves_its_groups_at_once(db: PgPool) {
    let (h, owner, _) = setup(db).await;
    let uri = "/plugins/acme.pages/settings";
    let res = page(&h, uri, &owner).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    for part in [
        r#"<form class="settings-form" method="post" action="/plugins/acme.pages/settings" data-settings>"#,
        r#"<input type="hidden" name="_form" value="prefs">"#,
        r##"<a href="#prefs-1" hx-boost="false" data-index="prefs-1">Fuel alerts"##,
        r#"<section class="card" id="prefs-0" data-group="Discord""#,
        "Where &#60;b&#62;pings&#60;/b&#62; go.",
        r#"id="prefs-hours" name="hours" value="48""#,
        r#"<div class="save-bar" data-save-bar"#,
        r#"<button type="reset" class="btn" data-variant="outline">Discard</button>"#,
    ] {
        assert!(res.body.contains(part), "{part}: {}", res.body);
    }
    // What it shows, so a save takes only what was changed.
    let drawn = format!(
        r#"<input type="hidden" name="_drawn" value="{{&#34;channel&#34;:&#34;{}&#34;,&#34;hours&#34;:&#34;{}&#34;}}">"#,
        fingerprint("1"),
        fingerprint("48")
    );
    assert!(res.body.contains(&drawn), "{drawn}: {}", res.body);
    // Saved at once, each value checked as a form's.
    let res = send(
        &h.app,
        post(uri, "_form=prefs&channel=2&mention=on&hours=24", &owner),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert!(
        res.body.contains(
            "got [(&#34;channel&#34;, &#34;2&#34;), (&#34;mention&#34;, &#34;true&#34;), (&#34;hours&#34;, &#34;24&#34;)]"
        ),
        "{}",
        res.body
    );
    for body in [
        "_form=prefs&channel=9&hours=24",
        "_form=prefs&channel=1&hours=500",
        "_form=prefs&channel=1",
    ] {
        let res = send(&h.app, post(uri, body, &owner)).await;
        assert_eq!(
            res.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{body}: {}",
            res.body
        );
        assert!(!res.body.contains("Settings saved"), "{body}");
    }
    // A field left as the page showed it takes its value now (another
    // admin may have saved it meanwhile): this page showed 24 hours.
    let drawn = format!(
        r#"{{"channel":"{}","hours":"{}"}}"#,
        fingerprint("1"),
        fingerprint("24")
    );
    let res = send(
        &h.app,
        post(
            uri,
            &format!(
                "_form=prefs&_drawn={}&channel=2&hours=24",
                url_encode(&drawn)
            ),
            &owner,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert!(
        res.body.contains(
            "got [(&#34;channel&#34;, &#34;2&#34;), (&#34;mention&#34;, &#34;false&#34;), (&#34;hours&#34;, &#34;48&#34;)]"
        ),
        "{}",
        res.body
    );
    // From the page itself, a refused save leaves it as it is, its
    // changes and all: a toast says why.
    let res = send(
        &h.app,
        boosted(post(uri, "_form=prefs&channel=1&hours=500", &owner), uri),
    )
    .await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.body);
    let (message, tone) = toast(&res).unwrap();
    assert!(message.contains("Warn under (hours)"), "{message}");
    assert_eq!(tone, "problem");
}
