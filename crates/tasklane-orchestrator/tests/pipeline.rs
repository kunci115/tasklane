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
    assert!(
        calls
            .lines()
            .nth(1)
            .unwrap()
            .contains("FAILED test_login: expected 200, got 500"),
        "and what the failed check said: {calls}"
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
async fn when_every_model_account_is_used_up_the_job_waits_and_no_attempt_is_spent() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let w = world(pool, |_| {}).await;
    w.arm("used_up");
    let t = ticket(&w, "T-usedup").await;
    let first = w.orch.run_one().await.unwrap();
    assert_eq!(first, RunResult::Waiting, "deferred, not retried or failed");
    assert_ne!(state(&w.pool, t).await, "failed");
    let (attempts, wait): (i16, f64) = sqlx::query_as(
        "SELECT attempts, extract(epoch FROM run_after - now())::float8 FROM jobs WHERE ticket_id = $1 AND kind = 'triage'",
    )
    .bind(t)
    .fetch_one(&w.pool)
    .await
    .unwrap();
    assert_eq!(attempts, 0, "waiting for an account costs no attempt");
    assert!(
        (30.0..=61.0).contains(&wait),
        "waits as long as the plug-in said (60 s): {wait}"
    );
    // the account comes back: the same job runs and the ticket goes on
    sqlx::query("UPDATE jobs SET run_after = now() WHERE ticket_id = $1")
        .bind(t)
        .execute(&w.pool)
        .await
        .unwrap();
    drive(&w).await;
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
async fn a_merged_ticket_closes_its_issue_only_once_the_change_is_in_the_default_branch() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let w = world(pool, |_| {}).await;
    sqlx::query("UPDATE services SET agent_profile = '{\"target_branch\":\"dev\"}'")
        .execute(&w.pool)
        .await
        .unwrap();
    assert!(std::process::Command::new("git")
        .args(["branch", "dev", "main"])
        .current_dir(w.remote())
        .status()
        .unwrap()
        .success());
    let t = ticket(&w, "T-plain").await;
    drive(&w).await;
    assert_eq!(state(&w.pool, t).await, "merged");
    let stored: Option<String> =
        sqlx::query_scalar("SELECT base_branch FROM tickets WHERE id = $1")
            .bind(t)
            .fetch_one(&w.pool)
            .await
            .unwrap();
    assert_eq!(
        stored.as_deref(),
        Some("dev"),
        "the base is fixed when development starts"
    );
    let said = w.read_state("comments.log");
    assert!(
        said.contains("stays open until the change reaches `main`"),
        "{said}"
    );
    std::fs::write(
        w.state_dir().join("services.json"),
        r#"[{"external_id":"org/repo","default_branch":"main","archived":false,"private":true}]"#,
    )
    .unwrap();
    w.orch.sync_once().await.unwrap();
    assert_eq!(state(&w.pool, t).await, "merged", "not in main yet");
    assert!(w.read_state("closed.log").is_empty());
    w.set_flag("in_branch");
    w.orch.sync_once().await.unwrap();
    assert_eq!(state(&w.pool, t).await, "closed_done");
    assert_eq!(w.read_state("closed.log").trim(), "T-plain");
    assert!(w.read_state("comments.log").contains("Closed: the change"));
    w.orch.sync_once().await.unwrap();
    assert_eq!(w.read_state("closed.log").lines().count(), 1, "closed once");
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

// ---- who gets the credit in the history

async fn credit_person(pool: &sqlx::PgPool, channel: &str, who: &str, login: &str, credit: bool) {
    sqlx::query("INSERT INTO principals (channel, ref, role, github_login, credit_coauthor) VALUES ($1, $2, 'owner', $3, $4) ON CONFLICT (channel, ref) DO UPDATE SET github_login = $3, credit_coauthor = $4")
        .bind(channel)
        .bind(who)
        .bind(login)
        .bind(credit)
        .execute(pool)
        .await
        .unwrap();
}

/// author name | author email | committer name | message, of the pushed branch
fn pushed_commit(w: &World, t: i64) -> (String, String, String, String) {
    let out = sh(
        &w.remote(),
        &format!("git log -1 --format='%an|%ae|%cn|%B' tasklane/{t}"),
    );
    let mut parts = out.splitn(4, '|');
    let mut next = || parts.next().unwrap().trim().to_string();
    (next(), next(), next(), next())
}

#[tokio::test]
async fn the_person_who_approved_is_the_author_and_the_requester_is_a_co_author() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let w = world(pool, |_| {}).await;
    credit_person(&w.pool, "console", "boss", "rino", true).await;
    credit_person(&w.pool, "github", "nabil", "", true).await;
    let t = ticket(&w, "T-l3-credit").await;
    drive(&w).await;
    assert_eq!(state(&w.pool, t).await, "awaiting_approval");
    let hash = plan_hash(&w, t).await;
    decide(
        &w.pool,
        t,
        &console("boss"),
        "console",
        Decision::Approve,
        &hash,
    )
    .await
    .unwrap();
    drive(&w).await;
    assert_eq!(state(&w.pool, t).await, "merged");
    let (an, ae, cn, msg) = pushed_commit(&w, t);
    assert_eq!(
        (an.as_str(), ae.as_str()),
        ("rino", "777+rino@users.noreply.github.com")
    );
    assert_eq!(cn, "tasklane", "tasklane stays the committer");
    for line in [
        "Requested-by: nabil",
        "Approved-by: rino",
        "Co-authored-by: nabil <4242+nabil@users.noreply.github.com>",
    ] {
        assert!(msg.contains(line), "{line} missing in {msg}");
    }
    let id: Option<i64> =
        sqlx::query_scalar("SELECT github_id FROM principals WHERE github_login = 'rino'")
            .fetch_one(&w.pool)
            .await
            .unwrap();
    assert_eq!(id, Some(777), "the id is looked up once and remembered");
    let body = w.read_state("pr_body");
    assert!(
        body.contains("Requested by nabil. Approved by rino. Credited to rino"),
        "{body}"
    );
}

#[tokio::test]
async fn nobody_is_credited_as_an_author_unless_they_turned_it_on() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let w = world(pool, |_| {}).await;
    credit_person(&w.pool, "github", "nabil", "", false).await;
    let t = ticket(&w, "T-credit").await;
    drive(&w).await;
    assert_eq!(state(&w.pool, t).await, "merged");
    let (an, _, _, msg) = pushed_commit(&w, t);
    assert_eq!(an, "tasklane");
    assert!(msg.contains("Requested-by: nabil"), "{msg}");
    assert!(!msg.contains("Co-authored-by"), "{msg}");
    assert!(!w.read_state("pr_body").contains("Credited to"));
}

#[tokio::test]
async fn a_requester_who_opted_in_is_the_author_when_nobody_had_to_approve() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let w = world(pool, |_| {}).await;
    credit_person(&w.pool, "github", "nabil", "", true).await;
    let t = ticket(&w, "T-credit").await;
    drive(&w).await;
    let (an, ae, cn, msg) = pushed_commit(&w, t);
    assert_eq!(
        (an.as_str(), ae.as_str(), cn.as_str()),
        ("nabil", "4242+nabil@users.noreply.github.com", "tasklane")
    );
    assert!(
        !msg.contains("Co-authored-by"),
        "no duplicate credit: {msg}"
    );
}

#[tokio::test]
async fn attribution_can_be_switched_off_and_a_missing_id_only_costs_the_credit() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let w = world(pool, |c| c.attribution = false).await;
    credit_person(&w.pool, "github", "nabil", "", true).await;
    let t = ticket(&w, "T-credit").await;
    drive(&w).await;
    let (an, _, _, msg) = pushed_commit(&w, t);
    assert_eq!(an, "tasklane");
    assert_eq!(msg, "tasklane: Fix T-credit (T-credit)");

    // the source cannot look the person up: the ticket still goes through, without credit
    let w = world(test_pool().await.unwrap(), |_| {}).await;
    w.set_flag("no_user_lookup");
    credit_person(&w.pool, "github", "nabil", "", true).await;
    let t = ticket(&w, "T-credit").await;
    drive(&w).await;
    assert_eq!(state(&w.pool, t).await, "merged");
    let (an, _, _, msg) = pushed_commit(&w, t);
    assert_eq!(an, "tasklane");
    assert!(msg.contains("Requested-by: nabil"), "{msg}");
}

#[tokio::test]
async fn a_pull_request_somebody_opened_gets_a_read_only_review_posted_as_a_comment() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let w = world(pool, |_| {}).await;
    // a pull request as GitHub exposes it: the head lives under refs/pull/<n>/head, not a branch
    let seed = w.dir.path().join("seed");
    sh(
        &seed,
        &format!(
            "git checkout -q -b feature && echo more >> README && git commit -qam change && git push -q {0} feature:refs/pull/7/head && git checkout -q main",
            w.remote().display()
        ),
    );
    let r = w.orch.review_pull("org/repo", "7", "tester").await.unwrap();
    assert_eq!(
        (r.verdict.as_str(), r.notes.as_str()),
        ("approve", "looks fine")
    );
    let comments = w.read_state("comments.log");
    assert_eq!(r.complexity, "O(n) in the changed lines");
    assert!(
        read_state_contains(&w, "comments.log", "Complexity (Big-O)"),
        "the comment carries the Big-O analysis"
    );
    assert!(
        comments.contains("\"ref\": \"7\"")
            && comments.contains("looks fine")
            && comments.contains("tasklane"),
        "{comments}"
    );
    let n: i64 =
        sqlx::query_scalar("SELECT count(*) FROM audit_events WHERE action = 'pull.reviewed'")
            .fetch_one(&w.pool)
            .await
            .unwrap();
    assert_eq!(n, 1);
    // watch mode means nothing happens, and a bad number never reaches git
    sqlx::query("UPDATE services SET mode = 'watch'")
        .execute(&w.pool)
        .await
        .unwrap();
    assert!(w.orch.review_pull("org/repo", "7", "tester").await.is_err());
    sqlx::query("UPDATE services SET mode = 'triage'")
        .execute(&w.pool)
        .await
        .unwrap();
    assert!(w
        .orch
        .review_pull("org/repo", "7; rm", "tester")
        .await
        .is_err());
}

#[tokio::test]
async fn a_pull_request_is_reviewed_against_the_branch_it_goes_into_not_the_default_branch() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let w = world(pool, |_| {}).await;
    let seed = w.dir.path().join("seed");
    // `dev` is ahead of `main` with work from other tickets; the pull request is built on `dev`
    sh(
        &seed,
        &format!(
            "git checkout -q -b dev && echo other-ticket > OTHER && git add . && git commit -qm other && git push -q {0} dev && git checkout -q -b feature && echo mine >> README && git commit -qam mine && git push -q {0} feature:refs/pull/8/head && git checkout -q main",
            w.remote().display()
        ),
    );
    std::fs::write(
        w.state_dir().join("pull_8.json"),
        r#"{"base":"dev","state":"open"}"#,
    )
    .unwrap();
    w.orch.review_pull("org/repo", "8", "tester").await.unwrap();
    let seen = w.read_state("review_diffs.log");
    assert!(
        seen.contains("mine") && !seen.contains("other-ticket"),
        "{seen}"
    );
    std::fs::write(
        w.state_dir().join("pull_9.json"),
        r#"{"base":"-x","state":"open"}"#,
    )
    .unwrap();
    assert!(w.orch.review_pull("org/repo", "9", "tester").await.is_err());
    std::fs::write(
        w.state_dir().join("pull_10.json"),
        r#"{"base":"dev","state":"closed"}"#,
    )
    .unwrap();
    assert!(w
        .orch
        .review_pull("org/repo", "10", "tester")
        .await
        .is_err());
}

#[tokio::test]
async fn an_approved_pull_request_is_merged_from_the_console_only_under_every_condition() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let w = world(pool, |_| {}).await;
    let seed = w.dir.path().join("seed");
    sh(
        &seed,
        &format!(
            "git checkout -q -b dev && git push -q {0} dev && git checkout -q -b feature && echo mine >> README && git commit -qam mine && git push -q {0} feature:refs/pull/8/head && git checkout -q main",
            w.remote().display()
        ),
    );
    let pull = |base: &str, head: &str| {
        std::fs::write(
            w.state_dir().join("pull_8.json"),
            format!(r#"{{"base":"{base}","state":"open","head_sha":"{head}"}}"#),
        )
        .unwrap();
    };
    pull("dev", "aaa");
    // no review yet, and the switch is off: both refuse
    assert!(w.orch.merge_pull("org/repo", "8", "tester").await.is_err());
    w.orch.review_pull("org/repo", "8", "tester").await.unwrap();
    let off = w
        .orch
        .merge_pull("org/repo", "8", "tester")
        .await
        .unwrap_err();
    assert!(off.to_string().contains("off for this repository"), "{off}");
    sqlx::query("UPDATE services SET agent_profile = agent_profile || '{\"console_merge\": true}'")
        .execute(&w.pool)
        .await
        .unwrap();
    // the head moved after the review
    pull("dev", "bbb");
    let moved = w
        .orch
        .merge_pull("org/repo", "8", "tester")
        .await
        .unwrap_err();
    assert!(moved.to_string().contains("exact commit"), "{moved}");
    // into the default branch: a person's job
    pull("main", "aaa");
    let prod = w
        .orch
        .merge_pull("org/repo", "8", "tester")
        .await
        .unwrap_err();
    assert!(prod.to_string().contains("default branch"), "{prod}");
    // GitHub refusing (checks failing) is reported, not hidden
    pull("dev", "aaa");
    w.set_flag("merge_refuse");
    let refused = w
        .orch
        .merge_pull("org/repo", "8", "tester")
        .await
        .unwrap_err();
    assert!(refused.to_string().contains("checks_failing"), "{refused}");
    std::fs::remove_file(w.state_dir().join("merge_refuse")).unwrap();
    // a stranger's or a fork's pull request is for a person, however the AI voted
    for extra in [r#""author_trusted":false"#, r#""from_fork":true"#] {
        std::fs::write(
            w.state_dir().join("pull_8.json"),
            format!(r#"{{"base":"dev","state":"open","head_sha":"aaa",{extra}}}"#),
        )
        .unwrap();
        let e = w
            .orch
            .merge_pull("org/repo", "8", "tester")
            .await
            .unwrap_err();
        assert!(e.to_string().contains("a person merges it"), "{e}");
    }
    pull("dev", "aaa");
    w.orch.merge_pull("org/repo", "8", "tester").await.unwrap();
    assert!(w.read_state("merged").contains("contents:write"));
    let acts: Vec<String> = sqlx::query_scalar(
        "SELECT action FROM audit_events WHERE action LIKE 'pull.merge%' ORDER BY id",
    )
    .fetch_all(&w.pool)
    .await
    .unwrap();
    assert_eq!(acts, vec!["pull.merge_refused", "pull.merged"]);
    // watch or triage mode never merges
    sqlx::query("UPDATE services SET mode = 'triage'")
        .execute(&w.pool)
        .await
        .unwrap();
    assert!(w.orch.merge_pull("org/repo", "8", "tester").await.is_err());
}

#[tokio::test]
async fn a_pull_request_that_touches_protected_paths_is_not_merged_from_the_console() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let w = world(pool, |_| {}).await;
    let seed = w.dir.path().join("seed");
    sh(
        &seed,
        &format!(
            "git checkout -q -b dev && git push -q {0} dev && git checkout -q -b feature && mkdir -p .github/workflows && echo x > .github/workflows/ci.yml && git add . && git commit -qm ci && git push -q {0} feature:refs/pull/11/head && git checkout -q main",
            w.remote().display()
        ),
    );
    std::fs::write(
        w.state_dir().join("pull_11.json"),
        r#"{"base":"dev","state":"open","head_sha":"ccc"}"#,
    )
    .unwrap();
    sqlx::query("UPDATE services SET agent_profile = agent_profile || '{\"console_merge\": true}'")
        .execute(&w.pool)
        .await
        .unwrap();
    w.orch
        .review_pull("org/repo", "11", "tester")
        .await
        .unwrap();
    let e = w
        .orch
        .merge_pull("org/repo", "11", "tester")
        .await
        .unwrap_err();
    assert!(
        e.to_string().contains("a person must merge this one"),
        "{e}"
    );
    assert!(!w.state_dir().join("merged").exists());
}

#[tokio::test]
async fn merged_tickets_waiting_for_the_default_branch_do_not_starve_the_newer_ones() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let w = world(pool, |_| {}).await;
    for n in 1..=25 {
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO tickets (service_id, external_ref, state) SELECT id, $1, 'merged' FROM services RETURNING id",
        )
        .bind(format!("m{n}"))
        .fetch_one(&w.pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO audit_events (actor, action, ticket_id, detail) VALUES ('t', 'ticket.merged', $1, $2)")
            .bind(id)
            .bind(serde_json::json!({"merge_sha": format!("sha{n}"), "closes_on": "main"}))
            .execute(&w.pool)
            .await
            .unwrap();
    }
    std::fs::write(
        w.state_dir().join("services.json"),
        r#"[{"external_id":"org/repo","default_branch":"main","archived":false,"private":true}]"#,
    )
    .unwrap();
    // only the five newest reached main
    std::fs::write(
        w.state_dir().join("in_main.json"),
        r#"["sha21","sha22","sha23","sha24","sha25"]"#,
    )
    .unwrap();
    w.orch.sync_once().await.unwrap();
    w.orch.sync_once().await.unwrap();
    let closed: i64 =
        sqlx::query_scalar("SELECT count(*) FROM tickets WHERE state = 'closed_done'")
            .fetch_one(&w.pool)
            .await
            .unwrap();
    assert_eq!(
        closed, 5,
        "the second pass reaches the tickets the first one left"
    );
}

#[tokio::test]
async fn an_issue_that_cannot_be_closed_stays_merged_and_gets_no_closing_note() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let w = world(pool, |_| {}).await;
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO tickets (service_id, external_ref, state) SELECT id, 'x1', 'merged' FROM services RETURNING id",
    )
    .fetch_one(&w.pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO audit_events (actor, action, ticket_id, detail) VALUES ('t', 'ticket.merged', $1, $2)")
        .bind(id)
        .bind(serde_json::json!({"merge_sha": "sha1", "closes_on": "main"}))
        .execute(&w.pool)
        .await
        .unwrap();
    std::fs::write(
        w.state_dir().join("services.json"),
        r#"[{"external_id":"org/repo","default_branch":"main","archived":false,"private":true}]"#,
    )
    .unwrap();
    w.set_flag("in_branch");
    w.set_flag("close_fail");
    w.orch.sync_once().await.unwrap();
    assert_eq!(state(&w.pool, id).await, "merged");
    assert!(!w.read_state("comments.log").contains("Closed:"));
}

#[tokio::test]
async fn work_runs_on_the_account_of_the_person_who_approved_it_and_never_on_anothers() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let w = world(pool, |_| {}).await;
    credit_person(&w.pool, "console", "boss", "rino", true).await;
    let t = ticket(&w, "T-l3-owner").await;
    drive(&w).await;
    let hash = plan_hash(&w, t).await;
    decide(
        &w.pool,
        t,
        &console("boss"),
        "console",
        Decision::Approve,
        &hash,
    )
    .await
    .unwrap();
    drive(&w).await;
    assert_eq!(state(&w.pool, t).await, "merged");
    let owners: Vec<serde_json::Value> = w
        .read_state("profile.log")
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .collect();
    let dev = owners.iter().find(|o| o["role"] == "dev").unwrap();
    assert_eq!(
        dev["owner"], "console:boss",
        "the developer works on the approver's account"
    );
    let triage = owners.iter().find(|o| o["role"] == "triage").unwrap();
    assert!(
        triage["owner"].is_null(),
        "before anyone approved, only shared accounts"
    );
}

#[tokio::test]
async fn before_anyone_approves_the_work_of_a_known_issue_author_uses_their_account() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let w = world(pool, |_| {}).await;
    // the fake source says "nabil" opened any issue whose ref contains "credit"
    sqlx::query("INSERT INTO principals (channel, ref, role) VALUES ('github', 'nabil', 'member')")
        .execute(&w.pool)
        .await
        .unwrap();
    let t = ticket(&w, "T-credit-own").await;
    drive(&w).await;
    assert_eq!(state(&w.pool, t).await, "merged");
    let first_triage = w
        .read_state("profile.log")
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .find(|o| o["role"] == "triage")
        .unwrap();
    assert_eq!(first_triage["owner"], "github:nabil");
}

#[tokio::test]
async fn reference_documents_reach_the_agents_are_cached_and_survive_a_failing_address() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let w = world(pool, |_| {}).await;
    sqlx::query("INSERT INTO reference_sources (name, url) VALUES ('Everywhere', 'https://wiki.example.com/all')")
        .execute(&w.pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO reference_sources (name, url, service_id) SELECT 'Only here', 'https://wiki.example.com/repo', id FROM services")
        .execute(&w.pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO reference_sources (name, url, enabled) VALUES ('Off', 'https://wiki.example.com/off', false)")
        .execute(&w.pool)
        .await
        .unwrap();
    let t = ticket(&w, "T-plain").await;
    drive(&w).await;
    assert_eq!(state(&w.pool, t).await, "merged");
    let runs: Vec<serde_json::Value> = w
        .read_state("profile.log")
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    for role in ["triage", "dev", "reviewer"] {
        let r = runs.iter().find(|o| o["role"] == role).unwrap();
        assert_eq!(
            r["references"],
            serde_json::json!(["Everywhere", "Only here"]),
            "{role}"
        );
    }
    // each address was fetched once: the text is kept for its lifetime
    let fetched = w.read_state("references.log");
    assert_eq!(fetched.matches("/all").count(), 1, "{fetched}");
    assert_eq!(
        fetched.matches("/off").count(),
        0,
        "a source that is off is never read"
    );
    // an address that fails later falls back to the last text it gave
    sqlx::query("UPDATE reference_sources SET url = url || '-fail', last_fetched = now() - interval '1 day' WHERE name = 'Everywhere'")
        .execute(&w.pool)
        .await
        .unwrap();
    let refs = w.orch.references_for("org/repo").await;
    let every = refs.iter().find(|r| r["name"] == "Everywhere").unwrap();
    assert!(every["text"]
        .as_str()
        .unwrap()
        .contains("Doc from https://wiki.example.com/all"));
    let status: String =
        sqlx::query_scalar("SELECT last_status FROM reference_sources WHERE name = 'Everywhere'")
            .fetch_one(&w.pool)
            .await
            .unwrap();
    assert!(status.starts_with("failed"), "{status}");
}

#[tokio::test]
async fn tools_reach_only_the_roles_and_repositories_they_were_granted_for_and_their_use_is_counted(
) {
    let Some(pool) = test_pool().await else {
        return;
    };
    let w = world(pool, |_| {}).await;
    std::fs::create_dir_all(w.dir.path().join("data/secrets")).unwrap();
    // global for triage; this repository's own for reviewer (with a credential); one that is off;
    // and one for another repository
    sqlx::query("INSERT INTO agent_tools (name, base_url, roles) VALUES ('wiki', 'https://wiki.example.com', '{triage}')")
        .execute(&w.pool)
        .await
        .unwrap();
    let id: i64 = sqlx::query_scalar("INSERT INTO agent_tools (name, base_url, roles, header_name, has_secret, service_id) SELECT 'db_schema', 'https://api.example.com/v1', '{reviewer}', 'Authorization', true, id FROM services RETURNING id")
        .fetch_one(&w.pool)
        .await
        .unwrap();
    std::fs::write(
        w.dir.path().join(format!("data/secrets/tool_{id}")),
        "Bearer s3\n",
    )
    .unwrap();
    sqlx::query("INSERT INTO agent_tools (name, base_url, roles, enabled) VALUES ('off_one', 'https://x.example.com', '{triage,dev,reviewer}', false)")
        .execute(&w.pool)
        .await
        .unwrap();
    let other: i64 = sqlx::query_scalar("INSERT INTO services (kind, source, external_id, mode) VALUES ('repo','test','org/other','full') RETURNING id")
        .fetch_one(&w.pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO agent_tools (name, base_url, roles, service_id) VALUES ('elsewhere', 'https://z.example.com', '{triage,dev,reviewer}', $1)")
        .bind(other)
        .execute(&w.pool)
        .await
        .unwrap();
    let t = ticket(&w, "T-plain").await;
    drive(&w).await;
    assert_eq!(state(&w.pool, t).await, "merged");
    let runs: Vec<serde_json::Value> = w
        .read_state("profile.log")
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    let names = |role: &str| -> Vec<String> {
        runs.iter().find(|o| o["role"] == role).unwrap()["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect()
    };
    assert_eq!(names("triage"), vec!["wiki"]);
    assert_eq!(names("reviewer"), vec!["db_schema"]);
    assert!(names("dev").is_empty(), "the developer was granted nothing");
    let reviewer = runs.iter().find(|o| o["role"] == "reviewer").unwrap();
    assert_eq!(
        reviewer["tools"][0]["header"], "Bearer s3",
        "the engine's broker gets the credential"
    );
    let calls: i64 =
        sqlx::query_scalar("SELECT calls_total FROM agent_tools WHERE name = 'db_schema'")
            .fetch_one(&w.pool)
            .await
            .unwrap();
    assert_eq!(calls, 2, "the engine's report of calls is counted");
    // a repository's own tool of the same name replaces the global one
    sqlx::query("INSERT INTO agent_tools (name, base_url, roles, service_id) SELECT 'wiki', 'https://own.example.com', '{triage}', id FROM services WHERE external_id = 'org/repo'")
        .execute(&w.pool)
        .await
        .unwrap();
    let granted = w.orch.tools_for("org/repo", "triage").await;
    assert_eq!(granted.len(), 1);
    assert_eq!(granted[0]["base_url"], "https://own.example.com");
}

fn read_state_contains(w: &World, name: &str, needle: &str) -> bool {
    w.read_state(name).contains(needle)
}
