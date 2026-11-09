//! End-to-end tests of the triage step: real Postgres, real child processes.
//! Needs TASKLANE_TEST_DATABASE_URL (see crates/tasklane-queue/tests/queue.rs).

use std::str::FromStr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::{ConnectOptions, PgPool};
use tasklane_core::State;
use tasklane_orchestrator::host::{CallError, PluginClient, PluginSpec};
use tasklane_orchestrator::{Config, Orchestrator, RunResult};
use tasklane_queue::Queue;

static COUNTER: AtomicU32 = AtomicU32::new(0);
const MIGRATION: &str = include_str!("../../../migrations/0001_init.sql");
const FAKE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/fake_plugin.py");

fn spec() -> PluginSpec {
    PluginSpec {
        program: "python3".into(),
        args: vec!["-I".into(), FAKE.into()],
        env: vec![("PATH".into(), std::env::var("PATH").unwrap())],
    }
}

async fn test_pool() -> Option<PgPool> {
    let Ok(url) = std::env::var("TASKLANE_TEST_DATABASE_URL") else {
        if std::env::var("TASKLANE_REQUIRE_DB").as_deref() == Ok("1") {
            panic!("TASKLANE_REQUIRE_DB=1 but TASKLANE_TEST_DATABASE_URL is not set");
        }
        eprintln!("skipped: TASKLANE_TEST_DATABASE_URL not set");
        return None;
    };
    let name = format!(
        "tl_orch_{}_{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    );
    let opts = PgConnectOptions::from_str(&url)
        .unwrap()
        .log_statements(log::LevelFilter::Off);
    let mut admin = opts.connect().await.unwrap();
    sqlx::query(&format!("CREATE DATABASE \"{name}\""))
        .execute(&mut admin)
        .await
        .unwrap();
    let pool = PgPoolOptions::new()
        .max_connections(16)
        .connect_with(opts.database(&name))
        .await
        .unwrap();
    sqlx::raw_sql(MIGRATION).execute(&pool).await.unwrap();
    Some(pool)
}

struct World {
    pool: PgPool,
    queue: Queue,
    orch: Orchestrator,
}

fn world(pool: PgPool, lease_secs: f64) -> World {
    let queue = Queue::new(pool.clone(), 2);
    let orch = Orchestrator::new(
        pool.clone(),
        queue.clone(),
        PluginClient::spawn(&spec()).unwrap(),
        PluginClient::spawn(&spec()).unwrap(),
        Config {
            owner: "orch-1".into(),
            lease_secs,
            call_timeout: Duration::from_secs(10),
        },
    );
    World { pool, queue, orch }
}

/// New ticket with a pending triage job. Returns (ticket id, job id).
async fn ticket(w: &World, external_ref: &str) -> (i64, i64) {
    let (svc,): (i64,) = sqlx::query_as(
        "INSERT INTO services (kind, source, external_id) VALUES ('repo','test','org/repo')
         ON CONFLICT (source, external_id) DO UPDATE SET kind = 'repo' RETURNING id",
    )
    .fetch_one(&w.pool)
    .await
    .unwrap();
    let (t,): (i64,) = sqlx::query_as(
        "INSERT INTO tickets (service_id, external_ref) VALUES ($1,$2) RETURNING id",
    )
    .bind(svc)
    .bind(external_ref)
    .fetch_one(&w.pool)
    .await
    .unwrap();
    let job = w.queue.enqueue(t, "triage", 0, 0.0).await.unwrap();
    (t, job)
}

async fn state(pool: &PgPool, t: i64) -> String {
    sqlx::query_scalar("SELECT state FROM tickets WHERE id = $1")
        .bind(t)
        .fetch_one(pool)
        .await
        .unwrap()
}
async fn level(pool: &PgPool, t: i64) -> Option<i16> {
    sqlx::query_scalar("SELECT level FROM tickets WHERE id = $1")
        .bind(t)
        .fetch_one(pool)
        .await
        .unwrap()
}
async fn job_status(pool: &PgPool, j: i64) -> String {
    sqlx::query_scalar("SELECT status FROM jobs WHERE id = $1")
        .bind(j)
        .fetch_one(pool)
        .await
        .unwrap()
}
async fn count(pool: &PgPool, sql: &str) -> i64 {
    sqlx::query_scalar(sql).fetch_one(pool).await.unwrap()
}

#[tokio::test]
async fn a_plain_ticket_is_triaged_at_level_one_and_goes_to_development() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let w = world(pool, 30.0);
    let (t, j) = ticket(&w, "T-plain").await;
    assert_eq!(
        w.orch.run_one().await.unwrap(),
        RunResult::Moved {
            ticket_id: t,
            to: State::Queued
        }
    );
    assert_eq!(state(&w.pool, t).await, "queued");
    assert_eq!(level(&w.pool, t).await, Some(1));
    assert_eq!(job_status(&w.pool, j).await, "done");
    assert_eq!(
        count(
            &w.pool,
            "SELECT count(*) FROM jobs WHERE kind = 'develop' AND status = 'queued'"
        )
        .await,
        1
    );
    assert_eq!(
        count(
            &w.pool,
            "SELECT count(*) FROM audit_events WHERE action = 'ticket.triaged'"
        )
        .await,
        1
    );
    let hash: Option<String> = sqlx::query_scalar("SELECT plan_hash FROM tickets WHERE id = $1")
        .bind(t)
        .fetch_one(&w.pool)
        .await
        .unwrap();
    assert_eq!(hash.unwrap().len(), 64);
    assert_eq!(w.orch.run_one().await.unwrap(), RunResult::Idle);
}

#[tokio::test]
async fn the_rule_floor_beats_a_model_that_says_level_one() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let w = world(pool, 30.0);
    let (t, _) = ticket(&w, "T-risky").await; // text mentions migration + password; model says L1
    assert_eq!(
        w.orch.run_one().await.unwrap(),
        RunResult::Moved {
            ticket_id: t,
            to: State::AwaitingApproval
        }
    );
    assert_eq!(level(&w.pool, t).await, Some(2));
    assert_eq!(
        count(&w.pool, "SELECT count(*) FROM jobs WHERE kind = 'develop'").await,
        0,
        "no development before approval"
    );
    let why: serde_json::Value =
        sqlx::query_scalar("SELECT level_reasons FROM tickets WHERE id = $1")
            .bind(t)
            .fetch_one(&w.pool)
            .await
            .unwrap();
    assert_eq!(
        why,
        serde_json::json!(["database migration", "login or permissions"])
    );
}

#[tokio::test]
async fn the_model_can_raise_the_level_above_the_floor() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let w = world(pool, 30.0);
    let (t, _) = ticket(&w, "T-l3").await;
    assert_eq!(
        w.orch.run_one().await.unwrap(),
        RunResult::Moved {
            ticket_id: t,
            to: State::AwaitingApproval
        }
    );
    assert_eq!(level(&w.pool, t).await, Some(3));
}

#[tokio::test]
async fn a_question_for_the_reporter_parks_the_ticket_in_needs_info() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let w = world(pool, 30.0);
    let (t, _) = ticket(&w, "T-ask").await;
    assert_eq!(
        w.orch.run_one().await.unwrap(),
        RunResult::Moved {
            ticket_id: t,
            to: State::NeedsInfo
        }
    );
    assert_eq!(
        count(&w.pool, "SELECT count(*) FROM jobs WHERE kind = 'develop'").await,
        0
    );
}

#[tokio::test]
async fn a_plugin_crash_gives_the_job_back_and_leaves_the_ticket_alone_until_attempts_run_out() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let w = world(pool, 30.0);
    let (t, j) = ticket(&w, "T-crash").await;
    assert_eq!(w.orch.run_one().await.unwrap(), RunResult::Retried);
    assert_eq!(state(&w.pool, t).await, "new");
    assert_eq!(job_status(&w.pool, j).await, "queued");
    assert_eq!(count(&w.pool, "SELECT count(*) FROM audit_events").await, 0);
}

#[tokio::test]
async fn unusable_plugin_output_is_not_trusted() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let w = world(pool, 30.0);
    let (t, _) = ticket(&w, "T-badjson").await;
    assert_eq!(w.orch.run_one().await.unwrap(), RunResult::Retried);
    assert_eq!(state(&w.pool, t).await, "new");
}

#[tokio::test]
async fn a_worker_that_lost_its_lease_changes_nothing() {
    let Some(pool) = test_pool().await else {
        return;
    };
    // lease 3 s => heartbeat every 1 s; the fake engine takes 2.5 s on "slow" tickets
    let w = Arc::new(world(pool, 3.0));
    let (t, j) = ticket(&w, "T-slow").await;
    let runner = {
        let w = w.clone();
        tokio::spawn(async move { w.orch.run_one().await.unwrap() })
    };
    tokio::time::sleep(Duration::from_millis(300)).await;
    // the host "freezes": lease expires and the reaper hands the job back
    sqlx::query("UPDATE jobs SET lease_until = now() - interval '1 second' WHERE id = $1")
        .bind(j)
        .execute(&w.pool)
        .await
        .unwrap();
    assert_eq!(w.queue.reap().await.unwrap().requeued, 1);
    assert_eq!(runner.await.unwrap(), RunResult::LeaseLost);
    assert_eq!(state(&w.pool, t).await, "new");
    assert_eq!(job_status(&w.pool, j).await, "queued");
    assert_eq!(count(&w.pool, "SELECT count(*) FROM audit_events").await, 0);
    assert_eq!(
        count(&w.pool, "SELECT count(*) FROM jobs WHERE kind = 'develop'").await,
        0
    );
}

#[tokio::test]
async fn a_job_for_a_ticket_that_already_moved_on_is_closed_without_changes() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let w = world(pool, 30.0);
    let (t, j) = ticket(&w, "T-plain").await;
    sqlx::query("UPDATE tickets SET state = 'closed_done' WHERE id = $1")
        .bind(t)
        .execute(&w.pool)
        .await
        .unwrap();
    assert_eq!(w.orch.run_one().await.unwrap(), RunResult::Skipped);
    assert_eq!(job_status(&w.pool, j).await, "done");
    assert_eq!(state(&w.pool, t).await, "closed_done");
}

#[tokio::test]
async fn plugin_host_matches_answers_to_calls_even_out_of_order() {
    let p = PluginClient::spawn(&spec()).unwrap();
    let slow = p.call(
        "echo",
        serde_json::json!({"delay": 0.4, "n": 1}),
        Duration::from_secs(5),
    );
    let fast = p.call(
        "echo",
        serde_json::json!({"delay": 0.0, "n": 2}),
        Duration::from_secs(5),
    );
    let (slow, fast) = tokio::join!(slow, fast);
    // the fake plug-in is single-threaded so it answers in order, but each caller must get its own answer
    assert_eq!(slow.unwrap()["n"], 1);
    assert_eq!(fast.unwrap()["n"], 2);
}

#[tokio::test]
async fn plugin_host_reports_timeout_unknown_ops_and_a_dead_plugin() {
    let p = PluginClient::spawn(&spec()).unwrap();
    assert!(matches!(
        p.call(
            "echo",
            serde_json::json!({"delay": 2.0}),
            Duration::from_millis(200)
        )
        .await,
        Err(CallError::Timeout)
    ));
    let p = PluginClient::spawn(&spec()).unwrap();
    match p
        .call("nope", serde_json::Value::Null, Duration::from_secs(5))
        .await
    {
        Err(CallError::Plugin(e)) => assert_eq!(e.kind, tasklane_plugin::ErrorKind::Permanent),
        other => panic!("{other:?}"),
    }
    let p = PluginClient::spawn(&spec()).unwrap();
    let r = p
        .call(
            "run",
            serde_json::json!({"ref": "T-crash"}),
            Duration::from_secs(5),
        )
        .await;
    assert!(matches!(r, Err(CallError::Closed)), "{r:?}");
}

#[tokio::test]
async fn plugins_get_no_inherited_environment() {
    std::env::set_var("TASKLANE_SECRET_PROBE", "leak");
    let p = PluginClient::spawn(&PluginSpec {
        program: "python3".into(),
        args: vec!["-I".into(), "-c".into(), "import os,sys,json\nfor l in sys.stdin:\n r=json.loads(l)\n print(json.dumps({'id':r['id'],'ok':True,'result':{'v':os.environ.get('TASKLANE_SECRET_PROBE')}}),flush=True)".into()],
        env: vec![("PATH".into(), std::env::var("PATH").unwrap())],
    })
    .unwrap();
    let r = p
        .call("x", serde_json::Value::Null, Duration::from_secs(5))
        .await
        .unwrap();
    assert_eq!(r["v"], serde_json::Value::Null);
}
