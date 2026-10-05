//! The Bulletin Board app end to end (aa-bulletin-board): installed from
//! its real component and migration; managers write, edit, limit to groups
//! and remove bulletins; pilots with basic_access see those for everyone
//! and those limited to one of their groups.

use std::sync::OnceLock;

use crate::common::*;
use axum::http::StatusCode;
use sqlx::PgPool;
use tether_plugins::testing::{self, Key};

const ID: &str = "tether.bulletin-board";

fn component() -> Vec<u8> {
    static COMPONENT: OnceLock<Vec<u8>> = OnceLock::new();
    COMPONENT
        .get_or_init(|| build_guest("bulletin-board"))
        .clone()
}

fn plugin_file(name: &str) -> String {
    std::fs::read_to_string(format!(
        "{}/../../plugins/bulletin-board/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

async fn install(h: &Harness, owner: &str) {
    let key = Key::new(9);
    let manifest = plugin_file("plugin.toml").replace("PUBLISHER_KEY", &key.public());
    let migration = plugin_file("migrations/0001_bulletin_board.sql");
    let component = component();
    let bytes = testing::zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("plugin.wasm", &component),
        ("migrations/0001_bulletin_board.sql", migration.as_bytes()),
    ]);
    let at = install_package(h, owner, &bytes, &key.sign(&bytes)).await;
    assert_eq!(at, format!("/admin/plugins/{ID}"));
}

async fn account(h: &Harness, character: i64) -> i64 {
    sqlx::query_scalar("SELECT account_id FROM core.characters WHERE id = $1")
        .bind(character)
        .fetch_one(&h.db)
        .await
        .unwrap()
}

async fn grant(h: &Harness, account: i64, permission: &str) {
    sqlx::query("INSERT INTO core.permission_grants (permission, account_id) VALUES ($1, $2)")
        .bind(format!("plugin.{ID}.{permission}"))
        .bind(account)
        .execute(&h.db)
        .await
        .unwrap();
}

async fn post(h: &Harness, token: &str, at: &str, body: &str) -> Res {
    let url = if at.is_empty() {
        format!("/plugins/{ID}")
    } else {
        format!("/plugins/{ID}/{at}")
    };
    send(&h.app, form(&url, body, token)).await
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn bulletin_board_end_to_end(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    install(&h, &owner).await;
    let a = log_in_as(&h, "443630591:Pilot A", None).await;
    let b = log_in_as(&h, "406944591:Pilot B", None).await;
    let (a_account, b_account) = (account(&h, 443630591).await, account(&h, 406944591).await);
    for account in [a_account, b_account] {
        grant(&h, account, "basic_access").await;
    }
    let scouts: i64 =
        sqlx::query_scalar("INSERT INTO core.groups (name) VALUES ('Scouts') RETURNING id")
            .fetch_one(&h.db)
            .await
            .unwrap();
    sqlx::query("INSERT INTO core.group_members (group_id, account_id) VALUES ($1, $2)")
        .bind(scouts)
        .bind(b_account)
        .execute(&h.db)
        .await
        .unwrap();

    // A manager (the owner) writes one; everyone with access sees it.
    let res = post(
        &h,
        &owner,
        "new",
        "_form=bulletin&title=Home+defense&content=Form+up+at+19%3A00.%0A%0ABring+a+fit.",
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let id: i32 = sqlx::query_scalar(r#"SELECT id FROM "plugin_tether.bulletin-board".bulletins"#)
        .fetch_one(&h.db)
        .await
        .unwrap();
    assert_eq!(res.location(), format!("/plugins/{ID}/bulletin/{id}"));
    let list = page(&h, &format!("/plugins/{ID}"), &a).await;
    assert_eq!(list.status, StatusCode::OK, "{}", list.body);
    assert!(list.body.contains("Home defense"), "{}", list.body);
    assert!(!list.body.contains("Create bulletin"), "{}", list.body);
    let one = page(&h, &format!("/plugins/{ID}/bulletin/{id}"), &a).await;
    assert!(one.body.contains("Form up at 19:00."), "{}", one.body);
    assert!(one.body.contains("Bring a fit."), "{}", one.body);
    // Not for pilots' changes.
    let res = post(&h, &a, "", &format!("_form=remove&bulletin={id}")).await;
    assert_ne!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert_eq!(
        page(&h, &format!("/plugins/{ID}/edit/{id}"), &a)
            .await
            .status,
        StatusCode::NOT_FOUND
    );

    // Limited to Scouts: B (a scout) sees it, A doesn't.
    let res = post(
        &h,
        &owner,
        &format!("edit/{id}"),
        &format!("_form=add_group&group={scouts}"),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert!(
        !page(&h, &format!("/plugins/{ID}"), &a)
            .await
            .body
            .contains("Home defense")
    );
    assert_eq!(
        page(&h, &format!("/plugins/{ID}/bulletin/{id}"), &a)
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    assert!(
        page(&h, &format!("/plugins/{ID}"), &b)
            .await
            .body
            .contains("Home defense")
    );

    // Edited: its title changes and it's marked updated.
    let res = post(
        &h,
        &owner,
        &format!("edit/{id}"),
        "_form=bulletin&title=Home+defense+moved&content=Form+up+at+20%3A00.",
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let one = page(&h, &format!("/plugins/{ID}/bulletin/{id}"), &b).await;
    assert!(one.body.contains("Home defense moved"), "{}", one.body);
    assert!(one.body.contains("Updated"), "{}", one.body);

    // A pilot can't edit it.
    let res = post(
        &h,
        &b,
        &format!("edit/{id}"),
        "_form=bulletin&title=Mine&content=Mine.",
    )
    .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND, "{}", res.body);
    // However old: one behind 500 newer ones still opens.
    sqlx::query(
        r#"INSERT INTO "plugin_tether.bulletin-board".bulletins
               (title, content, author_id, author_name, created_at)
           SELECT 'Filler ' || n, 'Text.', 196379789, 'Chribba', now() + make_interval(mins => n)
           FROM generate_series(1, 500) n"#,
    )
    .execute(&h.db)
    .await
    .unwrap();
    let old = page(&h, &format!("/plugins/{ID}/bulletin/{id}"), &b).await;
    assert_eq!(old.status, StatusCode::OK, "{}", old.body);

    // Removed from its edit page (the list shows the newest 500).
    let res = post(&h, &owner, &format!("edit/{id}"), "_form=remove").await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert!(
        !page(&h, &format!("/plugins/{ID}"), &owner)
            .await
            .body
            .contains("Home defense")
    );
}

/// Live pages: a new bulletin tells the open streams of everyone who may
/// open the board, and nobody else's; the board is marked to refresh.
#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_new_bulletin_reaches_open_boards(db: PgPool) {
    use std::time::Duration;
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    install(&h, &owner).await;
    let a = log_in_as(&h, "443630591:Pilot A", None).await;
    let b = log_in_as(&h, "406944591:Pilot B", None).await;
    grant(&h, account(&h, 443630591).await, "basic_access").await;

    let board = page(&h, &format!("/plugins/{ID}"), &a).await;
    assert!(
        board
            .body
            .contains(&format!(r#"data-app="{ID}" data-href="/plugins/{ID}""#)),
        "{}",
        board.body
    );
    let mut a_stream = open_stream(&h, &a).await;
    let mut b_stream = open_stream(&h, &b).await;

    let res = post(
        &h,
        &owner,
        "new",
        "_form=bulletin&title=Ops&content=Tonight.",
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let heard = next_sse(&mut a_stream, "app", Duration::from_secs(10))
        .await
        .expect("no app event after a new bulletin");
    assert!(heard.contains(&format!("data: {ID}\n")), "{heard}");
    // B may not open the board: not told.
    assert_eq!(
        next_sse(&mut b_stream, "app", Duration::from_secs(3)).await,
        None
    );

    // Only ids of apps the account may open pass, whatever is announced.
    tether_db::plugins::announce_change(&h.db, "tether.not-installed")
        .await
        .unwrap();
    assert_eq!(
        next_sse(&mut a_stream, "app", Duration::from_secs(3)).await,
        None
    );

    // Viewing the board writes no rows: nobody's told.
    assert_eq!(
        page(&h, &format!("/plugins/{ID}"), &a).await.status,
        StatusCode::OK
    );
    assert_eq!(
        next_sse(&mut a_stream, "app", Duration::from_secs(3)).await,
        None
    );
}
