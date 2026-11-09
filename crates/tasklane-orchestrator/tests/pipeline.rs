//! The whole path from a new ticket to a merge, with a real Postgres, real git
//! (a bare remote on disk) and a fake source/engine plug-in. Includes approvals.

mod common;
use common::*;

use tasklane_orchestrator::approval::{decide, ApprovalError, Decision, Principal};
use tasklane_orchestrator::RunResult;

fn console(who: &str) -> Principal {
    Principal {
        channel: "console".into(),
        reference: who.into(),
    }
}
async fn plan_hash(w: &World, t: i64) -> String {
    sqlx::query_scalar("SELECT plan_hash FROM tickets WHERE id = $1")
        .bind(t)
        .fetch_one(&w.pool)
        .await
        .unwrap()
}
fn remote_has_branch(w: &World, branch: &str) -> bool {
    !sh(&w.remote(), &format!("git branch --list {branch}"))
        .trim()
        .is_empty()
}

#[tokio::test]
async fn a_level_one_ticket_goes_all_the_way_to_merged_without_a_human() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let w = world(pool, |_| {}).await;
    let t = ticket(&w, "T-plain").await;
    drive(&w).await;
    assert_eq!(state(&w.pool, t).await, "merged");
    assert_eq!(
        audit_actions(&w.pool, t).await,
        vec![
            "ticket.triaged",
            "ticket.developing",
            "ticket.developed",
            "ticket.pr_opened",
            "ticket.ci_green",
            "ticket.reviewed",
            "ticket.merged"
        ]
    );
    assert!(
        remote_has_branch(&w, &format!("tasklane/{t}")),
        "the runner pushed the branch"
    );
    let comments = w.read_state("comments.log");
    assert!(
        comments.contains("Opened pull request 7") && comments.contains("Merged pull request 7"),
        "{comments}"
    );
    // the merge used a run token with write permissions, not the read token
    assert!(w.read_state("merged").contains("contents:write"));
    assert_eq!(
        count(
            &w.pool,
            "SELECT count(*) FROM jobs WHERE status NOT IN ('done')"
        )
        .await,
        0
    );
    // the working tree was cleaned up
    assert!(
        std::fs::read_dir(w.dir.path().join("data/work"))
            .map(|d| d.count())
            .unwrap_or(0)
            == 0
    );
}

#[tokio::test]
async fn a_change_to_protected_paths_stops_for_approval_of_that_exact_diff_then_merges() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let w = world(pool, |_| {}).await;
    add_principal(&w.pool, "alice", "team_approver").await;
    add_principal(&w.pool, "boss", "owner").await;
    let t = ticket(&w, "T-protected").await; // triage says level 1; the diff touches .github/workflows
    drive(&w).await;
    assert_eq!(state(&w.pool, t).await, "awaiting_approval");
    assert_eq!(level(&w.pool, t).await, Some(2));
    assert!(
        !remote_has_branch(&w, &format!("tasklane/{t}")),
        "nothing is pushed before the approval"
    );
    let hash = plan_hash(&w, t).await;

    // the approval is refused for strangers, for stale plans, and for a role that is too low
    assert!(matches!(
        decide(
            &w.pool,
            t,
            &console("mallory"),
            "console",
            Decision::Approve,
            &hash
        )
        .await,
        Err(ApprovalError::NotAllowed(_))
    ));
    assert!(matches!(
        decide(
            &w.pool,
            t,
            &console("alice"),
            "console",
            Decision::Approve,
            "0000"
        )
        .await,
        Err(ApprovalError::StalePlan)
    ));
    assert_eq!(state(&w.pool, t).await, "awaiting_approval");

    assert_eq!(
        decide(
            &w.pool,
            t,
            &console("alice"),
            "console",
            Decision::Approve,
            &hash
        )
        .await
        .unwrap()
        .as_str(),
        "queued"
    );
    assert!(
        matches!(
            decide(
                &w.pool,
                t,
                &console("alice"),
                "console",
                Decision::Approve,
                &hash
            )
            .await,
            Err(ApprovalError::WrongState(_))
        ),
        "no double approval"
    );
    drive(&w).await;
    assert_eq!(state(&w.pool, t).await, "merged");
    assert!(remote_has_branch(&w, &format!("tasklane/{t}")));
    assert_eq!(count(&w.pool, "SELECT count(*) FROM approvals").await, 1);
    // the approved diff was developed once: approval resumed at the PR step, not at development
    assert_eq!(w.read_state("dev_calls.log").lines().count(), 1);
}

#[tokio::test]
async fn level_three_needs_the_owner_and_a_rejection_closes_the_ticket() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let w = world(pool, |_| {}).await;
    add_principal(&w.pool, "alice", "team_approver").await;
    add_principal(&w.pool, "boss", "owner").await;
    let t = ticket(&w, "T-l3").await;
    drive(&w).await;
    let hash = plan_hash(&w, t).await;
    assert!(matches!(
        decide(
            &w.pool,
            t,
            &console("alice"),
            "chat",
            Decision::Approve,
            &hash
        )
        .await,
        Err(ApprovalError::NotAllowed(_))
    ));
    assert_eq!(
        decide(
            &w.pool,
            t,
            &console("boss"),
            "console",
            Decision::Reject,
            &hash
        )
        .await
        .unwrap()
        .as_str(),
        "closed_rejected"
    );
    assert_eq!(
        count(&w.pool, "SELECT count(*) FROM jobs WHERE kind = 'develop'").await,
        0
    );
}

#[tokio::test]
async fn a_large_diff_raises_the_level_even_if_no_protected_path_is_touched() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let w = world(pool, |_| {}).await;
    let t = ticket(&w, "T-big").await;
    drive(&w).await;
    assert_eq!(state(&w.pool, t).await, "awaiting_approval");
    assert!(w.read_state("comments.log").contains("changed lines"));
}

#[tokio::test]
async fn red_ci_sends_the_ticket_back_once_with_the_failure_and_then_it_merges() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let w = world(pool, |_| {}).await;
    w.arm("ci_red");
    let t = ticket(&w, "T-plain").await;
    drive(&w).await;
    assert_eq!(state(&w.pool, t).await, "merged");
    let calls = w.read_state("dev_calls.log");
    assert_eq!(calls.lines().count(), 2, "{calls}");
    assert!(
        calls.lines().nth(1).unwrap().contains("CI failed: tests"),
        "the second attempt is told what failed: {calls}"
    );
    assert!(audit_actions(&w.pool, t)
        .await
        .contains(&"ticket.ci_red".to_string()));
}

#[tokio::test]
async fn ci_that_keeps_failing_ends_with_a_person_not_an_endless_loop() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let w = world(pool, |_| {}).await;
    w.set_flag("ci_always_red");
    let t = ticket(&w, "T-plain").await;
    let results = drive(&w).await;
    assert_eq!(state(&w.pool, t).await, "failed");
    assert!(results.contains(&RunResult::GaveUp));
    let err: String = sqlx::query_scalar("SELECT last_error FROM tickets WHERE id = $1")
        .bind(t)
        .fetch_one(&w.pool)
        .await
        .unwrap();
    assert!(err.contains("CI failed"), "{err}");
    assert!(w.read_state("comments.log").contains("needs a person"));
    assert_eq!(
        count(
            &w.pool,
            "SELECT count(*) FROM jobs WHERE status NOT IN ('done','failed')"
        )
        .await,
        0
    );
}

#[tokio::test]
async fn pending_ci_is_polled_without_using_up_attempts() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let w = world(pool, |_| {}).await;
    w.arm("ci_pending");
    let t = ticket(&w, "T-plain").await;
    let results = drive(&w).await;
    assert!(results.contains(&RunResult::Waiting));
    assert_eq!(state(&w.pool, t).await, "merged");
}

#[tokio::test]
async fn a_reviewer_who_asks_for_changes_sends_the_ticket_back_with_the_notes() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let w = world(pool, |_| {}).await;
    w.arm("changes");
    let t = ticket(&w, "T-changes").await;
    drive(&w).await;
    assert_eq!(state(&w.pool, t).await, "merged");
    assert!(w.read_state("dev_calls.log").contains("please add a test"));
}

#[tokio::test]
async fn a_reviewer_escalation_needs_approval_and_then_resumes_at_the_merge() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let w = world(pool, |_| {}).await;
    add_principal(&w.pool, "alice", "team_approver").await;
    w.arm("escalate");
    let t = ticket(&w, "T-escalate").await;
    drive(&w).await;
    assert_eq!(state(&w.pool, t).await, "awaiting_approval");
    let hash = plan_hash(&w, t).await;
    decide(
        &w.pool,
        t,
        &console("alice"),
        "console",
        Decision::Approve,
        &hash,
    )
    .await
    .unwrap();
    drive(&w).await;
    assert_eq!(state(&w.pool, t).await, "merged");
    assert_eq!(
        w.read_state("dev_calls.log").lines().count(),
        1,
        "no second development run"
    );
}

#[tokio::test]
async fn a_head_that_moved_before_the_merge_is_not_merged_until_it_is_checked_and_reviewed_again() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let w = world(pool, |_| {}).await;
    w.arm("head_moves");
    let t = ticket(&w, "T-plain").await;
    drive(&w).await;
    assert_eq!(state(&w.pool, t).await, "merged");
    let actions = audit_actions(&w.pool, t).await;
    assert!(
        actions.contains(&"ticket.merge_refused".to_string()),
        "{actions:?}"
    );
    assert_eq!(
        actions.iter().filter(|a| *a == "ticket.reviewed").count(),
        2,
        "reviewed again after the refusal"
    );
}

#[tokio::test]
async fn an_agent_that_changes_nothing_does_not_produce_a_pull_request() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let w = world(pool, |_| {}).await;
    let t = ticket(&w, "T-nochange").await;
    for _ in 0..3 {
        // retries wait out their backoff; skip the wait
        sqlx::query("UPDATE jobs SET run_after = now() WHERE status = 'queued'")
            .execute(&w.pool)
            .await
            .unwrap();
        drive(&w).await;
    }
    assert_eq!(
        state(&w.pool, t).await,
        "failed",
        "after the attempts ran out"
    );
    assert!(!audit_actions(&w.pool, t)
        .await
        .contains(&"ticket.pr_opened".to_string()));
}

#[tokio::test]
async fn only_so_many_development_runs_start_at_once() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let w = world(pool, |_| {}).await;
    let a = ticket(&w, "T-plain").await;
    w.orch.run_one().await.unwrap(); // triage -> queued, develop job
                                     // pretend another worker is already developing something else
    let (other,): (i64,) = sqlx::query_as("INSERT INTO tickets (service_id, external_ref) SELECT id, 'T-other' FROM services LIMIT 1 RETURNING id")
        .fetch_one(&w.pool).await.unwrap();
    sqlx::query("INSERT INTO jobs (ticket_id, kind, status, lease_owner, lease_until, fence) VALUES ($1,'develop','running','someone', now() + interval '1 hour', 1)")
        .bind(other).execute(&w.pool).await.unwrap();
    assert_eq!(w.orch.run_one().await.unwrap(), RunResult::Waiting);
    assert_eq!(state(&w.pool, a).await, "queued", "not started");
    assert_eq!(count(&w.pool, "SELECT count(*) FROM jobs WHERE ticket_id = (SELECT id FROM tickets WHERE external_ref = 'T-plain') AND kind = 'develop' AND attempts = 0").await, 1, "waiting did not use an attempt");
}
