//! The Blacklist and Pilot Log, as allianceauth-blacklist: notes with
//! restricted and ultra restricted tiers, comments, own-corporation
//! permissions; a blacklisted main's account is in the Blacklist state,
//! and that state is all it changes.

use axum::http::StatusCode;
use sqlx::PgPool;
use tether_core::states::{Builtin, EntityKind};

use crate::common::*;

const CHRIBBA: &str = "196379789:Chribba"; // corp 1164409536, alliance 159826257
const GIGX: &str = "1887431749:gigX"; // corp 98133756, alliance 1695357456
const MITTANI: &str = "443630591:The Mittani"; // corp 1000167 (NPC)

async fn account_of(h: &Harness, token: &str) -> i64 {
    me(h, token).await["account_id"].as_i64().unwrap()
}

async fn evaluate(h: &Harness, account: i64) {
    tether_web::states::evaluate_account(&h.db, tether_db::accounts::AccountId(account))
        .await
        .unwrap();
}

/// ESI names for pilots and the NPC corporation (the fixtures don't
/// name them).
async fn name_pilots(h: &Harness) {
    for (id, name) in [
        (196379789_i64, "Chribba"),
        (1887431749, "gigX"),
        (443630591, "The Mittani"),
        (1000167, "State War Academy"),
    ] {
        sqlx::query(
            "INSERT INTO core.entity_names (id, name, category) VALUES ($1, $2, $3) \
             ON CONFLICT (id) DO UPDATE SET name = EXCLUDED.name, fetched_at = now()",
        )
        .bind(id)
        .bind(name)
        .bind(if id == 1000167 {
            "corporation"
        } else {
            "character"
        })
        .execute(&h.db)
        .await
        .unwrap();
    }
}

/// A group granting these permissions, with these accounts in it.
async fn group_with(h: &Harness, owner: &str, permissions: &[&str], members: &[i64]) -> i64 {
    let name = format!("g{}", rand_suffix());
    let group = send(
        &h.app,
        post_json(
            "/api/admin/groups",
            owner,
            &format!(r#"{{"name":"{name}"}}"#),
        ),
    )
    .await;
    let group: serde_json::Value = serde_json::from_str(&group.body).unwrap();
    let group = group["id"].as_i64().unwrap();
    for permission in permissions {
        let res = send(
            &h.app,
            post_json(
                "/api/admin/permissions/grants",
                owner,
                &format!(r#"{{"permission":"{permission}","group_id":{group}}}"#),
            ),
        )
        .await;
        assert_eq!(
            res.status,
            StatusCode::CREATED,
            "{permission}: {}",
            res.body
        );
    }
    for account in members {
        let res = send(
            &h.app,
            post_json(
                &format!("/api/admin/groups/{group}/members"),
                owner,
                &format!(r#"{{"account_id":{account}}}"#),
            ),
        )
        .await;
        assert!(res.status.is_success(), "{}", res.body);
    }
    group
}

fn rand_suffix() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

async fn blacklist_state(h: &Harness) -> i64 {
    sqlx::query_scalar("SELECT id FROM core.states WHERE builtin = 'blacklist'")
        .fetch_one(&h.db)
        .await
        .unwrap()
}

async fn permissions_of(h: &Harness, token: &str) -> Vec<String> {
    me(h, token).await["permissions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p.as_str().unwrap().to_owned())
        .collect()
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_blacklisted_main_gets_the_blacklist_state_and_nothing_else(db: PgPool) {
    cover(&db, Builtin::Member, EntityKind::Alliance, 1695357456).await;
    let h = harness(db, true).await;
    name_pilots(&h).await;
    let owner = log_in_owner(&h, CHRIBBA).await;
    let pilot = log_in_as(&h, GIGX, None).await;
    let pilot_account = account_of(&h, &pilot).await;
    assert_eq!(state_of(&h, &pilot).await, "Member");
    // In a group open to every state, and one for Member only.
    let anyone = group_with(&h, &owner, &["request_groups"], &[pilot_account]).await;
    let members_only = group_with(&h, &owner, &[], &[pilot_account]).await;
    let member_state: i64 =
        sqlx::query_scalar("SELECT id FROM core.states WHERE builtin = 'member'")
            .fetch_one(&h.db)
            .await
            .unwrap();
    sqlx::query("INSERT INTO core.group_states (group_id, state_id) VALUES ($1, $2)")
        .bind(members_only)
        .bind(member_state)
        .execute(&h.db)
        .await
        .unwrap();
    // The Blacklist state is a state like any other: it can be granted
    // things (AA).
    let res = send(
        &h.app,
        post_json(
            "/api/admin/permissions/grants",
            &owner,
            &format!(
                r#"{{"permission":"discord.access_discord","state_id":{}}}"#,
                blacklist_state(&h).await
            ),
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.body);

    // Never a superuser.
    let res = send(
        &h.app,
        form("/blacklist", "who=1164409536&reason=test", &owner),
    )
    .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
    assert!(res.body.contains("superuser"), "{}", res.body);
    // NPC corporations can be, as in AA.
    let res = send(
        &h.app,
        form("/blacklist", "who=1000167&reason=Scammers+corp", &owner),
    )
    .await;
    assert_eq!(res.location(), "/blacklist", "{}", res.body);

    let res = send(
        &h.app,
        form("/blacklist", "who=98133756&reason=Awoxed+a+Rorqual", &owner),
    )
    .await;
    assert_eq!(res.location(), "/blacklist", "{}", res.body);
    let queued: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM core.jobs WHERE kind = 'states.evaluate_all' AND state = 'queued'",
    )
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert!(queued >= 1, "everyone is re-evaluated at once");
    evaluate(&h, pilot_account).await;
    assert_eq!(state_of(&h, &pilot).await, "Blacklist");
    // The Blacklist state's grants, and groups that don't exclude it.
    let pilot_me = me(&h, &pilot).await;
    let held = permissions_of(&h, &pilot).await;
    assert!(
        held.contains(&"discord.access_discord".to_owned()),
        "{pilot_me}"
    );
    assert!(held.contains(&"request_groups".to_owned()), "{pilot_me}");
    let anyone: String = sqlx::query_scalar("SELECT name FROM core.groups WHERE id = $1")
        .bind(anyone)
        .fetch_one(&h.db)
        .await
        .unwrap();
    assert_eq!(
        pilot_me["groups"],
        serde_json::json!([anyone]),
        "{pilot_me}"
    );
    let listed = page(&h, "/blacklist", &owner).await.body;
    assert!(listed.contains("CircleOfTwo Holding") && listed.contains("Awoxed a Rorqual"));
    // The Blacklist state is offered where states are named.
    let permissions = page(&h, "/admin/permissions", &owner).await.body;
    assert!(permissions.contains(">Blacklist<"), "{permissions}");

    // Off the list, back to Member; the note stays in the Pilot Log.
    let res = send(&h.app, form("/blacklist/98133756/remove", "", &owner)).await;
    assert_eq!(res.location(), "/blacklist", "{}", res.body);
    evaluate(&h, pilot_account).await;
    assert_eq!(state_of(&h, &pilot).await, "Member");
    let log = page(&h, "/blacklist", &owner).await.body;
    assert!(log.contains("Awoxed a Rorqual"), "{log}");
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn blacklisting_goes_by_the_main(db: PgPool) {
    cover(&db, Builtin::Member, EntityKind::Character, 443630591).await;
    let h = harness(db, true).await;
    name_pilots(&h).await;
    let owner = log_in_owner(&h, CHRIBBA).await;
    let pilot = log_in_as(&h, MITTANI, None).await;
    let pilot_account = account_of(&h, &pilot).await;
    // An alt in the corporation about to be blacklisted.
    sqlx::query(
        "INSERT INTO core.characters (id, account_id, name, corporation_id) \
         VALUES (90000002, $1, 'Alt', 98133756)",
    )
    .bind(pilot_account)
    .execute(&h.db)
    .await
    .unwrap();
    let group = send(
        &h.app,
        post_json(
            "/api/admin/groups",
            &owner,
            r#"{"name":"Haulers","internal":false,"hidden":false}"#,
        ),
    )
    .await;
    let group: serde_json::Value = serde_json::from_str(&group.body).unwrap();
    let group = group["id"].as_i64().unwrap();
    let res = send(
        &h.app,
        axum::http::Request::put(format!("/api/admin/groups/{group}/leaders/{pilot_account}"))
            .header(axum::http::header::ORIGIN, SITE)
            .header(axum::http::header::COOKIE, format!("{SESSION}={owner}"))
            .body(axum::body::Body::empty())
            .unwrap(),
    )
    .await;
    assert!(res.status.is_success(), "{}", res.body);

    let res = send(
        &h.app,
        form("/blacklist", "who=98133756&reason=Spies", &owner),
    )
    .await;
    assert_eq!(res.location(), "/blacklist", "{}", res.body);
    // Only an alt is covered: AA's state comes from the main.
    assert_eq!(state_of(&h, &pilot).await, "Member");

    // A note on the pilot, on every character of their account (AA's
    // "all linked characters"), blacklisting them: the main is covered now.
    let res = send(
        &h.app,
        form(
            "/blacklist/notes",
            "who=443630591&reason=Spy&blacklisted=on&linked=on",
            &owner,
        ),
    )
    .await;
    assert_eq!(res.location(), "/blacklist", "{}", res.body);
    let noted: Vec<(i64, String)> = sqlx::query_as(
        "SELECT entity_id, note FROM core.pilot_notes WHERE entity_kind = 'character' ORDER BY entity_id",
    )
    .fetch_all(&h.db)
    .await
    .unwrap();
    assert_eq!(
        noted,
        vec![
            (90000002, "Linked: The Mittani - Spy".to_owned()),
            (443630591, "Spy".to_owned())
        ]
    );
    // At once, without waiting for any sync.
    assert_eq!(state_of(&h, &pilot).await, "Blacklist");
    // A clean alt made main would be a way out: refused.
    let res = send(
        &h.app,
        form("/profile/main", "character_id=90000002", &pilot),
    )
    .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.body);
    // Leading a group only needs what it always did (AA).
    assert_eq!(
        page(&h, &format!("/group-management/{group}"), &pilot)
            .await
            .status,
        StatusCode::OK
    );
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn the_pilot_log_has_aa_tiers_and_comments(db: PgPool) {
    let h = harness(db, true).await;
    name_pilots(&h).await;
    let owner = log_in_owner(&h, CHRIBBA).await;
    let pilot = log_in_as(&h, GIGX, None).await;
    let officer = log_in_as(&h, MITTANI, None).await;
    let officer_account = account_of(&h, &officer).await;
    assert_eq!(
        page(&h, "/blacklist", &pilot).await.status,
        StatusCode::FORBIDDEN
    );
    // The officer reads every note and adds them, and comments, but no
    // tier and no blacklisting.
    let group = group_with(
        &h,
        &owner,
        &[
            "blacklist.view_eve_notes",
            "blacklist.view_eve_blacklist",
            "blacklist.add_new_eve_notes",
            "blacklist.view_eve_note_comments",
            "blacklist.add_new_eve_note_comments",
        ],
        &[officer_account],
    )
    .await;
    for (body, why) in [
        ("who=98133756&reason=x&blacklisted=on", "blacklist"),
        ("who=98133756&reason=x&restricted=on", "restricted"),
        ("who=98133756&reason=x&ultra_restricted=on", "ultra"),
    ] {
        let res = send(&h.app, form("/blacklist/notes", body, &officer)).await;
        assert_eq!(res.status, StatusCode::FORBIDDEN, "{body}");
        assert!(res.body.contains(why), "{body}: {}", res.body);
    }
    let res = send(
        &h.app,
        form(
            "/blacklist/notes",
            "who=98133756&reason=Scammed+a+buyback",
            &officer,
        ),
    )
    .await;
    assert_eq!(res.location(), "/blacklist", "{}", res.body);
    // The owner adds a restricted note, blacklisting, and an ultra one.
    for body in [
        "who=1887431749&reason=Known+awoxer&restricted=on&blacklisted=on",
        "who=1887431749&reason=Deep+cover&ultra_restricted=on",
    ] {
        let res = send(&h.app, form("/blacklist/notes", body, &owner)).await;
        assert_eq!(res.location(), "/blacklist", "{body}: {}", res.body);
    }
    let notes: Vec<(i64, String)> =
        sqlx::query_as("SELECT id, note FROM core.pilot_notes ORDER BY id")
            .fetch_all(&h.db)
            .await
            .unwrap();
    let log = page(&h, "/blacklist", &officer).await.body;
    assert!(log.contains("Scammed a buyback"), "{log}");
    assert!(!log.contains("Deep cover"), "ultra restricted: {log}");
    // On the Blacklist, the restricted entry shows, not its reason.
    assert!(log.contains("gigX"), "{log}");
    assert!(!log.contains("Known awoxer"), "restricted: {log}");
    assert!(log.contains("Restricted: ask Chribba"), "{log}");
    // Nor taken off the Blacklist blind (given the permission to).
    let res = send(
        &h.app,
        post_json(
            "/api/admin/permissions/grants",
            &owner,
            &format!(r#"{{"permission":"blacklist.add_to_blacklist","group_id":{group}}}"#),
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.body);
    let res = send(&h.app, form("/blacklist/1887431749/remove", "", &officer)).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.body);
    // A restricted note can't be commented on or edited blind.
    for path in [
        format!("/blacklist/notes/{}/comments", notes[1].0),
        format!("/blacklist/notes/{}/edit", notes[1].0),
    ] {
        let res = send(&h.app, form(&path, "comment=x&reason=x", &officer)).await;
        assert_eq!(res.status, StatusCode::NOT_FOUND, "{path}");
    }

    // Comments, with their own tiers.
    let res = send(
        &h.app,
        form(
            &format!("/blacklist/notes/{}/comments", notes[0].0),
            "comment=Confirmed+by+two+directors",
            &officer,
        ),
    )
    .await;
    assert_eq!(res.location(), "/blacklist", "{}", res.body);
    let res = send(
        &h.app,
        form(
            &format!("/blacklist/notes/{}/comments", notes[0].0),
            "comment=Source+inside&restricted=on",
            &officer,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.body);
    let res = send(
        &h.app,
        form(
            &format!("/blacklist/notes/{}/comments", notes[0].0),
            "comment=Source+inside&restricted=on",
            &owner,
        ),
    )
    .await;
    assert_eq!(res.location(), "/blacklist", "{}", res.body);
    let log = page(&h, "/blacklist", &officer).await.body;
    assert!(log.contains("Confirmed by two directors"), "{log}");
    assert!(!log.contains("Source inside"), "restricted comment: {log}");

    // With the tiers, everything shows.
    for permission in [
        "blacklist.view_restricted_eve_notes",
        "blacklist.view_ultra_restricted_eve_notes",
        "blacklist.view_eve_note_restricted_comments",
    ] {
        send(
            &h.app,
            post_json(
                "/api/admin/permissions/grants",
                &owner,
                &format!(r#"{{"permission":"{permission}","group_id":{group}}}"#),
            ),
        )
        .await;
    }
    let log = page(&h, "/blacklist", &officer).await.body;
    for text in ["Known awoxer", "Deep cover", "Source inside"] {
        assert!(log.contains(text), "{text}: {log}");
    }

    // Editing keeps the flags the editor can't set (restricted here).
    let res = send(
        &h.app,
        form(
            &format!("/blacklist/notes/{}/edit", notes[1].0),
            "reason=Known+awoxer%2C+twice&blacklisted=on",
            &officer,
        ),
    )
    .await;
    assert_eq!(res.location(), "/blacklist", "{}", res.body);
    let (note, blacklisted, restricted): (String, bool, bool) =
        sqlx::query_as("SELECT note, blacklisted, restricted FROM core.pilot_notes WHERE id = $1")
            .bind(notes[1].0)
            .fetch_one(&h.db)
            .await
            .unwrap();
    assert_eq!(note, "Known awoxer, twice");
    assert!(blacklisted && restricted);

    // Only superusers delete (AA: the Django admin).
    let path = format!("/blacklist/notes/{}/delete", notes[0].0);
    let res = send(&h.app, form(&path, "", &officer)).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN);
    let res = send(&h.app, form(&path, "", &owner)).await;
    assert_eq!(res.location(), "/blacklist");
    let left: i64 = sqlx::query_scalar("SELECT count(*) FROM core.pilot_notes")
        .fetch_one(&h.db)
        .await
        .unwrap();
    assert_eq!(left, 2);
    assert!(
        page(&h, "/admin", &officer)
            .await
            .body
            .contains(r#"href="/blacklist""#)
    );
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn basic_notes_are_your_own_corporations(db: PgPool) {
    let h = harness(db, true).await;
    name_pilots(&h).await;
    let owner = log_in_owner(&h, CHRIBBA).await;
    let recruiter = log_in_as(&h, MITTANI, None).await; // corp 1000167
    let account = account_of(&h, &recruiter).await;
    group_with(
        &h,
        &owner,
        &[
            "blacklist.view_basic_eve_notes",
            "blacklist.add_basic_eve_notes",
        ],
        &[account],
    )
    .await;
    // Pilots in other corporations, or corporations: not theirs to note.
    for who in ["1887431749", "98133756"] {
        let res = send(
            &h.app,
            form(
                "/blacklist/notes",
                &format!("who={who}&reason=x"),
                &recruiter,
            ),
        )
        .await;
        assert_eq!(res.status, StatusCode::FORBIDDEN, "{who}: {}", res.body);
    }
    // Their own corporation's pilots: yes.
    let res = send(
        &h.app,
        form(
            "/blacklist/notes",
            "who=443630591&reason=Talks+too+much",
            &recruiter,
        ),
    )
    .await;
    assert_eq!(res.location(), "/blacklist", "{}", res.body);
    let res = send(
        &h.app,
        form(
            "/blacklist/notes",
            "who=1887431749&reason=Elsewhere",
            &owner,
        ),
    )
    .await;
    assert_eq!(res.location(), "/blacklist", "{}", res.body);
    let log = page(&h, "/blacklist", &recruiter).await.body;
    assert!(log.contains("Talks too much"), "{log}");
    assert!(!log.contains("Elsewhere"), "another corporation's: {log}");
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn blacklisting_never_reaches_past_what_you_hold(db: PgPool) {
    cover(&db, Builtin::Member, EntityKind::Alliance, 1695357456).await;
    let h = harness(db, true).await;
    name_pilots(&h).await;
    let owner = log_in_owner(&h, CHRIBBA).await;
    let admin = log_in_as(&h, GIGX, None).await; // corp 98133756
    let officer = log_in_as(&h, MITTANI, None).await;
    // gigX holds admin.states; the officer can blacklist but doesn't.
    group_with(
        &h,
        &owner,
        &["admin.states"],
        &[account_of(&h, &admin).await],
    )
    .await;
    group_with(
        &h,
        &owner,
        &[
            "blacklist.view_eve_blacklist",
            "blacklist.add_new_eve_notes",
            "blacklist.add_to_blacklist",
        ],
        &[account_of(&h, &officer).await],
    )
    .await;
    let res = send(
        &h.app,
        form("/blacklist", "who=98133756&reason=Coup", &officer),
    )
    .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.body);
    assert!(res.body.contains("admin.states"), "{}", res.body);
    assert_eq!(state_of(&h, &admin).await, "Member");

    // The Blacklist state never holds a sensitive permission: anyone can
    // walk into a blacklisted NPC corporation.
    let res = send(
        &h.app,
        post_json(
            "/api/admin/permissions/grants",
            &owner,
            &format!(
                r#"{{"permission":"compliance.view","state_id":{}}}"#,
                blacklist_state(&h).await
            ),
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST, "{}", res.body);
    // Nor can anyone blacklist past what the Blacklist state is granted:
    // blacklisting would hand it out.
    let res = send(
        &h.app,
        post_json(
            "/api/admin/permissions/grants",
            &owner,
            &format!(
                r#"{{"permission":"discord.access_discord","state_id":{}}}"#,
                blacklist_state(&h).await
            ),
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.body);
    let res = send(&h.app, form("/blacklist", "who=1000167&reason=x", &officer)).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.body);
    assert!(res.body.contains("discord.access_discord"), "{}", res.body);
}

/// Apps follow what the account holds, as core does: blacklisted, it may
/// use an app while the Blacklist state is granted one of its permissions.
#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_blacklisted_account_uses_the_apps_its_state_is_granted(db: PgPool) {
    cover(&db, Builtin::Member, EntityKind::Alliance, 1695357456).await;
    let h = harness(db, true).await;
    name_pilots(&h).await;
    let owner = log_in_owner(&h, CHRIBBA).await;
    let pilot = log_in_as(&h, GIGX, None).await;
    let pilot_account = tether_db::accounts::AccountId(account_of(&h, &pilot).await);
    let key = tether_plugins::testing::Key::new(7);
    let manifest = format!(
        "[plugin]\nid = \"acme.book\"\nname = \"Book\"\nversion = \"1.0.0\"\nhost_api = \"1\"\n\n\
         [publisher]\nkey = \"{}\"\n\n[permissions]\nview = \"See\"\n\n\
         [[pages]]\npath = \"\"\npermission = \"view\"\n",
        key.public()
    );
    let component = build_guest("tether-plugins-test-guest-storage");
    let bytes = tether_plugins::testing::zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("plugin.wasm", &component),
    ]);
    install_package(&h, &owner, &bytes, &key.sign(&bytes)).await;
    let holds = || async {
        tether_db::compliance::holds_app_permission(&h.db, pilot_account, "acme.book")
            .await
            .unwrap()
    };

    let res = send(
        &h.app,
        form("/blacklist", "who=98133756&reason=Awoxed+a+Rorqual", &owner),
    )
    .await;
    assert_eq!(res.location(), "/blacklist", "{}", res.body);
    evaluate(&h, pilot_account.0).await;
    assert_eq!(state_of(&h, &pilot).await, "Blacklist");
    assert!(!holds().await);

    let res = send(
        &h.app,
        post_json(
            "/api/admin/permissions/grants",
            &owner,
            &format!(
                r#"{{"permission":"plugin.acme.book.view","state_id":{}}}"#,
                blacklist_state(&h).await
            ),
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.body);
    assert!(holds().await, "the Blacklist state's grant");
    assert!(
        permissions_of(&h, &pilot)
            .await
            .contains(&"plugin.acme.book.view".to_owned())
    );
    // Deactivated: nothing, whatever is granted.
    sqlx::query("UPDATE core.accounts SET active = false WHERE id = $1")
        .bind(pilot_account.0)
        .execute(&h.db)
        .await
        .unwrap();
    assert!(!holds().await);
}
