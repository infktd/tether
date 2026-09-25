#![allow(clippy::unwrap_used, clippy::expect_used)] // test code

mod common;

use common::*;
use sqlx::PgPool;

fn sell(h: &Harness, character_id: i64) {
    h.sso
        .owner_hashes
        .lock()
        .unwrap()
        .insert(character_id, "owner-after-sale".into());
}

async fn transfer_audits(h: &Harness) -> Vec<serde_json::Value> {
    sqlx::query_scalar(
        "SELECT details FROM core.audit_log WHERE action = 'character.ownership_lost'",
    )
    .fetch_all(&h.db)
    .await
    .unwrap()
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_sold_alt_moves_to_the_buyer(db: PgPool) {
    let h = harness(db, true).await;
    let seller = log_in_as(&h, "90000001:Seller", None).await;
    let seller = log_in_as(&h, "90000002:Sold Alt", Some(&seller)).await;
    let buyer = log_in_as(&h, "90000003:Buyer", None).await;

    sell(&h, 90000002);
    let buyer = log_in_as(&h, "90000002:Sold Alt", Some(&buyer)).await;

    let buyer_chars = me(&h, &buyer).await["characters"].as_array().unwrap().len();
    assert_eq!(buyer_chars, 2);
    let seller_me = me(&h, &seller).await;
    assert_eq!(seller_me["characters"].as_array().unwrap().len(), 1);
    assert_eq!(seller_me["main"]["id"], 90000001);
    let audits = transfer_audits(&h).await;
    assert_eq!(audits.len(), 1);
    assert_eq!(audits[0]["was_main"], false);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn buying_the_owners_only_character_does_not_buy_ownership(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    assert_eq!(me(&h, &owner).await["is_owner"], true);

    sell(&h, 196379789);
    let buyer = log_in_as(&h, "196379789:Chribba", None).await;

    assert_eq!(me(&h, &buyer).await["is_owner"], false);
    // The old owner account has no characters and is owner no more...
    let old = me(&h, &owner).await;
    assert_eq!(old["is_owner"], false);
    assert!(old["main"].is_null());
    // ...so setup reopens for whoever holds the setup token.
    let setup = send(&h.app, get("/api/setup", &[])).await;
    assert!(
        setup.body.contains(r#""state":"needs_owner""#),
        "{}",
        setup.body
    );
    let audits = transfer_audits(&h).await;
    assert_eq!(audits[0]["owner_lost"], true);
    assert_eq!(audits[0]["reason"], "sold");
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn an_unchanged_owner_hash_signs_in_as_before(db: PgPool) {
    let h = harness(db, true).await;
    let first = log_in_as(&h, "90000001:Pilot", None).await;
    let again = log_in_as(&h, "90000001:Pilot", None).await;
    assert_eq!(
        me(&h, &first).await["account_id"],
        me(&h, &again).await["account_id"]
    );
    assert!(transfer_audits(&h).await.is_empty());
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn the_vault_keeps_the_verified_scopes(db: PgPool) {
    let h = harness(db, true).await;
    *h.sso.granted_scopes.lock().unwrap() = vec!["esi-skills.read_skills.v1".into()];
    log_in_as(&h, "90000001:Pilot", None).await;

    assert!(
        h.vault
            .access_token(90000001, &["esi-skills.read_skills.v1"])
            .await
            .is_ok()
    );
    assert!(
        h.vault
            .access_token(90000001, &["esi-wallet.read_character_wallet.v1"])
            .await
            .is_err()
    );
}
