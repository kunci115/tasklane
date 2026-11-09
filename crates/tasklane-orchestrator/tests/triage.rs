//! Triage step and plug-in host, against real Postgres and real child processes.

mod common;
use common::*;

use std::sync::Arc;
use std::time::Duration;

use tasklane_core::State;
use tasklane_orchestrator::host::{CallError, PluginClient, PluginSpec};
use tasklane_orchestrator::RunResult;

#[tokio::test]
async fn a_plain_ticket_is_triaged_at_level_one_and_goes_to_development() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let w = world(pool, |_| {}).await;
    let t = ticket(&w, "T-plain").await;
    assert_eq!(
        w.orch.run_one().await.unwrap(),
        RunResult::Moved {
            ticket_id: t,
            to: State::Queued
        }
    );
    assert_eq!(state(&w.pool, t).await, "queued");
    assert_eq!(level(&w.pool, t).await, Some(1));
    assert_eq!(
        count(
            &w.pool,
            "SELECT count(*) FROM jobs WHERE kind = 'triage' AND status = 'done'"
        )
        .await,
        1
    );
    assert_eq!(
        count(
            &w.pool,
            "SELECT count(*) FROM jobs WHERE kind = 'develop' AND status = 'queued'"
        )
        .await,
        1
    );
    assert_eq!(audit_actions(&w.pool, t).await, vec!["ticket.triaged"]);
    let hash: Option<String> = sqlx::query_scalar("SELECT plan_hash FROM tickets WHERE id = $1")
        .bind(t)
        .fetch_one(&w.pool)
        .await
        .unwrap();
    assert_eq!(hash.unwrap().len(), 64);
    assert!(
        w.read_state("comments.log").contains("Triaged as level 1"),
        "the reporter is told"
    );
}

#[tokio::test]
async fn the_rule_floor_beats_a_model_that_says_level_one() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let w = world(pool, |_| {}).await;
    let t = ticket(&w, "T-risky").await;
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
    let w = world(pool, |_| {}).await;
    let t = ticket(&w, "T-l3").await;
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
    let w = world(pool, |_| {}).await;
    let t = ticket(&w, "T-ask").await;
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
    assert!(w.read_state("comments.log").contains("Which page?"));
}

#[tokio::test]
async fn a_plugin_crash_gives_the_job_back_and_the_ticket_fails_when_attempts_run_out() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let w = world(pool, |_| {}).await;
    let t = ticket(&w, "T-crash").await;
    assert_eq!(w.orch.run_one().await.unwrap(), RunResult::Retried);
    assert_eq!(state(&w.pool, t).await, "new");
    assert_eq!(count(&w.pool, "SELECT count(*) FROM audit_events").await, 0);
}

#[tokio::test]
async fn unusable_plugin_output_is_not_trusted() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let w = world(pool, |_| {}).await;
    let t = ticket(&w, "T-badjson").await;
    assert_eq!(w.orch.run_one().await.unwrap(), RunResult::Retried);
    assert_eq!(state(&w.pool, t).await, "new");
}

#[tokio::test]
async fn a_worker_that_lost_its_lease_changes_nothing() {
    let Some(pool) = test_pool().await else {
        return;
    };
    // lease 3 s => heartbeat every 1 s; the fake engine takes 2.5 s on "slow" tickets
    let w = Arc::new(world(pool, |c| c.lease_secs = 3.0).await);
    let t = ticket(&w, "T-slow").await;
    let (j,): (i64,) = sqlx::query_as("SELECT id FROM jobs WHERE ticket_id = $1")
        .bind(t)
        .fetch_one(&w.pool)
        .await
        .unwrap();
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
    let w = world(pool, |_| {}).await;
    let t = ticket(&w, "T-plain").await;
    sqlx::query("UPDATE tickets SET state = 'closed_done' WHERE id = $1")
        .bind(t)
        .execute(&w.pool)
        .await
        .unwrap();
    assert_eq!(w.orch.run_one().await.unwrap(), RunResult::Skipped);
    assert_eq!(state(&w.pool, t).await, "closed_done");
    assert_eq!(
        count(&w.pool, "SELECT count(*) FROM jobs WHERE status = 'done'").await,
        1
    );
}

#[tokio::test]
async fn plugin_host_matches_answers_to_calls() {
    let p = PluginClient::spawn(&spec(&[])).unwrap();
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
    assert_eq!(slow.unwrap()["n"], 1);
    assert_eq!(fast.unwrap()["n"], 2);
}

#[tokio::test]
async fn plugin_host_reports_timeout_unknown_ops_and_a_dead_plugin() {
    let env = [
        ("FAKE_STATE_DIR", "/tmp".to_string()),
        ("FAKE_REMOTE_ROOT", "/tmp".to_string()),
    ];
    let p = PluginClient::spawn(&spec(&env)).unwrap();
    assert!(matches!(
        p.call(
            "echo",
            serde_json::json!({"delay": 2.0}),
            Duration::from_millis(200)
        )
        .await,
        Err(CallError::Timeout)
    ));
    let p = PluginClient::spawn(&spec(&env)).unwrap();
    match p
        .call("nope", serde_json::Value::Null, Duration::from_secs(5))
        .await
    {
        Err(CallError::Plugin(e)) => assert_eq!(e.kind, tasklane_plugin::ErrorKind::Permanent),
        other => panic!("{other:?}"),
    }
    let p = PluginClient::spawn(&spec(&env)).unwrap();
    let r = p
        .call(
            "run",
            serde_json::json!({"ticket": {"ref": "T-crash"}}),
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
