//! Integration tests against a real Postgres.
//! Set TASKLANE_TEST_DATABASE_URL to a superuser URL, e.g.
//!   postgres://tasklane:tasklane@127.0.0.1:5432/postgres
//! Each test makes its own database. Without the variable the tests are skipped,
//! unless TASKLANE_REQUIRE_DB=1 (CI sets it), which turns a missing database
//! into a failure.

use serde_json::json;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::{ConnectOptions, PgPool};
use std::collections::HashSet;
use std::str::FromStr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tasklane_queue::{Error, Outcome, Queue, ReapReport};

static COUNTER: AtomicU32 = AtomicU32::new(0);
const MIGRATION: &str = include_str!("../../../migrations/0001_init.sql");

struct TestDb {
    pool: PgPool,
}

async fn test_db() -> Option<TestDb> {
    let Ok(url) = std::env::var("TASKLANE_TEST_DATABASE_URL") else {
        if std::env::var("TASKLANE_REQUIRE_DB").as_deref() == Ok("1") {
            panic!("TASKLANE_REQUIRE_DB=1 but TASKLANE_TEST_DATABASE_URL is not set");
        }
        eprintln!("skipped: TASKLANE_TEST_DATABASE_URL not set");
        return None;
    };
    let name = format!(
        "tl_test_{}_{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    );
    let opts = PgConnectOptions::from_str(&url)
        .unwrap()
        .log_statements(log::LevelFilter::Off);
    let mut admin = opts.connect().await.unwrap();
    drop_dead_test_databases(&mut admin).await;
    sqlx::query(&format!("CREATE DATABASE \"{name}\""))
        .execute(&mut admin)
        .await
        .unwrap();
    let pool = PgPoolOptions::new()
        .max_connections(32)
        .connect_with(opts.clone().database(&name))
        .await
        .unwrap();
    sqlx::raw_sql(MIGRATION).execute(&pool).await.unwrap();
    Some(TestDb { pool })
}

async fn seed_ticket(pool: &PgPool, n: i64) -> i64 {
    let (svc,): (i64,) = sqlx::query_as(
        "INSERT INTO services (kind, source, external_id) VALUES ('repo','test',$1)
         ON CONFLICT (source, external_id) DO UPDATE SET kind = 'repo' RETURNING id",
    )
    .bind("svc")
    .fetch_one(pool)
    .await
    .unwrap();
    let (t,): (i64,) = sqlx::query_as(
        "INSERT INTO tickets (service_id, external_ref) VALUES ($1,$2) RETURNING id",
    )
    .bind(svc)
    .bind(format!("T-{n}"))
    .fetch_one(pool)
    .await
    .unwrap();
    t
}

async fn status_of(pool: &PgPool, id: i64) -> String {
    sqlx::query_scalar("SELECT status FROM jobs WHERE id = $1")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn expire(pool: &PgPool) {
    // make "lease_until < now()" true for any running job
    sqlx::query(
        "UPDATE jobs SET lease_until = now() - interval '1 second' WHERE status = 'running'",
    )
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn one_job_goes_to_exactly_one_of_many_racing_workers() {
    let Some(db) = test_db().await else { return };
    let q = Queue::new(db.pool.clone(), 3);
    let t = seed_ticket(&db.pool, 1).await;
    q.enqueue(t, "triage", 0, 0.0).await.unwrap();
    let mut tasks = vec![];
    for i in 0..24 {
        let q = q.clone();
        tasks.push(tokio::spawn(async move {
            q.claim(&format!("w{i}"), 60.0, &[]).await.unwrap()
        }));
    }
    let mut won = 0;
    for t in tasks {
        if t.await.unwrap().is_some() {
            won += 1;
        }
    }
    assert_eq!(won, 1);
}

#[tokio::test]
async fn many_workers_claim_every_job_exactly_once() {
    let Some(db) = test_db().await else { return };
    let q = Queue::new(db.pool.clone(), 3);
    for n in 0..200 {
        let t = seed_ticket(&db.pool, n).await;
        q.enqueue(t, "develop", 0, 0.0).await.unwrap();
    }
    let seen = Arc::new(Mutex::new(Vec::new()));
    let mut tasks = vec![];
    for w in 0..16 {
        let (q, seen) = (q.clone(), seen.clone());
        tasks.push(tokio::spawn(async move {
            while let Some(j) = q.claim(&format!("w{w}"), 60.0, &[]).await.unwrap() {
                seen.lock().unwrap().push(j.id);
            }
        }));
    }
    for t in tasks {
        t.await.unwrap();
    }
    let (total, distinct) = {
        let seen = seen.lock().unwrap();
        (seen.len(), seen.iter().collect::<HashSet<_>>().len())
    };
    assert_eq!(total, 200);
    assert_eq!(distinct, 200, "a job was claimed twice");
    let (n, max_attempts): (i64, i16) =
        sqlx::query_as("SELECT count(*), max(attempts) FROM jobs WHERE status = 'running'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!((n, max_attempts), (200, 1));
}

#[tokio::test]
async fn priority_first_then_age_and_future_jobs_wait() {
    let Some(db) = test_db().await else { return };
    let q = Queue::new(db.pool.clone(), 3);
    let t = seed_ticket(&db.pool, 1).await;
    let low = q.enqueue(t, "a", 0, 0.0).await.unwrap();
    let high = q.enqueue(t, "a", 5, 0.0).await.unwrap();
    let later = q.enqueue(t, "a", 9, 3600.0).await.unwrap();
    assert_eq!(q.claim("w", 60.0, &[]).await.unwrap().unwrap().id, high);
    assert_eq!(q.claim("w", 60.0, &[]).await.unwrap().unwrap().id, low);
    assert!(
        q.claim("w", 60.0, &[]).await.unwrap().is_none(),
        "job {later} is not due yet"
    );
}

#[tokio::test]
async fn kinds_filter_only_hands_out_matching_jobs() {
    let Some(db) = test_db().await else { return };
    let q = Queue::new(db.pool.clone(), 3);
    let t = seed_ticket(&db.pool, 1).await;
    q.enqueue(t, "triage", 0, 0.0).await.unwrap();
    let dev = q.enqueue(t, "develop", 0, 0.0).await.unwrap();
    assert_eq!(
        q.claim("w", 60.0, &["develop"]).await.unwrap().unwrap().id,
        dev
    );
    assert!(q.claim("w", 60.0, &["develop"]).await.unwrap().is_none());
}

#[tokio::test]
async fn crashed_worker_leaves_its_checkpoint_for_the_next_one() {
    let Some(db) = test_db().await else { return };
    let q = Queue::new(db.pool.clone(), 3);
    let t = seed_ticket(&db.pool, 1).await;
    let id = q.enqueue(t, "develop", 0, 0.0).await.unwrap();

    let a = q.claim("A", 60.0, &[]).await.unwrap().unwrap();
    q.checkpoint(&a, &json!({"step": 2, "branch": "x"}))
        .await
        .unwrap();
    // worker A "crashes": it never heartbeats again
    expire(&db.pool).await;
    assert_eq!(
        q.reap().await.unwrap(),
        ReapReport {
            requeued: 1,
            failed: 0
        }
    );
    assert_eq!(status_of(&db.pool, id).await, "queued");

    // the reaper backs the job off; pretend the delay has passed
    sqlx::query("UPDATE jobs SET run_after = now()")
        .execute(&db.pool)
        .await
        .unwrap();
    let b = q.claim("B", 60.0, &[]).await.unwrap().unwrap();
    assert_eq!(b.checkpoint, json!({"step": 2, "branch": "x"}));
    assert_eq!(b.attempts, 2);
    assert!(b.fence > a.fence);
}

#[tokio::test]
async fn a_stale_worker_can_no_longer_write_after_the_job_moved_on() {
    let Some(db) = test_db().await else { return };
    let q = Queue::new(db.pool.clone(), 3);
    let t = seed_ticket(&db.pool, 1).await;
    q.enqueue(t, "develop", 0, 0.0).await.unwrap();
    let a = q.claim("A", 60.0, &[]).await.unwrap().unwrap();
    expire(&db.pool).await;
    q.reap().await.unwrap();
    sqlx::query("UPDATE jobs SET run_after = now()")
        .execute(&db.pool)
        .await
        .unwrap();
    let b = q.claim("B", 60.0, &[]).await.unwrap().unwrap();

    assert!(matches!(q.heartbeat(&a, 60.0).await, Err(Error::LeaseLost)));
    assert!(matches!(
        q.checkpoint(&a, &json!({"evil": true})).await,
        Err(Error::LeaseLost)
    ));
    assert!(matches!(q.complete(&a).await, Err(Error::LeaseLost)));
    assert!(matches!(q.fail(&a).await, Err(Error::LeaseLost)));

    q.checkpoint(&b, &json!({"ok": true})).await.unwrap();
    q.complete(&b).await.unwrap();
    assert!(
        matches!(q.complete(&b).await, Err(Error::LeaseLost)),
        "done jobs cannot be completed twice"
    );
}

#[tokio::test]
async fn a_stale_worker_is_fenced_even_if_it_has_the_same_name() {
    let Some(db) = test_db().await else { return };
    let q = Queue::new(db.pool.clone(), 3);
    let t = seed_ticket(&db.pool, 1).await;
    q.enqueue(t, "develop", 0, 0.0).await.unwrap();
    let old = q.claim("same", 60.0, &[]).await.unwrap().unwrap();
    expire(&db.pool).await;
    q.reap().await.unwrap();
    sqlx::query("UPDATE jobs SET run_after = now()")
        .execute(&db.pool)
        .await
        .unwrap();
    let new = q.claim("same", 60.0, &[]).await.unwrap().unwrap();
    assert!(matches!(q.complete(&old).await, Err(Error::LeaseLost)));
    q.complete(&new).await.unwrap();
}

#[tokio::test]
async fn heartbeat_keeps_the_lease_and_the_reaper_leaves_it_alone() {
    let Some(db) = test_db().await else { return };
    let q = Queue::new(db.pool.clone(), 3);
    let t = seed_ticket(&db.pool, 1).await;
    q.enqueue(t, "develop", 0, 0.0).await.unwrap();
    let a = q.claim("A", 0.05, &[]).await.unwrap().unwrap();
    q.heartbeat(&a, 60.0).await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(q.reap().await.unwrap(), ReapReport::default());
    q.complete(&a).await.unwrap();
}

#[tokio::test]
async fn attempts_run_out_and_the_job_fails_for_good() {
    let Some(db) = test_db().await else { return };
    let q = Queue::new(db.pool.clone(), 2);
    let t = seed_ticket(&db.pool, 1).await;
    let id = q.enqueue(t, "develop", 0, 0.0).await.unwrap();

    let a = q.claim("A", 60.0, &[]).await.unwrap().unwrap();
    assert_eq!(q.fail(&a).await.unwrap(), Outcome::Requeued);
    sqlx::query("UPDATE jobs SET run_after = now()")
        .execute(&db.pool)
        .await
        .unwrap();
    let b = q.claim("B", 60.0, &[]).await.unwrap().unwrap();
    assert_eq!(b.attempts, 2);
    expire(&db.pool).await; // B crashes on its last attempt
    assert_eq!(
        q.reap().await.unwrap(),
        ReapReport {
            requeued: 0,
            failed: 1
        }
    );
    assert_eq!(status_of(&db.pool, id).await, "failed");
    assert!(q.claim("C", 60.0, &[]).await.unwrap().is_none());
}

#[tokio::test]
async fn reaper_and_heartbeat_racing_never_both_win() {
    let Some(db) = test_db().await else { return };
    let q = Queue::new(db.pool.clone(), 50);
    for round in 0..40 {
        let t = seed_ticket(&db.pool, 1000 + round).await;
        let id = q.enqueue(t, "develop", 0, 0.0).await.unwrap();
        let job = q.claim("A", 60.0, &["develop"]).await.unwrap().unwrap();
        assert_eq!(job.id, id);
        expire(&db.pool).await;
        let (hb, rp) = tokio::join!(q.heartbeat(&job, 60.0), q.reap());
        let reaped = rp.unwrap().requeued;
        match hb {
            Ok(()) => assert_eq!(reaped, 0, "heartbeat won, so the reaper must skip the job"),
            Err(Error::LeaseLost) => {
                assert_eq!(reaped, 1, "reaper won, so the heartbeat must fail")
            }
            Err(e) => panic!("{e}"),
        }
        // clean up for the next round
        sqlx::query("UPDATE jobs SET status = 'done', lease_owner = NULL, lease_until = NULL")
            .execute(&db.pool)
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn deferring_waits_without_using_an_attempt_and_is_fenced() {
    let Some(db) = test_db().await else { return };
    let q = Queue::new(db.pool.clone(), 1);
    let t = seed_ticket(&db.pool, 1).await;
    q.enqueue(t, "ci", 0, 0.0).await.unwrap();
    let a = q.claim("A", 60.0, &[]).await.unwrap().unwrap();
    q.defer(&a, 3600.0).await.unwrap();
    assert!(
        q.claim("A", 60.0, &[]).await.unwrap().is_none(),
        "deferred job is not due"
    );
    assert!(
        matches!(q.defer(&a, 1.0).await, Err(Error::LeaseLost)),
        "second defer by the same stale claim"
    );
    sqlx::query("UPDATE jobs SET run_after = now()")
        .execute(&db.pool)
        .await
        .unwrap();
    let b = q.claim("B", 60.0, &[]).await.unwrap().unwrap();
    assert_eq!(
        b.attempts, 1,
        "waiting must not use up attempts (max is 1 here)"
    );
}

/// Drop the databases earlier test runs left behind (`tl_<kind>_<pid>_<n>`) whose test process is
/// gone, so a long-lived test Postgres does not fill up with thousands of them.
async fn drop_dead_test_databases(admin: &mut sqlx::PgConnection) {
    let names: Vec<String> =
        sqlx::query_scalar("SELECT datname FROM pg_database WHERE datname LIKE 'tl\\_%'")
            .fetch_all(&mut *admin)
            .await
            .unwrap_or_default();
    for n in names {
        let pid = n.split('_').nth(2).and_then(|p| p.parse::<u32>().ok());
        if let Some(pid) = pid {
            if !std::path::Path::new(&format!("/proc/{pid}")).exists() {
                let _ = sqlx::query(&format!("DROP DATABASE IF EXISTS \"{n}\" WITH (FORCE)"))
                    .execute(&mut *admin)
                    .await;
            }
        }
    }
}
