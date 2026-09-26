use std::sync::atomic::Ordering;
use std::time::Duration;

use crate::common::*;
use sqlx::PgPool;
use tether_esi::vault::VaultError;

const PILOT: i64 = 90000001;

async fn stored_refresh(h: &Harness, character_id: i64) -> (Vec<u8>, String) {
    let (sealed, state): (Vec<u8>, String) = sqlx::query_as(
        "SELECT refresh_token, state FROM core.character_tokens WHERE character_id = $1",
    )
    .bind(character_id)
    .fetch_one(&h.db)
    .await
    .unwrap();
    (sealed, state)
}

fn decrypt(sealed: &[u8], character_id: i64) -> String {
    test_key()
        .open(sealed, &format!("token:{character_id}"))
        .unwrap()
        .expose()
        .clone()
}

/// Logs in with access tokens that are already due for refresh.
async fn log_in_expired(h: &Harness) -> String {
    *h.sso.token_ttl.lock().unwrap() = Duration::ZERO;
    let token = log_in_as(h, "90000001:Pilot", None).await;
    *h.sso.token_ttl.lock().unwrap() = Duration::from_secs(20 * 60);
    token
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn login_stores_the_refresh_token_encrypted(db: PgPool) {
    let h = harness(db, true).await;
    log_in_as(&h, "90000001:Pilot", None).await;

    let (sealed, state) = stored_refresh(&h, PILOT).await;
    assert_eq!(state, "valid");
    assert!(
        !sealed.windows(7).any(|w| w == b"refresh"),
        "no plaintext at rest"
    );
    assert_eq!(decrypt(&sealed, PILOT), "refresh-90000001-1");
    // Bound to its row: it won't decrypt as another character's token.
    assert!(test_key().open(&sealed, "token:90000002").is_err());
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn fresh_access_tokens_come_from_memory(db: PgPool) {
    let h = harness(db, true).await;
    log_in_as(&h, "90000001:Pilot", None).await;

    let token = h.vault.access_token(PILOT, &[]).await.unwrap();

    assert_eq!(token.expose(), "access-90000001-login");
    assert_eq!(h.sso.refresh_calls.load(Ordering::SeqCst), 0);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn expiring_tokens_refresh_once_and_keep_the_rotated_refresh_token(db: PgPool) {
    let h = harness(db, true).await;
    log_in_expired(&h).await;

    let token = h.vault.access_token(PILOT, &[]).await.unwrap();
    assert_eq!(token.expose(), "access-refreshed-1");
    assert_eq!(
        *h.sso.refresh_tokens_seen.lock().unwrap(),
        ["refresh-90000001-1"]
    );
    let (sealed, _) = stored_refresh(&h, PILOT).await;
    assert_eq!(decrypt(&sealed, PILOT), "refresh-90000001-2");

    // Now cached.
    h.vault.access_token(PILOT, &[]).await.unwrap();
    assert_eq!(h.sso.refresh_calls.load(Ordering::SeqCst), 1);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_refresh_without_rotation_keeps_the_old_refresh_token(db: PgPool) {
    let h = harness(db, true).await;
    log_in_expired(&h).await;
    *h.sso.refresh_outcome.lock().unwrap() = RefreshOutcome::Keep;

    h.vault.access_token(PILOT, &[]).await.unwrap();

    let (sealed, _) = stored_refresh(&h, PILOT).await;
    assert_eq!(decrypt(&sealed, PILOT), "refresh-90000001-1");
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn concurrent_requests_share_one_refresh(db: PgPool) {
    let h = harness(db, true).await;
    log_in_expired(&h).await;

    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..8 {
        let vault = h.vault.clone();
        tasks.spawn(async move {
            vault
                .access_token(PILOT, &[])
                .await
                .unwrap()
                .expose()
                .clone()
        });
    }
    let tokens = tasks.join_all().await;

    assert_eq!(h.sso.refresh_calls.load(Ordering::SeqCst), 1);
    assert!(
        tokens.iter().all(|t| t == "access-refreshed-1"),
        "{tokens:?}"
    );
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn revoked_tokens_are_marked_audited_shown_and_fixed_by_logging_in(db: PgPool) {
    let h = harness(db, true).await;
    let session = log_in_expired(&h).await;
    *h.sso.refresh_outcome.lock().unwrap() = RefreshOutcome::Revoked;

    let err = h.vault.access_token(PILOT, &[]).await.unwrap_err();
    assert!(matches!(err, VaultError::Revoked), "{err:?}");
    assert_eq!(stored_refresh(&h, PILOT).await.1, "revoked");
    // Known dead: no further calls to SSO.
    assert!(matches!(
        h.vault.access_token(PILOT, &[]).await,
        Err(VaultError::Revoked)
    ));
    assert_eq!(h.sso.refresh_calls.load(Ordering::SeqCst), 1);

    let audited: Vec<(String, Option<String>)> =
        sqlx::query_as("SELECT action, target FROM core.audit_log WHERE action = 'token.revoked'")
            .fetch_all(&h.db)
            .await
            .unwrap();
    assert_eq!(
        audited,
        [("token.revoked".into(), Some("character:90000001".into()))]
    );

    let profile = send(&h.app, get("/dashboard", &[(SESSION, &session)])).await;
    assert!(
        profile.body.contains("register this character again"),
        "{}",
        profile.body
    );

    // Logging in again with the character stores a new token.
    *h.sso.refresh_outcome.lock().unwrap() = RefreshOutcome::Rotate;
    log_in_as(&h, "90000001:Pilot", Some(&session)).await;
    assert_eq!(stored_refresh(&h, PILOT).await.1, "valid");
    assert!(h.vault.access_token(PILOT, &[]).await.is_ok());
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn sso_outages_are_not_revocations(db: PgPool) {
    let h = harness(db, true).await;
    log_in_expired(&h).await;
    *h.sso.refresh_outcome.lock().unwrap() = RefreshOutcome::Unavailable;

    let err = h.vault.access_token(PILOT, &[]).await.unwrap_err();

    assert!(matches!(err, VaultError::Unavailable(_)), "{err:?}");
    assert_eq!(stored_refresh(&h, PILOT).await.1, "valid");
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn scopes_are_checked_and_unknown_characters_have_no_token(db: PgPool) {
    let h = harness(db, true).await;
    log_in_as(&h, "90000001:Pilot", None).await;

    let err = h
        .vault
        .access_token(PILOT, &["esi-wallet.read_character_wallet.v1"])
        .await
        .unwrap_err();
    assert!(
        matches!(&err, VaultError::MissingScopes(s) if s == &["esi-wallet.read_character_wallet.v1"]),
        "{err:?}"
    );
    assert!(matches!(
        h.vault.access_token(1, &[]).await,
        Err(VaultError::NoToken)
    ));
}
