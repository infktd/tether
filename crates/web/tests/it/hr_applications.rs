//! The HR Applications plugin end to end: installed from its real
//! component and migration; forms per corporation with written, pick-one
//! and tick-any questions; pilots applying and following their status;
//! reviewers scoped to their main's corporation marking in progress,
//! commenting, approving, rejecting and deleting by permission.

use std::sync::OnceLock;

use crate::common::*;
use axum::http::StatusCode;
use sqlx::PgPool;
use tether_core::states::{Builtin, EntityKind};
use tether_plugins::testing::{self, Key};
use wiremock::matchers::{method, path};
use wiremock::{Mock, ResponseTemplate};

const ID: &str = "tether.hr-applications";
const OWNER_CORP: i64 = 1164409536;
const BLUE_CORP: i64 = 98133756;
/// Two recruiters of the same (NPC) corporation, from the affiliation
/// fixture.
const NPC_CORP: i64 = 1000167;

fn component() -> Vec<u8> {
    static COMPONENT: OnceLock<Vec<u8>> = OnceLock::new();
    COMPONENT
        .get_or_init(|| build_guest("hr-applications"))
        .clone()
}

fn plugin_file(name: &str) -> String {
    std::fs::read_to_string(format!(
        "{}/../../plugins/hr-applications/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

async fn install(h: &Harness, owner: &str) {
    let key = Key::new(10);
    let manifest = plugin_file("plugin.toml").replace("PUBLISHER_KEY", &key.public());
    let migration = plugin_file("migrations/0001_hr_applications.sql");
    let component = component();
    let bytes = testing::zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("plugin.wasm", &component),
        ("migrations/0001_hr_applications.sql", migration.as_bytes()),
    ]);
    let at = install_package(h, owner, &bytes, &key.sign(&bytes)).await;
    assert_eq!(at, format!("/admin/plugins/{ID}"));
}

/// Names for the corporations and alliances involved.
async fn mount_names(h: &Harness) {
    Mock::given(method("POST"))
        .and(path("/universe/names"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            { "id": NPC_CORP, "name": "Science and Trade Institute", "category": "corporation" },
            { "id": OWNER_CORP, "name": "Otherworld Enterprises", "category": "corporation" },
            { "id": BLUE_CORP, "name": "CircleOfTwo Holding", "category": "corporation" },
            { "id": 159826257, "name": "Otherworld Empire", "category": "alliance" },
            { "id": 1695357456, "name": "Circle-Of-Two", "category": "alliance" },
        ])))
        // Before the harness's own names fixture.
        .with_priority(1)
        .mount(&h.esi_server)
        .await;
}

async fn grant(h: &Harness, owner: &str, permission: &str, state: i64) {
    let res = send(
        &h.app,
        form(
            "/admin/permissions/grant",
            &format!("permission=plugin.{ID}.{permission}&grantee=state:{state}"),
            owner,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
}

/// A page's address: `""` is the main page.
fn at(uri: &str) -> String {
    if uri.is_empty() {
        format!("/plugins/{ID}")
    } else {
        format!("/plugins/{ID}/{uri}")
    }
}

async fn post(h: &Harness, token: &str, uri: &str, body: &str) -> Res {
    send(&h.app, form(&at(uri), body, token)).await
}

async fn open(h: &Harness, token: &str, uri: &str) -> Res {
    page(h, &at(uri), token).await
}

async fn form_of(h: &Harness, corporation: i64) -> i64 {
    sqlx::query_scalar(
        "SELECT id FROM \"plugin_tether.hr-applications\".forms WHERE corporation_id = $1",
    )
    .bind(corporation)
    .fetch_one(&h.db)
    .await
    .unwrap()
}

async fn application_of(h: &Harness, main: &str, form: i64) -> i64 {
    sqlx::query_scalar(
        "SELECT id FROM \"plugin_tether.hr-applications\".applications \
         WHERE main_name = $1 AND form_id = $2",
    )
    .bind(main)
    .bind(form)
    .fetch_one(&h.db)
    .await
    .unwrap()
}

async fn applications_from(h: &Harness, main: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM \"plugin_tether.hr-applications\".applications WHERE main_name = $1",
    )
    .bind(main)
    .fetch_one(&h.db)
    .await
    .unwrap()
}

async fn question(h: &Harness, title: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT id FROM \"plugin_tether.hr-applications\".questions WHERE title = $1",
    )
    .bind(title)
    .fetch_one(&h.db)
    .await
    .unwrap()
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn hr_applications_end_to_end(db: PgPool) {
    cover(&db, Builtin::Member, EntityKind::Alliance, 159826257).await;
    cover(&db, Builtin::Member, EntityKind::Corporation, NPC_CORP).await;
    cover(&db, Builtin::Blue, EntityKind::Corporation, BLUE_CORP).await;
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    install(&h, &owner).await;
    mount_names(&h).await;

    // Application Forms: one per corporation, picked from the manager's
    // corporations or given by ID, checked with EVE.
    let forms = open(&h, &owner, "forms").await;
    assert_eq!(forms.status, StatusCode::OK, "{}", forms.body);
    assert!(
        forms.body.contains("Otherworld Enterprises"),
        "{}",
        forms.body
    );
    let res = post(
        &h,
        &owner,
        "forms",
        &format!("_form=add_form&corporation={OWNER_CORP}&corporation_id={NPC_CORP}"),
    )
    .await;
    assert!(res.body.contains("not both"), "{}", res.body);
    let res = post(
        &h,
        &owner,
        "forms",
        "_form=add_form&corporation=&corporation_id=99999999",
    )
    .await;
    assert!(res.body.contains("isn&#39;t a corporation"), "{}", res.body);
    // Once their only corporation has a form, only the ID is asked.
    for body in [
        format!("_form=add_form&corporation={OWNER_CORP}&corporation_id="),
        format!("_form=add_form&corporation_id={NPC_CORP}"),
        format!("_form=add_form&corporation_id={BLUE_CORP}"),
    ] {
        let res = post(&h, &owner, "forms", &body).await;
        assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    }
    let again = post(
        &h,
        &owner,
        "forms",
        &format!("_form=add_form&corporation_id={NPC_CORP}"),
    )
    .await;
    assert!(again.body.contains("has a form already"), "{}", again.body);
    let npc_form = form_of(&h, NPC_CORP).await;
    let owner_form = form_of(&h, OWNER_CORP).await;
    let blue_form = form_of(&h, BLUE_CORP).await;

    // Questions: written, pick one, tick any.
    let questions = format!("forms/{npc_form}");
    for body in [
        "_form=add_question&title=What+do+you+fly%3F&help_text=&choices=Mining%0APvP%0AIndustry&multi_select=on",
        "_form=add_question&title=Why+us%3F&help_text=A+few+lines&choices=",
        "_form=add_question&title=Timezone&help_text=&choices=EU%0AUS%0AAU",
    ] {
        let res = post(&h, &owner, &questions, body).await;
        assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    }
    let bad = post(
        &h,
        &owner,
        &questions,
        "_form=add_question&title=Pick&help_text=&choices=&multi_select=on",
    )
    .await;
    assert!(bad.body.contains("Give the choices"), "{}", bad.body);
    let many: String = (0..20).map(|i| format!("c{i}%0A")).collect();
    let res = post(
        &h,
        &owner,
        &questions,
        &format!("_form=add_question&title=Skills&help_text=&choices={many}&multi_select=on"),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let over = post(
        &h,
        &owner,
        &questions,
        &format!("_form=add_question&title=More&help_text=&choices={many}&multi_select=on"),
    )
    .await;
    assert!(over.body.contains("answer fields"), "{}", over.body);
    let skills = question(&h, "Skills").await;
    let res = post(
        &h,
        &owner,
        &questions,
        &format!("_form=delete_question&question={skills}&confirm=on"),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    // Reorder: "What do you fly?" goes last; then edit it.
    let fly = question(&h, "What do you fly?").await;
    for _ in 0..2 {
        let res = post(
            &h,
            &owner,
            &questions,
            &format!("_form=move_question&question={fly}&direction=down"),
        )
        .await;
        assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    }
    let res = post(
        &h,
        &owner,
        &format!("forms/{npc_form}/question/{fly}"),
        "_form=edit_question&title=What+do+you+fly%3F&help_text=Tick+all&choices=Mining%0APvP%0AIndustry%0AExploration&multi_select=on",
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let form_page = open(&h, &owner, &questions).await;
    let (why, tz, fly_at) = (
        form_page.body.find("Why us?").unwrap(),
        form_page.body.find("Timezone").unwrap(),
        form_page.body.find("What do you fly?").unwrap(),
    );
    assert!(why < tz && tz < fly_at, "{}", form_page.body);
    assert!(form_page.body.contains("Exploration"));
    let why_q = question(&h, "Why us?").await;
    let tz_q = question(&h, "Timezone").await;

    // Pilots apply once granted basic (Guests too: they're who applies).
    let pilot = log_in(&h, None).await;
    let blue = log_in_as(&h, "1887431749:gigX", None).await;
    let a = log_in_as(&h, "443630591:Pilot A", None).await;
    let b = log_in_as(&h, "406944591:Pilot B", None).await;
    assert_eq!(open(&h, &pilot, "").await.status, StatusCode::NOT_FOUND);
    for state in [GUEST_STATE, BLUE_STATE, MEMBER_STATE] {
        grant(&h, &owner, "basic", state).await;
    }
    let mine = open(&h, &pilot, "").await;
    assert_eq!(mine.status, StatusCode::OK, "{}", mine.body);
    assert!(
        mine.body.contains("Apply to Science and Trade Institute"),
        "{}",
        mine.body
    );
    let apply = open(&h, &pilot, &format!("apply/{npc_form}")).await;
    assert!(apply.body.contains("Why us?"), "{}", apply.body);
    assert!(apply.body.contains("What do you fly?: Exploration"));
    let answers = format!("_form=apply&q{why_q}=Rocks+are+great&q{tz_q}=0&q{fly}_0=on&q{fly}_3=on");
    // Consent is required.
    let refused = post(&h, &pilot, &format!("apply/{npc_form}"), &answers).await;
    assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);
    let res = post(
        &h,
        &pilot,
        &format!("apply/{npc_form}"),
        &format!("{answers}&consent=on"),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let pilot_app = application_of(&h, "Pilot", npc_form).await;
    assert_eq!(res.location(), format!("/plugins/{ID}/view/{pilot_app}"));
    let view = open(&h, &pilot, &format!("view/{pilot_app}")).await;
    assert!(view.body.contains("Pending"), "{}", view.body);
    assert!(view.body.contains("Rocks are great"));
    assert!(view.body.contains("EU"));
    assert!(view.body.contains("Mining, Exploration"), "{}", view.body);
    // Once per corporation.
    let twice = open(&h, &pilot, &format!("apply/{npc_form}")).await;
    assert!(twice.body.contains("already applied"), "{}", twice.body);
    let res = post(
        &h,
        &pilot,
        &format!("apply/{npc_form}"),
        &format!("{answers}&consent=on"),
    )
    .await;
    assert_eq!(res.status, StatusCode::CONFLICT, "{}", res.body);
    // Nobody else sees it as theirs.
    assert_eq!(
        open(&h, &blue, &format!("view/{pilot_app}")).await.status,
        StatusCode::NOT_FOUND
    );

    // gigX applies to two forms (with no questions: just the consent).
    for f in [owner_form, npc_form] {
        let body = if f == npc_form {
            format!("{answers}&consent=on")
        } else {
            "_form=apply&consent=on".to_owned()
        };
        let res = post(&h, &blue, &format!("apply/{f}"), &body).await;
        assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    }
    let blue_owner_app = application_of(&h, "gigX", owner_form).await;
    let blue_npc_app = application_of(&h, "gigX", npc_form).await;
    // ...and withdraws one while it's pending.
    let res = post(
        &h,
        &blue,
        &format!("view/{blue_npc_app}"),
        "_form=delete&confirm=on",
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert_eq!(
        open(&h, &blue, &format!("view/{blue_npc_app}"))
            .await
            .status,
        StatusCode::NOT_FOUND
    );

    // Reviewers: Members get human_resources, approve and reject (not
    // delete). Only applications to their main's corporation.
    for uri in ["review", "forms"] {
        assert_eq!(
            open(&h, &a, uri).await.status,
            StatusCode::NOT_FOUND,
            "{uri}"
        );
    }
    for permission in [
        "human_resources",
        "approve_application",
        "reject_application",
    ] {
        grant(&h, &owner, permission, MEMBER_STATE).await;
    }
    let queue = open(&h, &a, "review").await;
    assert_eq!(queue.status, StatusCode::OK, "{}", queue.body);
    assert!(
        queue.body.contains(&format!("review/{pilot_app}")),
        "{}",
        queue.body
    );
    assert!(!queue.body.contains(&format!("review/{blue_owner_app}")));
    assert_eq!(
        open(&h, &a, &format!("review/{blue_owner_app}"))
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(open(&h, &a, "forms").await.status, StatusCode::NOT_FOUND);
    // Guests can't review.
    assert_eq!(
        open(&h, &pilot, "review").await.status,
        StatusCode::NOT_FOUND
    );

    // The application, with the applicant's characters.
    let review = format!("review/{pilot_app}");
    let seen = open(&h, &a, &review).await;
    assert_eq!(seen.status, StatusCode::OK, "{}", seen.body);
    assert!(seen.body.contains("Rocks are great"));
    assert!(seen.body.contains("Characters"));
    assert!(seen.body.contains("<td>Pilot</td>"), "{}", seen.body);
    assert!(seen.body.contains("Mark in Progress"));
    assert!(!seen.body.contains("Save Decision"), "{}", seen.body);
    assert!(!seen.body.contains("Delete Application"));

    // Nobody decides before marking it in progress; then only its reviewer.
    let res = post(&h, &b, &review, "_form=decide&decision=approve").await;
    assert_eq!(res.status, StatusCode::CONFLICT, "{}", res.body);
    let res = post(&h, &a, &review, "_form=claim&confirm=on").await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let for_b = open(&h, &b, &review).await;
    assert!(for_b.body.contains("In Progress"), "{}", for_b.body);
    assert!(for_b.body.contains("Pilot A"));
    assert!(!for_b.body.contains("Mark in Progress"));
    for body in ["_form=claim&confirm=on", "_form=decide&decision=reject"] {
        let res = post(&h, &b, &review, body).await;
        assert_eq!(res.status, StatusCode::CONFLICT, "{body}: {}", res.body);
    }
    let res = post(&h, &b, &review, "_form=comment&comment=Knows+his+rocks").await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert!(open(&h, &a, &review).await.body.contains("Knows his rocks"));
    // The applicant sees the status, not the comments.
    let view = open(&h, &pilot, &format!("view/{pilot_app}")).await;
    assert!(view.body.contains("In Progress"), "{}", view.body);
    assert!(!view.body.contains("Knows his rocks"));
    // ...and can't withdraw it now it's being reviewed (the notes stay).
    assert!(!view.body.contains("Delete Application"), "{}", view.body);
    let res = post(
        &h,
        &pilot,
        &format!("view/{pilot_app}"),
        "_form=delete&confirm=on",
    )
    .await;
    assert_eq!(res.status, StatusCode::CONFLICT, "{}", res.body);
    // Comments are capped, so the page always fits the host's limits.
    sqlx::query(
        "INSERT INTO \"plugin_tether.hr-applications\".comments \
         (application_id, author_account_id, author_character_id, author_name, body) \
         SELECT $1, 0, 0, 'Filler', repeat('é', 500) FROM generate_series(1, 199)",
    )
    .bind(pilot_app)
    .execute(&h.db)
    .await
    .unwrap();
    let full = post(&h, &b, &review, "_form=comment&comment=One+more").await;
    assert!(full.body.contains("at most 200 comments"), "{}", full.body);
    assert_eq!(open(&h, &a, &review).await.status, StatusCode::OK);
    // Deleting needs delete_application.
    let res = post(&h, &a, &review, "_form=delete&confirm=on").await;
    assert_eq!(res.status, StatusCode::CONFLICT, "{}", res.body);

    let res = post(&h, &a, &review, "_form=decide&decision=approve").await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let view = open(&h, &pilot, &format!("view/{pilot_app}")).await;
    assert!(view.body.contains("Approved"), "{}", view.body);
    assert!(!view.body.contains("Delete Application"));
    let res = post(
        &h,
        &pilot,
        &format!("view/{pilot_app}"),
        "_form=delete&confirm=on",
    )
    .await;
    assert_eq!(res.status, StatusCode::CONFLICT, "{}", res.body);
    let res = post(&h, &a, &review, "_form=decide&decision=reject").await;
    assert_eq!(res.status, StatusCode::CONFLICT, "{}", res.body);
    let reviewed = open(&h, &a, "review?_tab=1").await;
    assert!(reviewed.body.contains("Approved"), "{}", reviewed.body);

    // The owner (every permission, all corporations): search, reject
    // without marking in progress first, delete; never their own.
    let found = post(&h, &owner, "review", "_form=search&q=GIGX").await;
    assert_eq!(found.status, StatusCode::OK, "{}", found.body);
    assert!(found.body.contains(&format!("review/{blue_owner_app}")));
    assert!(!found.body.contains(&format!("review/{pilot_app}")));
    let res = post(
        &h,
        &owner,
        &format!("review/{blue_owner_app}"),
        "_form=decide&decision=reject",
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert!(
        open(&h, &blue, &format!("view/{blue_owner_app}"))
            .await
            .body
            .contains("Rejected")
    );
    let res = post(
        &h,
        &owner,
        &format!("review/{blue_owner_app}"),
        "_form=delete&confirm=on",
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert_eq!(
        open(&h, &blue, &format!("view/{blue_owner_app}"))
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    let res = post(
        &h,
        &owner,
        &format!("apply/{blue_form}"),
        "_form=apply&consent=on",
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let own = application_of(&h, "Chribba", blue_form).await;
    assert_eq!(
        open(&h, &owner, &format!("review/{own}")).await.status,
        StatusCode::NOT_FOUND
    );

    // Deleting a form takes its applications with it.
    let res = post(
        &h,
        &owner,
        &format!("forms/{blue_form}"),
        "_form=delete_form&confirm=on",
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert_eq!(applications_from(&h, "Chribba").await, 0);

    let problems: Vec<String> = sqlx::query_scalar(
        "SELECT message FROM core.plugin_logs WHERE plugin_id = $1 AND level IN ('warn', 'error')",
    )
    .bind(ID)
    .fetch_all(&h.db)
    .await
    .unwrap();
    assert!(problems.is_empty(), "{problems:?}");
}
