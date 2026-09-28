//! The Fittings plugin end to end (allianceauth-fittings): installed from
//! its real component and migration, fits added from EFT text with names
//! looked up through the mocked ESI (and cached), item details and
//! required skills filled in by its job, doctrines and categories, AA's
//! two permissions, and categories limited to groups.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use crate::common::*;
use axum::http::StatusCode;
use sqlx::PgPool;
use tether_core::groups::Flags;
use tether_core::states::{Builtin, EntityKind};
use tether_jobs::{Outcome, Registry, WorkerConfig, run_once};
use tether_plugins::testing::{self, Key};
use wiremock::matchers::{method, path, path_regex};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const ID: &str = "tether.fittings";
/// A pilot of an NPC corporation Member covers, from the affiliation
/// fixture.
const PILOT_A: &str = "443630591:Pilot A";
const NPC_CORP: i64 = 1000167;

fn component() -> Vec<u8> {
    static COMPONENT: OnceLock<Vec<u8>> = OnceLock::new();
    COMPONENT.get_or_init(|| build_guest("fittings")).clone()
}

fn plugin_file(name: &str) -> String {
    std::fs::read_to_string(format!(
        "{}/../../plugins/fittings/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

async fn install(h: &Harness, owner: &str) {
    let key = Key::new(11);
    let manifest = plugin_file("plugin.toml").replace("PUBLISHER_KEY", &key.public());
    let first = plugin_file("migrations/0001_fittings.sql");
    let pilots = plugin_file("migrations/0002_pilots.sql");
    let component = component();
    let bytes = testing::zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("plugin.wasm", &component),
        ("migrations/0001_fittings.sql", first.as_bytes()),
        ("migrations/0002_pilots.sql", pilots.as_bytes()),
    ]);
    let at = install_package(h, owner, &bytes, &key.sign(&bytes)).await;
    assert_eq!(at, format!("/admin/plugins/{ID}"));
}

async fn grant(h: &Harness, owner: &str, permission: &str, state: i64) {
    let res = send(
        &h.app,
        form(
            "/admin/permissions/set",
            &format!("permission=plugin.{ID}.{permission}&grantee=state:{state}"),
            owner,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
}

fn urlencode(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

// ---- mocked ESI --------------------------------------------------------------

/// A required skill and its level.
type Skill = Option<(i64, i64)>;

/// name, type id, item group, category, required skill and level.
const TYPES: &[(&str, i64, i64, i64, Skill)] = &[
    ("Rifter", 587, 25, 6, Some((3329, 1))),
    ("Damage Control II", 2048, 60, 7, Some((3318, 3))),
    ("Warp Scrambler II", 3244, 52, 7, Some((3435, 4))),
    ("200mm AutoCannon II", 2881, 55, 7, Some((3302, 5))),
    ("Republic Fleet EMP S", 21894, 83, 8, None),
    ("Small Projectile Burst Aerator I", 31668, 781, 7, None),
    ("Warrior II", 2488, 100, 18, Some((3436, 5))),
    ("Nanite Repair Paste", 28668, 916, 17, None),
    ("Slasher", 585, 25, 6, Some((3329, 1))),
];

/// Skills, for `/universe/names`.
const SKILLS: &[(i64, &str)] = &[
    (3329, "Minmatar Frigate"),
    (3318, "Weapon Upgrades"),
    (3435, "Propulsion Jamming"),
    (3302, "Small Projectile Turret"),
    (3436, "Drones"),
];

/// `/universe/ids`: the item names it knows, as EVE writes them, matched
/// without case like ESI.
struct Ids;

impl Respond for Ids {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let names: Vec<String> = serde_json::from_slice(&request.body).unwrap();
        let found: Vec<serde_json::Value> = names
            .iter()
            .filter_map(|n| {
                TYPES
                    .iter()
                    .find(|t| t.0.eq_ignore_ascii_case(n))
                    .map(|t| serde_json::json!({ "id": t.1, "name": t.0 }))
            })
            .collect();
        if found.is_empty() {
            ResponseTemplate::new(200).set_body_json(serde_json::json!({}))
        } else {
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({ "inventory_types": found }))
        }
    }
}

/// `/universe/types/{id}`.
struct Types;

impl Respond for Types {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let id: i64 = request
            .url
            .path()
            .rsplit('/')
            .next()
            .unwrap()
            .parse()
            .unwrap();
        let Some(t) = TYPES.iter().find(|t| t.1 == id) else {
            return ResponseTemplate::new(404)
                .set_body_json(serde_json::json!({ "error": "Type not found" }));
        };
        let mut attributes = vec![serde_json::json!({ "attribute_id": 9, "value": 350.0 })];
        if let Some((skill, level)) = t.4 {
            attributes.push(serde_json::json!({ "attribute_id": 182, "value": skill as f64 }));
            attributes.push(serde_json::json!({ "attribute_id": 277, "value": level as f64 }));
        }
        ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "type_id": t.1, "name": t.0, "description": "", "group_id": t.2,
            "published": true, "dogma_attributes": attributes
        }))
    }
}

/// `/universe/groups/{id}`.
struct Groups;

impl Respond for Groups {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let id: i64 = request
            .url
            .path()
            .rsplit('/')
            .next()
            .unwrap()
            .parse()
            .unwrap();
        let category = TYPES.iter().find(|t| t.2 == id).map_or(0, |t| t.3);
        ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "group_id": id, "name": format!("Group {id}"), "category_id": category,
            "published": true, "types": []
        }))
    }
}

/// `/universe/names`: the recorded fixture, plus the skills.
struct Names(BTreeMap<i64, serde_json::Value>);

impl Respond for Names {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let ids: Vec<i64> = serde_json::from_slice(&request.body).unwrap();
        let found: Vec<&serde_json::Value> = ids.iter().filter_map(|id| self.0.get(id)).collect();
        ResponseTemplate::new(200).set_body_json(found)
    }
}

async fn esi_server() -> MockServer {
    let server = MockServer::start().await;
    mount_affiliations(&server).await;
    Mock::given(method("POST"))
        .and(path("/universe/ids"))
        .respond_with(Ids)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path_regex(r"^/universe/types/\d+$"))
        .respond_with(Types)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path_regex(r"^/universe/groups/\d+$"))
        .respond_with(Groups)
        .mount(&server)
        .await;
    let fixture: Vec<serde_json::Value> = serde_json::from_str(
        &std::fs::read_to_string(format!(
            "{}/../../tests/fixtures/esi/universe_names.json",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap(),
    )
    .unwrap();
    let mut names: BTreeMap<i64, serde_json::Value> = fixture
        .into_iter()
        .map(|n| (n["id"].as_i64().unwrap(), n))
        .collect();
    for (id, name) in SKILLS {
        names.insert(
            *id,
            serde_json::json!({ "id": id, "name": name, "category": "inventory_type" }),
        );
    }
    Mock::given(method("POST"))
        .and(path("/universe/names"))
        .respond_with(Names(names))
        .mount(&server)
        .await;
    server
}

async fn ids_lookups(h: &Harness) -> usize {
    h.esi_server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == "/universe/ids")
        .count()
}

async fn work(h: &Harness) {
    let mut registry = Registry::new();
    tether_web::plugin_jobs::register_jobs(&mut registry, h.db.clone(), h.plugins.clone());
    let config = WorkerConfig::default();
    while run_once(&h.db, &registry, &config).await.unwrap() != Outcome::Idle {}
}

// ---- helpers -----------------------------------------------------------------

const RIFTER: &str = "[Rifter, Fast Tackle]\n\
Damage Control II\n\
[Empty Low slot]\n\
\n\
Warp Scrambler II /OFFLINE\n\
\n\
200mm AutoCannon II, Republic Fleet EMP S\n\
200mm AutoCannon II, Republic Fleet EMP S\n\
[Empty High slot]\n\
\n\
Small Projectile Burst Aerator I\n\
\n\
\n\
Warrior II x3\n\
\n\
Nanite Repair Paste x50\n";

async fn post(h: &Harness, token: &str, uri: &str, body: &str) -> Res {
    send(&h.app, form(&format!("/plugins/{ID}/{uri}"), body, token)).await
}

async fn open(h: &Harness, token: &str, uri: &str) -> Res {
    if uri.is_empty() {
        return page(h, &format!("/plugins/{ID}"), token).await;
    }
    page(h, &format!("/plugins/{ID}/{uri}"), token).await
}

async fn add_fit(h: &Harness, token: &str, eft: &str, extra: &str) -> Res {
    post(
        h,
        token,
        "add-fit",
        &format!(
            "_form=fit&eft={}&role=Tackle&description=Burn+in{extra}",
            urlencode(eft)
        ),
    )
    .await
}

async fn one(h: &Harness, sql: &str) -> i64 {
    sqlx::query_scalar(sqlx::AssertSqlSafe(
        sql.replace("{schema}", &format!("\"plugin_{ID}\"")),
    ))
    .fetch_one(&h.db)
    .await
    .unwrap()
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn fittings_end_to_end(db: PgPool) {
    cover(&db, Builtin::Member, EntityKind::Corporation, NPC_CORP).await;
    let h = harness_with_esi(db, true, esi_server().await).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    install(&h, &owner).await;

    // A fit from EFT: name from its header, straight to its page.
    let res = add_fit(&h, &owner, RIFTER, "&doctrine=").await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let rifter = one(
        &h,
        "SELECT id FROM {schema}.fits WHERE name = 'Fast Tackle'",
    )
    .await;
    assert_eq!(res.location(), format!("/plugins/{ID}/fit/{rifter}"));
    assert_eq!(ids_lookups(&h).await, 1);
    assert_eq!(
        one(&h, "SELECT count(*) FROM {schema}.fit_items").await,
        7,
        "every line naming an item"
    );

    // Until its job runs, the page says details are coming, and reloads.
    let fit = open(&h, &owner, &format!("fit/{rifter}")).await;
    assert_eq!(fit.status, StatusCode::OK, "{}", fit.body);
    assert!(fit.body.contains("still being looked up"), "{}", fit.body);
    work(&h).await;
    let fit = open(&h, &owner, &format!("fit/{rifter}")).await;
    assert!(!fit.body.contains("still being looked up"), "{}", fit.body);
    for text in [
        "Fast Tackle",
        "High slots",
        "Mid slots",
        "Low slots",
        "Rigs",
        "Drones",
        "Cargo",
        "Republic Fleet EMP S",
        "Offline",
        "Copy EFT",
        // AA's Copy Buy All: each item once, how many the fit holds.
        "Copy Buy All",
        "Rifter x1\n",
        "200mm AutoCannon II x2",
        "Warrior II x3",
        "Burn in",
        "Required skills",
        "Small Projectile Turret",
        "Minmatar Frigate",
        "images.evetech.net/types/587/icon",
        "images.evetech.net/types/2488/icon",
    ] {
        assert!(fit.body.contains(text), "{text}: {}", fit.body);
    }
    // Cargo needs no skills; the drones do.
    assert!(fit.body.contains(">Drones<"), "{}", fit.body);
    assert!(!fit.body.contains("Skill 3"), "{}", fit.body);

    // Mistakes come back line by line, with the text as pasted.
    let res = add_fit(
        &h,
        &owner,
        "[Rifter, Typo]\nDamage Kontrol II\n\nWarrior II x0\n",
        "&doctrine=",
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert!(res.body.contains("x0"), "{}", res.body);
    let res = add_fit(
        &h,
        &owner,
        "[Rifter, Typo]\nDamage Kontrol II\n",
        "&doctrine=",
    )
    .await;
    assert!(
        res.body.contains("isn&#39;t an item EVE knows"),
        "{}",
        res.body
    );
    assert!(res.body.contains("Damage Kontrol II"), "{}", res.body);
    let res = add_fit(&h, &owner, "Damage Control II", "&doctrine=").await;
    assert!(res.body.contains("header"), "{}", res.body);
    // AA's one fit per hull and name.
    let res = add_fit(&h, &owner, RIFTER, "&doctrine=").await;
    assert!(res.body.contains("already a Rifter fit"), "{}", res.body);
    // Names seen before aren't looked up again.
    let before = ids_lookups(&h).await;
    let res = add_fit(
        &h,
        &owner,
        "[Rifter, Cheap]\nDamage Control II\n",
        "&doctrine=",
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert_eq!(ids_lookups(&h).await, before);
    let cheap = one(&h, "SELECT id FROM {schema}.fits WHERE name = 'Cheap'").await;

    // A doctrine, then its fits.
    let res = post(
        &h,
        &owner,
        "add-doctrine",
        "_form=doctrine&name=Frigate+Gang&description=Fast+frigates&icon=",
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let doctrine = one(&h, "SELECT id FROM {schema}.doctrines").await;
    assert_eq!(
        res.location(),
        format!("/plugins/{ID}/edit/doctrine/{doctrine}")
    );
    let res = post(
        &h,
        &owner,
        &format!("edit/doctrine/{doctrine}"),
        &format!("_form=add_fit&fit={rifter}"),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    // A Slasher fit, added straight into the doctrine.
    let res = add_fit(
        &h,
        &owner,
        "[Slasher, Scout]\nDamage Control II\n",
        &format!("&doctrine={doctrine}"),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    work(&h).await;
    let list = open(&h, &owner, "").await;
    assert!(list.body.contains("Frigate Gang"), "{}", list.body);
    assert!(list.body.contains("Fast frigates"), "{}", list.body);
    // The icon is the main hull: the first added of two.
    assert!(
        list.body.contains("images.evetech.net/types/587/icon"),
        "{}",
        list.body
    );
    let page = open(&h, &owner, &format!("doctrine/{doctrine}")).await;
    assert!(page.body.contains("Fast Tackle"), "{}", page.body);
    assert!(page.body.contains("Scout"), "{}", page.body);
    assert!(page.body.contains("Edit Doctrine"), "{}", page.body);

    // Members with access_fittings see; they don't manage.
    let pilot = log_in_as(&h, PILOT_A, None).await;
    assert_eq!(open(&h, &pilot, "").await.status, StatusCode::NOT_FOUND);
    grant(&h, &owner, "access_fittings", MEMBER_STATE).await;
    let list = open(&h, &pilot, "").await;
    assert_eq!(list.status, StatusCode::OK, "{}", list.body);
    assert!(list.body.contains("Frigate Gang"), "{}", list.body);
    assert!(!list.body.contains("Add Fit"), "{}", list.body);
    assert_eq!(
        open(&h, &pilot, "add-fit").await.status,
        StatusCode::NOT_FOUND
    );
    let res = add_fit(&h, &pilot, "[Rifter, Mine]\nDamage Control II\n", "").await;
    assert!(res.status.is_client_error(), "{}", res.body);
    let fits = open(&h, &pilot, "fits").await;
    assert!(fits.body.contains("Cheap"), "{}", fits.body);
    assert!(!fits.body.contains("delete_fit"), "{}", fits.body);

    // AA's categories: one limited to a group hides its doctrine and that
    // doctrine's fits from everyone else; a fit in no category stays.
    let officers = tether_db::groups::create(&h.db, "Officers", "", Flags::default())
        .await
        .unwrap();
    let res = post(
        &h,
        &owner,
        "add-category",
        "_form=category&name=Leadership&color=%23FF0000",
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let category = one(&h, "SELECT id FROM {schema}.categories").await;
    let edit = open(&h, &owner, &format!("edit/category/{category}")).await;
    assert!(edit.body.contains("Officers"), "{}", edit.body);
    for body in [
        format!("_form=add_group&group={}", officers.0),
        format!("_form=add_doctrine&doctrine={doctrine}"),
    ] {
        let res = post(&h, &owner, &format!("edit/category/{category}"), &body).await;
        assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    }
    let list = open(&h, &pilot, "").await;
    assert!(!list.body.contains("Frigate Gang"), "{}", list.body);
    // Shared with Fleet Pings and FAT as seen here: for the category's
    // group, and managers.
    type Shared = (String, String, Option<Vec<i64>>, Option<String>);
    let shared: Vec<Shared> = sqlx::query_as(
        "SELECT name, link, groups, see_all FROM core.shared_doctrines WHERE plugin_id = $1",
    )
    .bind(ID)
    .fetch_all(&h.db)
    .await
    .unwrap();
    assert_eq!(
        shared,
        vec![(
            "Frigate Gang".to_owned(),
            format!("doctrine/{doctrine}"),
            Some(vec![officers.0]),
            Some("manage".to_owned())
        )]
    );
    for hidden in [
        format!("doctrine/{doctrine}"),
        format!("fit/{rifter}"),
        format!("category/{category}"),
    ] {
        assert_eq!(
            open(&h, &pilot, &hidden).await.status,
            StatusCode::NOT_FOUND,
            "{hidden}"
        );
    }
    let fits = open(&h, &pilot, "fits").await;
    assert!(fits.body.contains("Cheap"), "{}", fits.body);
    assert!(!fits.body.contains("Fast Tackle"), "{}", fits.body);
    // Managers see everything.
    let fits = open(&h, &owner, "fits").await;
    assert!(fits.body.contains("Fast Tackle"), "{}", fits.body);
    assert!(fits.body.contains("Leadership"), "{}", fits.body);

    // A member of the group sees it all.
    let pilot_id =
        tether_db::accounts::AccountId(me(&h, &pilot).await["account_id"].as_i64().unwrap());
    tether_db::groups::add_member(&h.db, officers, pilot_id)
        .await
        .unwrap();
    let list = open(&h, &pilot, "").await;
    assert!(list.body.contains("Frigate Gang"), "{}", list.body);
    assert_eq!(
        open(&h, &pilot, &format!("fit/{rifter}")).await.status,
        StatusCode::OK
    );
    let categories = open(&h, &pilot, "categories").await;
    assert!(
        categories.body.contains("Leadership"),
        "{}",
        categories.body
    );

    // A public category besides makes the doctrine public again (AA: any
    // category the viewer sees).
    tether_db::groups::remove_member(&h.db, officers, pilot_id)
        .await
        .unwrap();
    let res = post(
        &h,
        &owner,
        "add-category",
        "_form=category&name=Frigates&color=",
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let public = one(
        &h,
        "SELECT id FROM {schema}.categories WHERE name = 'Frigates'",
    )
    .await;
    let res = post(
        &h,
        &owner,
        &format!("edit/category/{public}"),
        &format!("_form=add_doctrine&doctrine={doctrine}"),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let list = open(&h, &pilot, "").await;
    assert!(list.body.contains("Frigate Gang"), "{}", list.body);

    // Editing a fit: the EFT again, same checks; its name from the header.
    let res = post(
        &h,
        &owner,
        &format!("edit/fit/{cheap}"),
        &format!(
            "_form=fit&eft={}&role=&description=",
            urlencode("[Rifter, Cheaper]\nDamage Control II\n\nWarp Scrambler II\n")
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert_eq!(
        one(
            &h,
            "SELECT count(*) FROM {schema}.fits WHERE name = 'Cheaper'"
        )
        .await,
        1
    );

    // Deleting asks first, from the row; then it's gone.
    let fits = open(&h, &owner, "fits").await;
    assert!(fits.body.contains("is deleted"), "{}", fits.body);
    let res = post(&h, &owner, "fits", &format!("_form=delete_fit&fit={cheap}")).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert_eq!(
        open(&h, &owner, &format!("fit/{cheap}")).await.status,
        StatusCode::NOT_FOUND
    );
    let res = post(
        &h,
        &owner,
        &format!("edit/doctrine/{doctrine}"),
        "_form=delete_doctrine",
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let res = post(
        &h,
        &owner,
        "categories",
        &format!("_form=delete_category&category={category}"),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    // Its fits stay.
    assert_eq!(
        open(&h, &pilot, &format!("fit/{rifter}")).await.status,
        StatusCode::OK
    );
}

// ---- the pilot's side: can I fly it, Save to EVE -----------------------------------

const CHRIBBA: i64 = 196379789;

/// Registers `character` for Fittings through the SSO round trip; the new
/// session (logins rotate it).
async fn register(h: &Harness, token: &str, character: &str) -> String {
    let res = send(
        &h.app,
        form(&format!("/register/start?app={ID}"), "", token),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let login = res.cookie_value(LOGIN);
    let state = query_param(res.location(), "state").to_owned();
    let res = send(
        &h.app,
        get(
            &format!(
                "/auth/callback?code=ok:{}&state={state}",
                character.replace(' ', "%20")
            ),
            &[(LOGIN, &login), (SESSION, token)],
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    res.cookie_value(SESSION)
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn pilots_see_whether_they_can_fly_a_fit_and_save_it_to_eve(db: PgPool) {
    cover(&db, Builtin::Member, EntityKind::Corporation, NPC_CORP).await;
    let h = harness_with_esi(db, true, esi_server().await).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    install(&h, &owner).await;
    add_fit(&h, &owner, RIFTER, "&doctrine=").await;
    let rifter = one(
        &h,
        "SELECT id FROM {schema}.fits WHERE name = 'Fast Tackle'",
    )
    .await;
    work(&h).await;

    // Nothing registered: the Register Character card.
    let fit = open(&h, &owner, &format!("fit/{rifter}")).await.body;
    assert!(fit.contains(&format!("/register?app={ID}")), "{fit}");
    assert!(fit.contains("Register a character with Fittings"), "{fit}");

    // Frigate I, Weapon Upgrades III, Propulsion Jamming II (IV needed),
    // Drones V; no Small Projectile Turret (V needed).
    Mock::given(method("GET"))
        .and(path(format!("/characters/{CHRIBBA}/skills")))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "skills": [
                {"skill_id": 3329, "active_skill_level": 1, "trained_skill_level": 1, "skillpoints_in_skill": 250},
                {"skill_id": 3318, "active_skill_level": 3, "trained_skill_level": 3, "skillpoints_in_skill": 8000},
                {"skill_id": 3435, "active_skill_level": 2, "trained_skill_level": 4, "skillpoints_in_skill": 1400},
                {"skill_id": 3436, "active_skill_level": 5, "trained_skill_level": 5, "skillpoints_in_skill": 256000}
            ],
            "total_sp": 265650
        })))
        .mount(&h.esi_server)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("/characters/{CHRIBBA}/fittings")))
        .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
            "fitting_id": 7
        })))
        .expect(1)
        .mount(&h.esi_server)
        .await;
    // Registering runs the app's schedules (in the background, and not
    // again within ten minutes of the runs installing it queued).
    sqlx::query("UPDATE core.schedules SET last_enqueued_at = now() - interval '1 hour'")
        .execute(&h.db)
        .await
        .unwrap();
    let owner = register(&h, &owner, "196379789:Chribba").await;
    for _ in 0..100 {
        let queued: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM core.audit_log WHERE action = 'schedule.run_now'",
        )
        .fetch_one(&h.db)
        .await
        .unwrap();
        if queued > 0 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    // Its skills are read.
    work(&h).await;

    let fit = open(&h, &owner, &format!("fit/{rifter}")).await.body;
    for text in [
        "Can I fly it",
        ">No<",
        "Propulsion Jamming IV (has II)",
        "Small Projectile Turret V (untrained)",
        "Save to EVE",
        "Read skills again",
    ] {
        assert!(fit.contains(text), "{text}: {fit}");
    }
    // Trained IV, but only II active (an Alpha clone): II counts.
    assert!(!fit.contains("Weapon Upgrades III"), "{fit}");

    // Save to EVE: the fit as ESI's fitting, to Chribba's own character.
    let saved = post(
        &h,
        &owner,
        &format!("fit/{rifter}"),
        &format!("_form=save_to_eve&character={CHRIBBA}"),
    )
    .await;
    assert_eq!(saved.status, StatusCode::OK, "{}", saved.body);
    assert!(saved.body.contains("s fittings in EVE."), "{}", saved.body);
    let sent: Vec<serde_json::Value> = h
        .esi_server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.method.as_str() == "POST" && r.url.path().ends_with("/fittings"))
        .map(|r| serde_json::from_slice(&r.body).unwrap())
        .collect();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0]["name"], "Fast Tackle");
    assert_eq!(sent[0]["ship_type_id"], 587);
    assert_eq!(
        sent[0]["items"],
        serde_json::json!([
            {"flag": "LoSlot0", "quantity": 1, "type_id": 2048},
            {"flag": "MedSlot0", "quantity": 1, "type_id": 3244},
            {"flag": "HiSlot0", "quantity": 1, "type_id": 2881},
            {"flag": "HiSlot1", "quantity": 1, "type_id": 2881},
            {"flag": "RigSlot0", "quantity": 1, "type_id": 31668},
            {"flag": "DroneBay", "quantity": 3, "type_id": 2488},
            {"flag": "Cargo", "quantity": 50, "type_id": 28668},
        ])
    );

    // Reading again, now.
    let again = post(
        &h,
        &owner,
        &format!("fit/{rifter}"),
        &format!("_form=read_skills&character={CHRIBBA}"),
    )
    .await;
    assert_eq!(again.status, StatusCode::OK, "{}", again.body);
    assert!(again.body.contains("s skills."), "{}", again.body);

    // Another pilot never gets Chribba's buttons, nor sees his skills.
    grant(&h, &owner, "access_fittings", MEMBER_STATE).await;
    let pilot = log_in_as(&h, PILOT_A, None).await;
    let theirs = open(&h, &pilot, &format!("fit/{rifter}")).await.body;
    assert!(
        !theirs.contains("Propulsion Jamming IV (has II)"),
        "{theirs}"
    );
    let refused = post(
        &h,
        &pilot,
        &format!("fit/{rifter}"),
        &format!("_form=save_to_eve&character={CHRIBBA}"),
    )
    .await;
    assert_ne!(refused.status, StatusCode::OK, "{}", refused.body);
}
