#![allow(clippy::unwrap_used, clippy::expect_used)] // test code

use std::time::Duration;

use sqlx::PgPool;
use tether_jobs::schedule::{ScheduleSpec, Scheduler, ensure, prune_succeeded, run_due};

fn spec(name: &str, every_secs: u64) -> ScheduleSpec {
    ScheduleSpec::new(name, "test.kind", Duration::from_secs(every_secs))
}

async fn next_run_in(pool: &PgPool, name: &str) -> f64 {
    sqlx::query_scalar("SELECT EXTRACT(EPOCH FROM next_run_at - now())::float8 FROM core.schedules WHERE name = $1")
        .bind(name)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn jobs_for(pool: &PgPool, name: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM core.jobs WHERE schedule = $1")
        .bind(name)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn finish_jobs(pool: &PgPool, name: &str) {
    sqlx::query(
        "UPDATE core.jobs SET state = 'succeeded', finished_at = now() WHERE schedule = $1",
    )
    .bind(name)
    .execute(pool)
    .await
    .unwrap();
}

async fn make_due(pool: &PgPool, name: &str) {
    sqlx::query("UPDATE core.schedules SET next_run_at = now() WHERE name = $1")
        .bind(name)
        .execute(pool)
        .await
        .unwrap();
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn new_schedules_run_now_and_then_every_interval(pool: PgPool) {
    ensure(&pool, &spec("sync", 600)).await.unwrap();

    assert_eq!(run_due(&pool).await.unwrap(), ["sync"]);
    assert_eq!(jobs_for(&pool, "sync").await, 1);
    let next = next_run_in(&pool, "sync").await;
    assert!((595.0..=600.0).contains(&next), "{next}");
    // Not due again yet.
    assert!(run_due(&pool).await.unwrap().is_empty());
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn ensure_updates_the_interval_but_keeps_the_clock(pool: PgPool) {
    ensure(&pool, &spec("sync", 600)).await.unwrap();
    run_due(&pool).await.unwrap();
    let before = next_run_in(&pool, "sync").await;

    ensure(&pool, &spec("sync", 60)).await.unwrap();

    let every: i32 =
        sqlx::query_scalar("SELECT every_secs FROM core.schedules WHERE name = 'sync'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(every, 60);
    assert!(
        (next_run_in(&pool, "sync").await - before).abs() < 2.0,
        "restart kept next_run_at"
    );
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn runs_never_pile_up(pool: PgPool) {
    ensure(&pool, &spec("slow", 1)).await.unwrap();
    run_due(&pool).await.unwrap();

    // Due again while the first run is still queued: skipped.
    make_due(&pool, "slow").await;
    assert!(run_due(&pool).await.unwrap().is_empty());
    assert_eq!(jobs_for(&pool, "slow").await, 1);

    // Once it's done, the next run is enqueued.
    finish_jobs(&pool, "slow").await;
    make_due(&pool, "slow").await;
    assert_eq!(run_due(&pool).await.unwrap(), ["slow"]);
    assert_eq!(jobs_for(&pool, "slow").await, 2);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn disabled_schedules_are_skipped(pool: PgPool) {
    ensure(&pool, &spec("off", 60)).await.unwrap();
    sqlx::query("UPDATE core.schedules SET enabled = false")
        .execute(&pool)
        .await
        .unwrap();
    assert!(run_due(&pool).await.unwrap().is_empty());
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn concurrent_schedulers_enqueue_once(pool: PgPool) {
    for n in 0..5 {
        ensure(&pool, &spec(&format!("s{n}"), 600)).await.unwrap();
    }
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..6 {
        let pool = pool.clone();
        tasks.spawn(async move { run_due(&pool).await.unwrap().len() });
    }
    let total: usize = tasks.join_all().await.into_iter().sum();

    assert_eq!(total, 5);
    let jobs: i64 = sqlx::query_scalar("SELECT count(*) FROM core.jobs")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(jobs, 5);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn the_scheduler_task_enqueues_and_stops(pool: PgPool) {
    ensure(&pool, &spec("tick", 3600)).await.unwrap();
    let scheduler = Scheduler::start(pool.clone(), Duration::from_millis(20));

    let mut enqueued = 0;
    for _ in 0..100 {
        enqueued = jobs_for(&pool, "tick").await;
        if enqueued > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(enqueued, 1);
    tokio::time::timeout(Duration::from_secs(2), scheduler.shutdown())
        .await
        .expect("stops promptly");
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn pruning_keeps_recent_and_dead_jobs(pool: PgPool) {
    sqlx::query(
        "INSERT INTO core.jobs (kind, state, finished_at) VALUES
            ('old', 'succeeded', now() - interval '8 days'),
            ('recent', 'succeeded', now() - interval '1 day'),
            ('dead', 'dead', now() - interval '30 days')",
    )
    .execute(&pool)
    .await
    .unwrap();

    let removed = prune_succeeded(&pool, Duration::from_secs(7 * 86400))
        .await
        .unwrap();

    assert_eq!(removed, 1);
    let left: Vec<String> = sqlx::query_scalar("SELECT kind FROM core.jobs ORDER BY kind")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(left, ["dead", "recent"]);
}
