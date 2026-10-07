//! Buyback's statistics (aa-buybackprogram's `views/stats.py`) end to
//! end: installed from its real component and migration, its tables
//! seeded as its contract sync leaves them, then each page as each viewer
//! may see it: a contract's details before and after its contract, My
//! statistics, Program and All statistics (untracked contracts, wallets,
//! Refresh contracts), a program's leaderboard and its performance with
//! the CSV. AA's bugs fixed: finished contracts stay after their expiry
//! (B5), performance ISK is quantity x unit value (B8), and programs a
//! viewer may not use stay hidden (B2).

use std::sync::OnceLock;

use crate::common::*;
use axum::http::StatusCode;
use sqlx::PgPool;
use tether_plugins::testing::{self, Key};
use wiremock::matchers::{method, path};
use wiremock::{Mock, ResponseTemplate};

const ID: &str = "tether.buyback";
const SCHEMA: &str = r#""plugin_tether.buyback""#;
const CHRIBBA: i64 = 196379789;
const MITTANI: i64 = 443630591;
const OUTSIDER: i64 = 1887431749;
const TRITANIUM: i64 = 34;
const PYERITE: i64 = 35;
const MEXALLON: i64 = 36;

fn component() -> Vec<u8> {
    static COMPONENT: OnceLock<Vec<u8>> = OnceLock::new();
    COMPONENT.get_or_init(|| build_guest("buyback")).clone()
}

fn plugin_file(name: &str) -> String {
    std::fs::read_to_string(format!(
        "{}/../../plugins/buyback/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

async fn install(h: &Harness, owner: &str) {
    let key = Key::new(21);
    let manifest = plugin_file("plugin.toml").replace("PUBLISHER_KEY", &key.public());
    let migration = plugin_file("migrations/0001_buyback.sql");
    let component = component();
    let bytes = testing::zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("plugin.wasm", &component),
        ("migrations/0001_buyback.sql", migration.as_bytes()),
    ]);
    let at = install_package(h, owner, &bytes, &key.sign(&bytes)).await;
    assert_eq!(at, format!("/admin/plugins/{ID}"));
}

async fn grant(h: &Harness, character: i64, permissions: &[&str]) -> i64 {
    let account: i64 = sqlx::query_scalar("SELECT account_id FROM core.characters WHERE id = $1")
        .bind(character)
        .fetch_one(&h.db)
        .await
        .unwrap();
    for p in permissions {
        sqlx::query("INSERT INTO core.permission_grants (permission, account_id) VALUES ($1, $2)")
            .bind(format!("plugin.{ID}.{p}"))
            .bind(account)
            .execute(&h.db)
            .await
            .unwrap();
    }
    account
}

async fn post(h: &Harness, token: &str, at: &str, body: &str) -> Res {
    send(&h.app, form(&format!("/plugins/{ID}/{at}"), body, token)).await
}

async fn open(h: &Harness, at: &str, token: &str) -> Res {
    page(h, &format!("/plugins/{ID}/{at}"), token).await
}

async fn sql(h: &Harness, statement: &str) {
    // Test seeds, written here (no outside input).
    sqlx::query(sqlx::AssertSqlSafe(
        statement.replace("bb.", &format!("{SCHEMA}.")),
    ))
    .execute(&h.db)
    .await
    .unwrap_or_else(|e| panic!("{statement}: {e}"));
}

/// A calculation, and its contract when it has one.
#[allow(clippy::too_many_arguments)]
async fn tracked(
    h: &Harness,
    number: &str,
    program: i64,
    seller: (i64, i64),
    contract: Option<(i64, &str, f64, &str, &str)>,
    donation: f64,
    items: &[(i64, i64, f64)],
    assignee: i64,
) {
    let (account, character) = seller;
    let net: f64 = items.iter().map(|(_, q, u)| *q as f64 * u).sum();
    if let Some((id, status, price, issued, expired)) = contract {
        sql(
            h,
            &format!(
                "INSERT INTO bb.contracts (contract_id, assignee_id, date_issued, date_expired, \
                     date_completed, issuer_corporation_id, issuer_id, start_location_id, \
                     location_name, price, status, title, volume, owner_character, items_read) \
                 VALUES ({id}, {assignee}, '{issued}', '{expired}', \
                     CASE WHEN '{status}' = 'finished' THEN '{issued}'::timestamptz + interval '2 hours' END, \
                     98000001, {character}, 60003760, 'Jita IV - Moon 4 - Caldari Navy Assembly Plant', \
                     {price}, '{status}', '{number}', 1000, {CHRIBBA}, true)"
            ),
        )
        .await;
    }
    sql(
        h,
        &format!(
            "INSERT INTO bb.trackings (program_id, contract_id, issuer_account, issuer_character, \
                 value, taxes, donation, net_price, tracking_number) \
             VALUES ({program}, {}, {account}, {character}, {}, {}, {donation}, {}, '{number}')",
            contract.map_or("NULL".to_owned(), |c| c.0.to_string()),
            net / 0.9,
            net / 0.9 - net,
            net - donation,
        ),
    )
    .await;
    for (t, q, u) in items {
        sql(
            h,
            &format!(
                "INSERT INTO bb.tracking_items (tracking_id, type_id, quantity, buy_value) \
                 SELECT id, {t}, {q}, {u} FROM bb.trackings WHERE tracking_number = '{number}'"
            ),
        )
        .await;
    }
}

async fn contract_items(h: &Harness, contract: i64, items: &[(i64, i64)]) {
    for (t, q) in items {
        sql(
            h,
            &format!(
                "INSERT INTO bb.contract_items (contract_id, type_id, quantity) VALUES ({contract}, {t}, {q})"
            ),
        )
        .await;
    }
}

async fn flag(h: &Harness, contract: i64, tone: &str, header: &str, message: &str) {
    sql(
        h,
        &format!(
            "INSERT INTO bb.contract_flags (contract_id, tone, header, message) \
             VALUES ({contract}, '{tone}', '{header}', '{message}')"
        ),
    )
    .await;
}

fn has(res: &Res, texts: &[&str]) {
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    for text in texts {
        assert!(res.body.contains(text), "{text}: {}", res.body);
    }
}

fn lacks(res: &Res, texts: &[&str]) {
    for text in texts {
        assert!(!res.body.contains(text), "{text}: {}", res.body);
    }
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn buyback_statistics(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    install(&h, &owner).await;
    Mock::given(method("POST"))
        .and(path("/universe/names"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            { "id": CHRIBBA, "name": "Chribba", "category": "character" },
            { "id": MITTANI, "name": "The Mittani", "category": "character" },
            { "id": OUTSIDER, "name": "Outsider", "category": "character" },
        ])))
        .with_priority(1)
        .mount(&h.esi_server)
        .await;
    let pilot = log_in_as(&h, "443630591:The Mittani", None).await;
    let outsider = log_in_as(&h, "1887431749:Outsider", None).await;
    let chribba = grant(&h, CHRIBBA, &[]).await;
    let mittani = grant(&h, MITTANI, &["basic_access"]).await;
    let other = grant(&h, OUTSIDER, &["basic_access"]).await;
    let corp: i64 = sqlx::query_scalar("SELECT corporation_id FROM core.characters WHERE id = $1")
        .bind(CHRIBBA)
        .fetch_one(&h.db)
        .await
        .unwrap();

    // Chribba manages two programs through his corporation: one open to
    // everyone with basic access, one to a group nobody's in.
    sql(
        &h,
        &format!(
            "INSERT INTO bb.programs (id, name, owner_character, owner_corporation, manager_account, \
                 is_corporation, tax, wallet_division) \
             VALUES (1, 'Ore buyback', {CHRIBBA}, {corp}, {chribba}, true, 10, 1)"
        ),
    )
    .await;
    sql(
        &h,
        &format!(
            "INSERT INTO bb.programs (id, name, owner_character, owner_corporation, manager_account, \
                 is_corporation, restricted_groups) \
             VALUES (2, 'Officers only', {CHRIBBA}, {corp}, {chribba}, true, '{{424242}}')"
        ),
    )
    .await;
    sql(
        &h,
        &format!(
            "INSERT INTO bb.wallets (corporation_id, division, name, balance) \
             VALUES ({corp}, 1, 'Buyback Wallet', 123456789)"
        ),
    )
    .await;

    // Finished in August 2025, its expiry long past (B5: it stays).
    tracked(
        &h,
        "aa-bbp-1-AAAAAA",
        1,
        (mittani, MITTANI),
        Some((
            1001,
            "finished",
            900_000.0,
            "2025-08-10T12:00:00Z",
            "2025-08-24T12:00:00Z",
        )),
        0.0,
        &[(TRITANIUM, 100_000, 5.0), (PYERITE, 10_000, 40.0)],
        corp,
    )
    .await;
    contract_items(&h, 1001, &[(TRITANIUM, 100_000), (PYERITE, 10_000)]).await;
    flag(
        &h,
        1001,
        "warning",
        "Title variation",
        "Contract description contains extra characters.",
    )
    .await;
    // The outsider sold more in August, with a donation.
    tracked(
        &h,
        "aa-bbp-5-EEEEEE",
        1,
        (other, OUTSIDER),
        Some((
            1005,
            "finished",
            2_000_000.0,
            "2025-08-20T12:00:00Z",
            "2025-09-03T12:00:00Z",
        )),
        100_000.0,
        &[(TRITANIUM, 420_000, 5.0)],
        corp,
    )
    .await;
    contract_items(&h, 1005, &[(TRITANIUM, 420_000)]).await;
    // September: Mittani alone.
    tracked(
        &h,
        "aa-bbp-6-FFFFFF",
        1,
        (mittani, MITTANI),
        Some((
            1006,
            "finished",
            500_000.0,
            "2025-09-05T12:00:00Z",
            "2025-09-19T12:00:00Z",
        )),
        0.0,
        &[(PYERITE, 12_500, 40.0)],
        corp,
    )
    .await;
    // Outstanding, with the wrong items: Pyerite missing, Mexallon added.
    let now = chrono::Utc::now();
    let rfc = |d: chrono::Duration| (now + d).to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    tracked(
        &h,
        "aa-bbp-2-BBBBBB",
        1,
        (mittani, MITTANI),
        Some((
            1002,
            "outstanding",
            2_000_000_000.0,
            &rfc(chrono::Duration::hours(-1)),
            &rfc(chrono::Duration::days(14)),
        )),
        0.0,
        &[(TRITANIUM, 100_000, 5.0), (PYERITE, 10_000, 40.0)],
        corp,
    )
    .await;
    contract_items(&h, 1002, &[(TRITANIUM, 100_000), (MEXALLON, 5)]).await;
    flag(
        &h,
        1002,
        "danger",
        "Item mismatch",
        "Tracked items do not match the actual items in the contract.",
    )
    .await;
    // Outstanding but expired: gone, as in AA.
    tracked(
        &h,
        "aa-bbp-4-DDDDDD",
        1,
        (mittani, MITTANI),
        Some((
            1004,
            "outstanding",
            1_000.0,
            &rfc(chrono::Duration::days(-30)),
            &rfc(chrono::Duration::days(-2)),
        )),
        0.0,
        &[(TRITANIUM, 200, 5.0)],
        corp,
    )
    .await;
    // No contract yet.
    tracked(
        &h,
        "aa-bbp-3-CCCCCC",
        1,
        (mittani, MITTANI),
        None,
        0.0,
        &[(TRITANIUM, 1_000, 5.0)],
        corp,
    )
    .await;
    // Mittani's sale to the officers' program (he's no officer now).
    tracked(
        &h,
        "aa-bbp-7-GGGGGG",
        2,
        (mittani, MITTANI),
        Some((
            1007,
            "finished",
            7_000.0,
            "2025-09-10T12:00:00Z",
            "2025-09-24T12:00:00Z",
        )),
        0.0,
        &[(MEXALLON, 100, 70.0)],
        corp,
    )
    .await;
    // A scam: a buyback prefix, no calculation.
    sql(
        &h,
        &format!(
            "INSERT INTO bb.contracts (contract_id, assignee_id, date_issued, date_expired, \
                 issuer_corporation_id, issuer_id, price, status, title, no_tracking, owner_character) \
             VALUES (1099, {corp}, '{}', '{}', 98000001, {OUTSIDER}, 5000000, 'outstanding', \
                 'aa-bbp please pay', true, {CHRIBBA})",
            rfc(chrono::Duration::hours(-2)),
            rfc(chrono::Duration::days(10)),
        ),
    )
    .await;
    flag(
        &h,
        1099,
        "danger",
        "Suspicious Contract",
        "Possibly a scam contract.",
    )
    .await;

    // ---- My statistics -------------------------------------------------------
    let mine = open(&h, "stats", &pilot).await;
    has(
        &mine,
        &[
            "My statistics",
            "Outstanding contracts",
            "aa-bbp-2-BBBBBB",
            "Item mismatch",
        ],
    );
    lacks(
        &mine,
        &["aa-bbp-4-DDDDDD", "aa-bbp-3-CCCCCC", "aa-bbp please pay"],
    );
    // Finished: the expired one stays (B5), across both programs.
    let finished = open(&h, "stats?_tab=1", &pilot).await;
    has(
        &finished,
        &[
            "aa-bbp-1-AAAAAA",
            "aa-bbp-6-FFFFFF",
            "aa-bbp-7-GGGGGG",
            "Title variation",
        ],
    );
    lacks(&finished, &["aa-bbp-5-EEEEEE"]);
    // Filtered by program.
    let officers = open(&h, "stats?program=2&_tab=1", &pilot).await;
    has(&officers, &["aa-bbp-7-GGGGGG"]);
    lacks(&officers, &["aa-bbp-1-AAAAAA"]);
    // The outsider made none of these.
    let theirs = open(&h, "stats", &outsider).await;
    lacks(&theirs, &["aa-bbp-2-BBBBBB"]);

    // ---- Program statistics and All statistics --------------------------------
    let programs = open(&h, "program-stats", &owner).await;
    has(
        &programs,
        &[
            "Program statistics",
            "Untracked contracts",
            "You have 1 outstanding contracts that start with the buyback prefill text",
            "Corporate funding wallets",
            "Buyback Wallet",
            "Ore buyback",
            "Refresh contracts",
            "aa-bbp-2-BBBBBB",
        ],
    );
    let scams = open(&h, "program-stats?_tab=2", &owner).await;
    has(&scams, &["aa-bbp please pay", "Suspicious Contract"]);
    // Not for sellers.
    assert_eq!(
        open(&h, "program-stats", &pilot).await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        open(&h, "all-stats", &pilot).await.status,
        StatusCode::NOT_FOUND
    );
    let all = open(&h, "all-stats?_tab=1", &owner).await;
    has(
        &all,
        &[
            "All statistics",
            "aa-bbp-5-EEEEEE",
            "aa-bbp-7-GGGGGG",
            "Buyback Wallet",
        ],
    );
    // Refresh contracts queues the contract read.
    let res = post(&h, &owner, "program-stats", "_form=refresh_contracts").await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let queued: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM core.jobs WHERE plugin_id = $1 AND job_key = 'contracts' \
         AND state = 'queued'",
    )
    .bind(ID)
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert_eq!(queued, 1);
    assert_eq!(
        post(&h, &pilot, "program-stats", "_form=refresh_contracts")
            .await
            .status,
        StatusCode::NOT_FOUND
    );

    // ---- a contract's details -------------------------------------------------
    // Before its contract: the calculation and how to make it.
    let waiting = open(&h, "tracking/aa-bbp-3-CCCCCC", &pilot).await;
    has(
        &waiting,
        &[
            "Tracking aa-bbp-3-CCCCCC",
            "How to create the contract",
            "Invoice",
            "Calculated items",
            "Tritanium",
        ],
    );
    // With its contract: its information, flags and items against the
    // calculation.
    let details = open(&h, "tracking/aa-bbp-2-BBBBBB", &pilot).await;
    has(
        &details,
        &[
            "Contract information",
            "Jita IV - Moon 4 - Caldari Navy Assembly Plant",
            "Item mismatch",
            "Tracked items do not match the actual items in the contract.",
            "Original calculation",
            "Contract items",
            "Pyerite is missing from the created contract",
        ],
    );
    lacks(&details, &["Tritanium is missing"]);
    let items = open(&h, "tracking/aa-bbp-2-BBBBBB?_tab=1", &pilot).await;
    has(
        &items,
        &["Mexallon is missing from the original calculation"],
    );
    // Anyone with basic access who may use the program, unless the
    // details are restricted.
    has(
        &open(&h, "tracking/aa-bbp-2-BBBBBB", &outsider).await,
        &["Contract information"],
    );
    // Not for a program they may not use.
    assert_eq!(
        open(&h, "tracking/aa-bbp-7-GGGGGG", &outsider).await.status,
        StatusCode::NOT_FOUND
    );
    // The seller always may.
    has(
        &open(&h, "tracking/aa-bbp-7-GGGGGG", &pilot).await,
        &["Contract information"],
    );
    sql(
        &h,
        "UPDATE bb.settings SET restrict_tracking_details = true",
    )
    .await;
    assert_eq!(
        open(&h, "tracking/aa-bbp-2-BBBBBB", &outsider).await.status,
        StatusCode::NOT_FOUND
    );
    has(
        &open(&h, "tracking/aa-bbp-2-BBBBBB", &pilot).await,
        &["Contract information"],
    );
    has(
        &open(&h, "tracking/aa-bbp-2-BBBBBB", &owner).await,
        &["Contract information"],
    );
    assert_eq!(
        open(&h, "tracking/aa-bbp-0-NOPE00", &owner).await.status,
        StatusCode::NOT_FOUND
    );

    // ---- leaderboard ------------------------------------------------------------
    // The latest month first, with the one before.
    let board = open(&h, "program/1/leaderboard", &pilot).await;
    has(
        &board,
        &[
            "Ore buyback leaderboard",
            "September 2025",
            "August 2025",
            "The Mittani",
            "1st",
        ],
    );
    lacks(&board, &["Outsider"]);
    let august = open(&h, "program/1/leaderboard?month=2025-08", &owner).await;
    has(
        &august,
        &[
            "Outsider",
            "The Mittani",
            "1st",
            "2nd",
            "Total",
            "September 2025",
        ],
    );
    let first = august.body.find("Outsider").unwrap();
    let second = august.body.find("The Mittani").unwrap();
    assert!(first < second, "{}", august.body);
    // Not for a program the viewer may not use (B2).
    assert_eq!(
        open(&h, "program/2/leaderboard", &pilot).await.status,
        StatusCode::NOT_FOUND
    );
    has(
        &open(&h, "program/2/leaderboard", &owner).await,
        &["Officers only leaderboard"],
    );

    // ---- performance --------------------------------------------------------------
    // Not for sellers.
    assert_eq!(
        open(&h, "program/1/performance", &pilot).await.status,
        StatusCode::NOT_FOUND
    );
    let performance = open(&h, "program/1/performance", &owner).await;
    // August's 2.9M and September's 0.5M: shown in millions.
    has(
        &performance,
        &[
            "Ore buyback performance",
            "By month",
            "August 2025",
            "September 2025",
            "Bought (Millions of ISK)",
            "2.900",
            "By item group",
            "Mineral",
            "Build CSV",
        ],
    );
    // A group's items (B8: quantity x unit value): Tritanium 520,000 x 5,
    // its months averaging under a million, so in ISK.
    let minerals = open(&h, "program/1/performance?group=18", &owner).await;
    has(
        &minerals,
        &[
            "Mineral by month",
            "Mineral items",
            "Tritanium",
            "Pyerite",
            "2 600 000.00",
        ],
    );

    // The CSV, for see_performance.
    let res = post(&h, &owner, "program/1/performance", "_form=export").await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let csv = open(&h, "downloads/performance-1", &owner).await;
    assert_eq!(csv.status, StatusCode::OK, "{}", csv.body);
    assert!(
        csv.body.starts_with(
            "Contract ID,Date Issued,Date Finished,User ID,Total ISK,Object Category,Object ID,\
             Object Name,Object Quant,Object ISK\r\n"
        ),
        "{}",
        csv.body
    );
    assert!(
        csv.body.contains(&format!(
            "1001,2025-08-10T12:00:00Z,2025-08-10T14:00:00Z,{MITTANI},900000.00,Mineral,34,Tritanium,100000,500000.00\r\n"
        )),
        "{}",
        csv.body
    );
    assert!(!csv.body.contains("1002,"), "{}", csv.body);
    let listed = open(&h, "program/1/performance", &owner).await;
    has(&listed, &["buyback.csv"]);
    assert_eq!(
        open(&h, "downloads/performance-1", &pilot).await.status,
        StatusCode::NOT_FOUND
    );
}
