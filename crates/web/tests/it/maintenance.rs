use crate::common::*;
use sqlx::PgPool;

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn prune_removes_only_expired_rows(db: PgPool) {
    let h = harness(db, true).await;
    let live = log_in_as(&h, "90000001:Live", None).await;
    let stale = log_in_as(&h, "90000002:Stale", None).await;
    sqlx::query(
        "UPDATE core.sessions SET expires_at = now() - interval '1 second' WHERE token_hash = $1",
    )
    .bind(tether_core::hash_token(&stale))
    .execute(&h.db)
    .await
    .unwrap();
    start_login(&h, "/").await; // a pending attempt that's still valid
    sqlx::query(
        "INSERT INTO core.login_attempts (state, browser_hash, pkce_verifier, return_to, expires_at)
         VALUES ('old', '\\x00', 'v', '/', now() - interval '1 hour')",
    )
    .execute(&h.db)
    .await
    .unwrap();

    tether_web::maintenance::prune(&h.db).await.unwrap();

    let sessions: i64 = sqlx::query_scalar("SELECT count(*) FROM core.sessions")
        .fetch_one(&h.db)
        .await
        .unwrap();
    let attempts: i64 = sqlx::query_scalar("SELECT count(*) FROM core.login_attempts")
        .fetch_one(&h.db)
        .await
        .unwrap();
    assert_eq!((sessions, attempts), (1, 1));
    assert_eq!(me(&h, &live).await["main"]["name"], "Live");
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn the_prune_schedule_is_declared_hourly(db: PgPool) {
    for spec in tether_web::maintenance::schedules() {
        tether_jobs::schedule::ensure(&db, &spec).await.unwrap();
    }
    let every: i32 = sqlx::query_scalar(
        "SELECT every_secs FROM core.schedules WHERE kind = 'maintenance.prune'",
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(every, 3600);
}
