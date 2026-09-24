#![allow(clippy::unwrap_used, clippy::expect_used)] // test code
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use serde_json::json;
use sqlx::{PgPool, Row};
use tether_jobs::{
    JobError, JobId, NewJob, Outcome, Registry, WorkerConfig, WorkerPool, enqueue, run_once,
};

fn fast_config() -> WorkerConfig {
    WorkerConfig {
        workers: 1,
        poll_interval: Duration::from_millis(20),
        lease: Duration::from_secs(5),
        backoff_base: Duration::from_secs(60),
        backoff_max: Duration::from_secs(3600),
    }
}

struct Row_ {
    state: String,
    attempts: i32,
    last_error: Option<String>,
    retry_in_s: f64,
}

async fn row(pool: &PgPool, id: JobId) -> Row_ {
    let r = sqlx::query(
        "SELECT state, attempts, last_error,
                EXTRACT(EPOCH FROM run_at - now())::float8 AS retry_in_s
         FROM core.jobs WHERE id = $1",
    )
    .bind(id.0)
    .fetch_one(pool)
    .await
    .unwrap();
    Row_ {
        state: r.get("state"),
        attempts: r.get("attempts"),
        last_error: r.get("last_error"),
        retry_in_s: r.get("retry_in_s"),
    }
}

/// Makes a queued job runnable now, skipping its backoff.
async fn make_due(pool: &PgPool, id: JobId) {
    sqlx::query("UPDATE core.jobs SET run_at = now() WHERE id = $1")
        .bind(id.0)
        .execute(pool)
        .await
        .unwrap();
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn successful_job_runs_once_with_its_payload(pool: PgPool) {
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut registry = Registry::new();
    let seen2 = Arc::clone(&seen);
    registry.register("echo", move |job| {
        let seen = Arc::clone(&seen2);
        async move {
            seen.lock().unwrap().push(job.payload);
            Ok(())
        }
    });
    let id = enqueue(&pool, NewJob::new("echo", json!({"n": 1})))
        .await
        .unwrap();

    let outcome = run_once(&pool, &registry, &fast_config()).await.unwrap();

    assert_eq!(outcome, Outcome::Succeeded(id));
    assert_eq!(*seen.lock().unwrap(), vec![json!({"n": 1})]);
    let r = row(&pool, id).await;
    assert_eq!((r.state.as_str(), r.attempts), ("succeeded", 1));
    assert_eq!(
        run_once(&pool, &registry, &fast_config()).await.unwrap(),
        Outcome::Idle
    );
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn failures_back_off_then_dead_letter(pool: PgPool) {
    let mut registry = Registry::new();
    registry.register("flaky", |_| async { Err(JobError::retry("ESI 503")) });
    let id = enqueue(&pool, NewJob::new("flaky", json!({})).max_attempts(3))
        .await
        .unwrap();
    let config = fast_config();

    assert_eq!(
        run_once(&pool, &registry, &config).await.unwrap(),
        Outcome::Retrying(id)
    );
    let r = row(&pool, id).await;
    assert_eq!((r.state.as_str(), r.attempts), ("queued", 1));
    assert_eq!(r.last_error.as_deref(), Some("ESI 503"));
    assert!((55.0..=60.0).contains(&r.retry_in_s), "{}", r.retry_in_s);
    // Not due yet, so nothing to run.
    assert_eq!(
        run_once(&pool, &registry, &config).await.unwrap(),
        Outcome::Idle
    );

    make_due(&pool, id).await;
    assert_eq!(
        run_once(&pool, &registry, &config).await.unwrap(),
        Outcome::Retrying(id)
    );
    let r = row(&pool, id).await;
    assert!(
        (115.0..=120.0).contains(&r.retry_in_s),
        "second backoff doubles"
    );

    make_due(&pool, id).await;
    assert_eq!(
        run_once(&pool, &registry, &config).await.unwrap(),
        Outcome::Dead(id)
    );
    let r = row(&pool, id).await;
    assert_eq!((r.state.as_str(), r.attempts), ("dead", 3));
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn permanent_error_dead_letters_immediately(pool: PgPool) {
    let mut registry = Registry::new();
    registry.register("bad", |_| async {
        Err(JobError::permanent("character deleted"))
    });
    let id = enqueue(&pool, NewJob::new("bad", json!({}))).await.unwrap();

    let outcome = run_once(&pool, &registry, &fast_config()).await.unwrap();

    assert_eq!(outcome, Outcome::Dead(id));
    assert_eq!(row(&pool, id).await.attempts, 1);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn panics_and_timeouts_are_retried(pool: PgPool) {
    let mut registry = Registry::new();
    registry.register("panics", |_| async { panic!("boom") });
    registry.register("hangs", |_| async {
        tokio::time::sleep(Duration::from_secs(60)).await;
        Ok(())
    });
    let config = WorkerConfig {
        lease: Duration::from_millis(200),
        ..fast_config()
    };
    let panics = enqueue(&pool, NewJob::new("panics", json!({})))
        .await
        .unwrap();
    let hangs = enqueue(&pool, NewJob::new("hangs", json!({})))
        .await
        .unwrap();

    run_once(&pool, &registry, &config).await.unwrap();
    run_once(&pool, &registry, &config).await.unwrap();

    let p = row(&pool, panics).await;
    assert_eq!(p.state, "queued");
    assert_eq!(p.last_error.as_deref(), Some("handler panicked"));
    let h = row(&pool, hangs).await;
    assert_eq!(h.state, "queued");
    assert!(h.last_error.unwrap().starts_with("timed out"));
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn unknown_kinds_are_left_queued(pool: PgPool) {
    let mut registry = Registry::new();
    registry.register("known", |_| async { Ok(()) });
    let id = enqueue(&pool, NewJob::new("from-a-removed-plugin", json!({})))
        .await
        .unwrap();

    let outcome = run_once(&pool, &registry, &fast_config()).await.unwrap();

    assert_eq!(outcome, Outcome::Idle);
    assert_eq!(row(&pool, id).await.state, "queued");
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn expired_lease_is_reclaimed(pool: PgPool) {
    // Simulates a worker that claimed attempt 1 and then crashed.
    let id = enqueue(&pool, NewJob::new("work", json!({})))
        .await
        .unwrap();
    sqlx::query(
        "UPDATE core.jobs SET state = 'running', attempts = 1,
                locked_until = now() - interval '1 second' WHERE id = $1",
    )
    .bind(id.0)
    .execute(&pool)
    .await
    .unwrap();
    let mut registry = Registry::new();
    registry.register("work", |_| async { Ok(()) });

    let outcome = run_once(&pool, &registry, &fast_config()).await.unwrap();

    assert_eq!(outcome, Outcome::Succeeded(id));
    assert_eq!(row(&pool, id).await.attempts, 2);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn reclaimed_after_final_attempt_is_dead(pool: PgPool) {
    let id = enqueue(&pool, NewJob::new("work", json!({})).max_attempts(1))
        .await
        .unwrap();
    sqlx::query(
        "UPDATE core.jobs SET state = 'running', attempts = 1,
                locked_until = now() - interval '1 second' WHERE id = $1",
    )
    .bind(id.0)
    .execute(&pool)
    .await
    .unwrap();
    let mut registry = Registry::new();
    registry.register("work", |_| async { Ok(()) });

    let outcome = run_once(&pool, &registry, &fast_config()).await.unwrap();

    assert_eq!(outcome, Outcome::Dead(id));
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn concurrent_workers_run_each_job_exactly_once(pool: PgPool) {
    let runs = Arc::new(AtomicUsize::new(0));
    let mut registry = Registry::new();
    let runs2 = Arc::clone(&runs);
    registry.register("count", move |_| {
        let runs = Arc::clone(&runs2);
        async move {
            runs.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(5)).await;
            Ok(())
        }
    });
    for n in 0..40 {
        enqueue(&pool, NewJob::new("count", json!({ "n": n })))
            .await
            .unwrap();
    }

    let mut workers = tokio::task::JoinSet::new();
    for _ in 0..8 {
        let (pool, registry, config) = (pool.clone(), registry.clone(), fast_config());
        workers.spawn(async move {
            while run_once(&pool, &registry, &config).await.unwrap() != Outcome::Idle {}
        });
    }
    workers.join_all().await;

    assert_eq!(runs.load(Ordering::SeqCst), 40);
    let succeeded: i64 =
        sqlx::query_scalar("SELECT count(*) FROM core.jobs WHERE state = 'succeeded'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(succeeded, 40);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn worker_pool_processes_jobs_and_shuts_down(pool: PgPool) {
    let mut registry = Registry::new();
    registry.register("noop", |_| async { Ok(()) });
    let workers = WorkerPool::start(
        pool.clone(),
        registry,
        WorkerConfig {
            workers: 2,
            ..fast_config()
        },
    );
    let id = enqueue(&pool, NewJob::new("noop", json!({})))
        .await
        .unwrap();

    let mut state = String::new();
    for _ in 0..100 {
        state = row(&pool, id).await.state;
        if state == "succeeded" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(state, "succeeded");

    tokio::time::timeout(Duration::from_secs(2), workers.shutdown())
        .await
        .expect("idle workers stop promptly");
}
