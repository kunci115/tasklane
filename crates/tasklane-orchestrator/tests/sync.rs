//! The sync job against a real database and the fake source.

mod common;
use common::*;

use tasklane_orchestrator::sync::{Noted, SyncReport};
use tasklane_orchestrator::MARKER;

fn write(w: &World, name: &str, v: serde_json::Value) {
    std::fs::write(w.state_dir().join(name), v.to_string()).unwrap();
}

#[tokio::test]
async fn new_repos_start_read_only_and_new_issues_become_tickets_once() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let w = world(pool, |_| {}).await;
    write(
        &w,
        "services.json",
        serde_json::json!([
            {"external_id": "org/new", "default_branch": "trunk", "archived": false, "private": true},
            {"external_id": "org/old", "default_branch": "main", "archived": true, "private": true},
            {"external_id": "org/repo", "default_branch": "main", "archived": false, "private": true}
        ]),
    );
    write(
        &w,
        "changes.json",
        serde_json::json!([
            {"service": "org/new", "ref": "1", "kind": "issue", "updated_at": "t"},
            {"service": "org/old", "ref": "2", "kind": "issue", "updated_at": "t"},
            {"service": "org/new", "ref": "3", "kind": "pr", "updated_at": "t"},
            {"service": "org/repo", "ref": "9", "kind": "issue", "updated_at": "t"}
        ]),
    );
    let r = w.orch.sync_once().await.unwrap();
    assert_eq!(
        r,
        SyncReport {
            services_seen: 3,
            tickets_created: 2,
            tickets_resumed: 0
        }
    );

    let modes: Vec<(String, String)> =
        sqlx::query_as("SELECT external_id, mode FROM services ORDER BY external_id")
            .fetch_all(&w.pool)
            .await
            .unwrap();
    assert_eq!(
        modes,
        vec![
            ("org/new".into(), "triage".into()),
            ("org/old".into(), "watch".into()),
            ("org/repo".into(), "full".into())
        ]
    );
    let facts: serde_json::Value =
        sqlx::query_scalar("SELECT facts FROM services WHERE external_id = 'org/new'")
            .fetch_one(&w.pool)
            .await
            .unwrap();
    assert_eq!(facts["default_branch"], "trunk");
    assert_eq!(
        facts["test_command"], "pytest",
        "facts were detected for the new repo"
    );
    assert_eq!(
        count(&w.pool, "SELECT count(*) FROM jobs WHERE kind = 'triage'").await,
        2
    );
    // the cursor moved
    let cursor: String =
        sqlx::query_scalar("SELECT value FROM sync_state WHERE key = 'changes_cursor'")
            .fetch_one(&w.pool)
            .await
            .unwrap();
    assert_eq!(cursor, "c4");

    // the same issues again create nothing
    let r = w.orch.sync_once().await.unwrap();
    assert_eq!((r.tickets_created, r.tickets_resumed), (0, 0));
    assert_eq!(count(&w.pool, "SELECT count(*) FROM tickets").await, 2);
}

#[tokio::test]
async fn in_a_triage_repo_even_a_level_one_ticket_waits_for_a_person_and_the_approval_wakes_the_repo(
) {
    let Some(pool) = test_pool().await else {
        return;
    };
    let w = world(pool, |_| {}).await;
    add_principal(&w.pool, "alice", "team_approver").await;
    sqlx::query("UPDATE services SET mode = 'triage'")
        .execute(&w.pool)
        .await
        .unwrap();
    let t = ticket(&w, "T-plain").await;
    drive(&w).await;
    assert_eq!(
        state(&w.pool, t).await,
        "awaiting_approval",
        "a read-only repo does not start work by itself"
    );

    let hash: String = sqlx::query_scalar("SELECT plan_hash FROM tickets WHERE id = $1")
        .bind(t)
        .fetch_one(&w.pool)
        .await
        .unwrap();
    let who = tasklane_orchestrator::approval::Principal {
        channel: "console".into(),
        reference: "alice".into(),
    };
    tasklane_orchestrator::approval::decide(
        &w.pool,
        t,
        &who,
        "console",
        tasklane_orchestrator::approval::Decision::Approve,
        &hash,
    )
    .await
    .unwrap();
    let mode: String = sqlx::query_scalar("SELECT mode FROM services")
        .fetch_one(&w.pool)
        .await
        .unwrap();
    assert_eq!(mode, "full");
    drive(&w).await;
    assert_eq!(state(&w.pool, t).await, "merged");
}

#[tokio::test]
async fn a_reply_from_a_person_resumes_a_ticket_that_asked_a_question_but_our_own_comment_does_not()
{
    let Some(pool) = test_pool().await else {
        return;
    };
    let w = world(pool, |_| {}).await;
    let t = ticket(&w, "T-ask").await;
    drive(&w).await;
    assert_eq!(state(&w.pool, t).await, "needs_info");

    // our own question is the last comment: nothing happens
    write(
        &w,
        "comments_T-ask.json",
        serde_json::json!([{"author": "bot", "body": format!("Which page?\n\n{MARKER}"), "at": "t"}]),
    );
    assert_eq!(
        w.orch.note_issue("org/repo", "T-ask").await.unwrap(),
        Noted::Nothing
    );
    assert_eq!(state(&w.pool, t).await, "needs_info");

    // a person answers
    write(
        &w,
        "comments_T-ask.json",
        serde_json::json!([
            {"author": "bot", "body": format!("Which page?\n\n{MARKER}"), "at": "t"},
            {"author": "sam", "body": "The report page", "at": "t2"}
        ]),
    );
    assert_eq!(
        w.orch.note_issue("org/repo", "T-ask").await.unwrap(),
        Noted::Resumed
    );
    assert_eq!(state(&w.pool, t).await, "new");
    assert_eq!(
        count(
            &w.pool,
            "SELECT count(*) FROM jobs WHERE kind = 'triage' AND status = 'queued'"
        )
        .await,
        1
    );
}
