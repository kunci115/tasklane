//! Regression tests for the findings of the independent review.

mod common;
use common::*;

use std::time::Duration;

use tasklane_orchestrator::approval::{
    decide, resume_kind, retry_or_close, ApprovalError, Decision, Principal,
};
use tasklane_orchestrator::host::{PluginClient, PluginSpec};
use tasklane_orchestrator::workspace::Workspaces;
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

// ---- 1. the protected-path rule cannot be sidestepped ----

#[tokio::test]
async fn a_non_ascii_name_under_a_protected_directory_is_still_protected() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let w = world(pool, |_| {}).await;
    let t = ticket(&w, "T-unicode").await;
    drive(&w).await;
    assert_eq!(
        state(&w.pool, t).await,
        "awaiting_approval",
        "git would have quoted this name and hidden it from a prefix check"
    );
    assert!(w.read_state("comments.log").contains("protected paths"));
}

#[tokio::test]
async fn moving_a_workflow_file_away_is_seen_through_its_old_name() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let w = world(pool, |_| {}).await;
    let t = ticket(&w, "T-moveaway").await;
    drive(&w).await;
    assert_eq!(state(&w.pool, t).await, "awaiting_approval");
}

#[tokio::test]
async fn a_migration_in_a_nested_directory_is_protected_even_with_no_list_for_the_repository() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let w = world(pool, |_| {}).await;
    sqlx::query("UPDATE services SET facts = '{}'")
        .execute(&w.pool)
        .await
        .unwrap(); // detection never ran
    let t = ticket(&w, "T-nested").await;
    drive(&w).await;
    assert_eq!(state(&w.pool, t).await, "awaiting_approval");
}

// ---- 2. an approval covers what the approver saw ----

#[tokio::test]
async fn a_plan_approved_at_level_two_does_not_cover_a_diff_that_touches_protected_paths() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let w = world(pool, |_| {}).await;
    add_principal(&w.pool, "alice", "team_approver").await;
    let t = ticket(&w, "T-risky-protected").await; // text rules give level 2; the dev run also edits a workflow
    drive(&w).await;
    assert_eq!(state(&w.pool, t).await, "awaiting_approval");
    let plan = plan_hash(&w, t).await;
    decide(
        &w.pool,
        t,
        &console("alice"),
        "console",
        Decision::Approve,
        &plan,
    )
    .await
    .unwrap();
    drive(&w).await;
    // the first approval covered the plan; the developed diff needs its own
    assert_eq!(
        state(&w.pool, t).await,
        "awaiting_approval",
        "asked again, for the diff"
    );
    let diff_hash = plan_hash(&w, t).await;
    assert_ne!(diff_hash, plan);
    let diff: String = sqlx::query_scalar("SELECT diff_text FROM tickets WHERE id = $1")
        .bind(t)
        .fetch_one(&w.pool)
        .await
        .unwrap();
    assert!(
        diff.contains(".github/workflows/ci.yml"),
        "the approver can see the change: {diff}"
    );
    let plan_text: String = sqlx::query_scalar("SELECT plan_text FROM tickets WHERE id = $1")
        .bind(t)
        .fetch_one(&w.pool)
        .await
        .unwrap();
    assert!(!plan_text.is_empty());
    // the old approval cannot be replayed for the diff
    assert!(matches!(
        decide(
            &w.pool,
            t,
            &console("alice"),
            "console",
            Decision::Approve,
            &plan
        )
        .await,
        Err(ApprovalError::StalePlan)
    ));
    decide(
        &w.pool,
        t,
        &console("alice"),
        "console",
        Decision::Approve,
        &diff_hash,
    )
    .await
    .unwrap();
    drive(&w).await;
    assert_eq!(state(&w.pool, t).await, "merged");
}

// ---- 5. the text can change after approval; strangers do not start unattended work ----

#[tokio::test]
async fn text_added_after_approval_is_scored_again_before_the_agent_reads_it() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let w = world(pool, |_| {}).await;
    add_principal(&w.pool, "boss", "owner").await;
    let t = ticket(&w, "T-plain").await;
    w.orch.run_one().await.unwrap(); // triage: level 1, queued, develop job waiting
    assert_eq!(state(&w.pool, t).await, "queued");
    // someone comments after triage
    std::fs::write(
        w.state_dir().join("comments_T-plain.json"),
        r#"[{"author":"x","body":"also rotate the api key and the credential","at":"t"}]"#,
    )
    .unwrap();
    assert_eq!(
        w.orch.run_one().await.unwrap(),
        RunResult::Moved {
            ticket_id: t,
            to: tasklane_core::State::AwaitingApproval
        }
    );
    assert_eq!(level(&w.pool, t).await, Some(3));
    assert!(
        !w.state_dir().join("dev_calls.log").exists(),
        "the agent never saw the new text"
    );
}

#[tokio::test]
async fn a_stranger_s_issue_always_waits_for_a_person_even_in_a_full_mode_repository_at_level_one()
{
    let Some(pool) = test_pool().await else {
        return;
    };
    let w = world(pool, |_| {}).await;
    let t = ticket(&w, "T-stranger").await;
    drive(&w).await;
    assert_eq!(state(&w.pool, t).await, "awaiting_approval");
    let why: serde_json::Value =
        sqlx::query_scalar("SELECT level_reasons FROM tickets WHERE id = $1")
            .bind(t)
            .fetch_one(&w.pool)
            .await
            .unwrap();
    assert!(why.to_string().contains("not a collaborator"));
}

// ---- 6. pull requests and closed issues are not tickets ----

#[tokio::test]
async fn events_about_pull_requests_and_closed_issues_create_no_ticket() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let w = world(pool, |_| {}).await;
    use tasklane_orchestrator::sync::Noted;
    assert_eq!(
        w.orch.note_issue("org/repo", "pr-5").await.unwrap(),
        Noted::Nothing
    );
    assert_eq!(
        w.orch.note_issue("org/repo", "closed-6").await.unwrap(),
        Noted::Nothing
    );
    assert_eq!(count(&w.pool, "SELECT count(*) FROM tickets").await, 0);
    assert_eq!(
        w.orch.note_issue("org/repo", "7").await.unwrap(),
        Noted::Created
    );
}

// ---- 7/8. re-development updates the existing pull request; approvals pick the right job ----

#[test]
fn the_job_after_an_approval_depends_on_what_was_pushed() {
    assert_eq!(resume_kind(None, None, None, None), "develop");
    assert_eq!(
        resume_kind(Some("b"), None, Some("h1"), None),
        "pr",
        "local work must be pushed first"
    );
    assert_eq!(
        resume_kind(Some("b"), Some("7"), Some("h2"), Some("h1")),
        "pr",
        "a re-developed head is not what is on the pull request"
    );
    assert_eq!(
        resume_kind(Some("b"), Some("7"), Some("h1"), Some("h1")),
        "merge",
        "only a pushed, unchanged head is merged"
    );
}

#[tokio::test]
async fn a_re_developed_ticket_updates_the_pull_request_instead_of_opening_a_second_one() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let w = world(pool, |_| {}).await;
    w.arm("changes"); // the reviewer asks for changes once, so development runs twice on one PR
    let t = ticket(&w, "T-changes").await;
    drive(&w).await;
    assert_eq!(
        state(&w.pool, t).await,
        "merged",
        "the fake source refuses a second pull request, as GitHub does"
    );
    let pushed: Option<String> =
        sqlx::query_scalar("SELECT pushed_head FROM tickets WHERE id = $1")
            .bind(t)
            .fetch_one(&w.pool)
            .await
            .unwrap();
    assert!(pushed.is_some());
}

// ---- 9. a ticket whose job the reaper gave up on does not sit forever ----

#[tokio::test]
async fn a_ticket_whose_worker_kept_dying_is_failed_and_a_person_is_told() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let w = world(pool, |_| {}).await;
    let t = ticket(&w, "T-plain").await;
    w.orch.run_one().await.unwrap(); // -> queued with a develop job
    sqlx::query("UPDATE tickets SET state = 'developing' WHERE id = $1")
        .bind(t)
        .execute(&w.pool)
        .await
        .unwrap();
    // the develop job was claimed three times (the maximum here is two) and the worker died each time
    sqlx::query("UPDATE jobs SET status = 'running', attempts = 2, lease_owner = 'dead', lease_until = now() - interval '1 minute', fence = 2 WHERE kind = 'develop'")
        .execute(&w.pool).await.unwrap();
    let (report, dead) = w.queue.reap_detailed().await.unwrap();
    assert_eq!(report.failed, 1);
    for (job, ticket_id) in dead {
        w.orch
            .fail_orphaned(job, ticket_id, "the worker died or hung on every attempt")
            .await
            .unwrap();
    }
    assert_eq!(state(&w.pool, t).await, "failed");
    let comments = w.read_state("comments.log");
    assert!(
        comments.contains("needs a person") && !comments.contains("worker died"),
        "no internals on the public issue: {comments}"
    );
}

#[tokio::test]
async fn a_leftover_worktree_for_the_branch_does_not_block_the_next_run() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let w = world(pool, |_| {}).await;
    let t = ticket(&w, "T-plain").await;
    w.orch.run_one().await.unwrap();
    // simulate a crashed earlier run: a registered worktree already holds the branch
    let ws = Workspaces::new(w.dir.path().join("data"));
    let url = format!("file://{}", w.remote().display());
    let first = ws
        .prepare("org/repo", &url, "main", &format!("tasklane/{t}"), 900)
        .await
        .unwrap();
    assert!(first.worktree.exists());
    let second = ws
        .prepare("org/repo", &url, "main", &format!("tasklane/{t}"), 901)
        .await
        .unwrap();
    assert!(
        second.worktree.exists() && !first.worktree.exists(),
        "the stale worktree was cleared"
    );
}

// ---- 11. a dead plug-in comes back; an agent cannot make git run its code ----

#[tokio::test]
async fn a_plugin_that_died_is_started_again_on_the_next_call() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("started");
    let script = format!(
        "import sys,json,os\nopen({m:?},'a').write('x')\nfor l in sys.stdin:\n r=json.loads(l)\n print(json.dumps({{'id':r['id'],'ok':True,'result':{{'n':len(open({m:?}).read())}}}}),flush=True)\n os._exit(0)\n",
        m = marker.display().to_string()
    );
    let p = PluginClient::spawn(&PluginSpec {
        program: "python3".into(),
        args: vec!["-I".into(), "-c".into(), script],
        env: vec![("PATH".into(), std::env::var("PATH").unwrap())],
    })
    .unwrap();
    assert_eq!(
        p.call("x", serde_json::Value::Null, Duration::from_secs(5))
            .await
            .unwrap()["n"],
        1
    );
    tokio::time::sleep(Duration::from_millis(300)).await; // the process exits after answering
    let mut second = None;
    for _ in 0..3 {
        if let Ok(v) = p
            .call("x", serde_json::Value::Null, Duration::from_secs(5))
            .await
        {
            second = Some(v);
            break;
        }
    }
    assert_eq!(second.expect("restarted")["n"], 2, "a second process ran");
}

#[tokio::test]
async fn git_does_not_run_code_planted_in_the_agents_worktree() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let w = world(pool, |_| {}).await;
    let ws = Workspaces::new(w.dir.path().join("data"));
    let url = format!("file://{}", w.remote().display());
    let p = ws
        .prepare("org/repo", &url, "main", "tasklane/77", 1)
        .await
        .unwrap();
    // the agent replaces the worktree's .git file with a directory that names a program to run
    let marker = w.dir.path().join("pwned");
    std::fs::remove_file(p.worktree.join(".git")).unwrap();
    std::fs::create_dir_all(p.worktree.join(".git/hooks")).unwrap();
    std::fs::write(
        p.worktree.join(".git/config"),
        format!(
            "[core]\n\trepositoryformatversion = 0\n\tfsmonitor = touch {}\n\thooksPath = {0}\n",
            marker.display()
        ),
    )
    .unwrap();
    std::fs::write(p.worktree.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
    std::fs::write(p.worktree.join("new.txt"), "agent work\n").unwrap();
    // a hook the agent hopes will run
    std::fs::write(
        p.worktree.join(".git/hooks/pre-commit"),
        format!("#!/bin/sh\ntouch {}\n", marker.display()),
    )
    .unwrap();
    let committed = ws.commit_all(&p, "msg").await.unwrap();
    assert!(
        committed,
        "the commit still works, using the trusted git directory"
    );
    assert!(!marker.exists(), "no program named by the agent was run");
    assert!(sh_ok(&w.remote(), "true"));
    let head = ws.head("org/repo", "tasklane/77").await.unwrap();
    assert_eq!(head.len(), 40);
}

fn sh_ok(dir: &std::path::Path, cmd: &str) -> bool {
    std::process::Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .current_dir(dir)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

// ---- 14. watch mode stops work already in flight; failed tickets have a way out ----

#[tokio::test]
async fn switching_a_repository_to_watch_stops_the_work_already_queued() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let w = world(pool, |_| {}).await;
    let t = ticket(&w, "T-plain").await;
    w.orch.run_one().await.unwrap(); // queued, develop job waiting
    sqlx::query("UPDATE services SET mode = 'watch'")
        .execute(&w.pool)
        .await
        .unwrap();
    assert_eq!(w.orch.run_one().await.unwrap(), RunResult::Waiting);
    assert_eq!(state(&w.pool, t).await, "queued", "nothing was developed");
    assert!(!w.state_dir().join("dev_calls.log").exists());
    sqlx::query("UPDATE services SET mode = 'full'")
        .execute(&w.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE jobs SET run_after = now()")
        .execute(&w.pool)
        .await
        .unwrap();
    drive(&w).await;
    assert_eq!(state(&w.pool, t).await, "merged");
}

#[tokio::test]
async fn a_person_can_retry_or_close_a_failed_ticket_and_strangers_cannot() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let w = world(pool, |_| {}).await;
    add_principal(&w.pool, "boss", "owner").await;
    let t = ticket(&w, "T-plain").await;
    sqlx::query("UPDATE tickets SET state = 'failed', dev_attempts = 3, ci_runs = 2, last_error = 'x' WHERE id = $1").bind(t).execute(&w.pool).await.unwrap();
    sqlx::query("DELETE FROM jobs")
        .execute(&w.pool)
        .await
        .unwrap();
    assert!(matches!(
        retry_or_close(&w.pool, t, &console("nobody"), false).await,
        Err(ApprovalError::NotAllowed(_))
    ));
    assert_eq!(
        retry_or_close(&w.pool, t, &console("boss"), false)
            .await
            .unwrap()
            .as_str(),
        "queued"
    );
    assert_eq!(
        count(
            &w.pool,
            "SELECT count(*) FROM jobs WHERE kind = 'develop' AND status = 'queued'"
        )
        .await,
        1
    );
    let attempts: i16 = sqlx::query_scalar("SELECT dev_attempts FROM tickets WHERE id = $1")
        .bind(t)
        .fetch_one(&w.pool)
        .await
        .unwrap();
    assert_eq!(attempts, 0);
    assert!(
        matches!(
            retry_or_close(&w.pool, t, &console("boss"), false).await,
            Err(ApprovalError::WrongState(_))
        ),
        "only failed tickets"
    );
    sqlx::query("UPDATE tickets SET state = 'failed' WHERE id = $1")
        .bind(t)
        .execute(&w.pool)
        .await
        .unwrap();
    assert_eq!(
        retry_or_close(&w.pool, t, &console("boss"), true)
            .await
            .unwrap()
            .as_str(),
        "closed_rejected"
    );
}

#[tokio::test]
async fn tasklane_s_own_comments_do_not_trip_the_text_rules_but_a_person_s_do() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let w = world(pool, |_| {}).await;
    let t = ticket(&w, "T-plain").await;
    w.orch.run_one().await.unwrap(); // triage: level 1, queued
                                     // tasklane's own plan comment says "no secrets"; the author is the bot
    std::fs::write(
        w.state_dir().join("comments_T-plain.json"),
        r#"[{"author":"bot","body":"Plan: edit README. It needs no secrets and no permissions.","at":"t","mine":true}]"#,
    )
    .unwrap();
    drive(&w).await;
    assert_eq!(
        state(&w.pool, t).await,
        "merged",
        "its own words did not stop it"
    );
}

#[tokio::test]
async fn a_push_refused_for_workflow_permission_fails_at_once_with_a_clear_reason() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let w = world(pool, |_| {}).await;
    // the remote behaves like GitHub does for an App without the Workflows permission
    let hook = w.remote().join("hooks/pre-receive");
    std::fs::write(&hook, "#!/bin/sh\necho 'refusing to allow a GitHub App to create or update workflow `.github/workflows/ci.yml` without `workflows` permission' >&2\nexit 1\n").unwrap();
    std::process::Command::new("chmod")
        .arg("+x")
        .arg(&hook)
        .status()
        .unwrap();
    let t = ticket(&w, "T-protected").await; // touches .github/workflows/
    add_principal(&w.pool, "boss", "owner").await;
    drive(&w).await; // triage, develop -> asks for approval of the diff
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
    assert_eq!(state(&w.pool, t).await, "failed");
    let err: String = sqlx::query_scalar("SELECT last_error FROM tickets WHERE id = $1")
        .bind(t)
        .fetch_one(&w.pool)
        .await
        .unwrap();
    assert!(err.contains("Workflows permission"), "{err}");
    let attempts: i16 = sqlx::query_scalar("SELECT attempts FROM jobs WHERE kind = 'pr'")
        .fetch_one(&w.pool)
        .await
        .unwrap();
    assert_eq!(attempts, 1, "no pointless retries");
}

#[tokio::test]
async fn a_flood_of_issues_does_not_queue_unlimited_model_runs_and_nothing_is_lost() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let w = world(pool, |c| c.max_pending_triage = 2).await;
    use tasklane_orchestrator::sync::Noted;
    let mut created = 0;
    let mut held = 0;
    for n in 0..5 {
        match w
            .orch
            .note_issue("org/repo", &format!("{}", 200 + n))
            .await
            .unwrap()
        {
            Noted::Created => created += 1,
            Noted::Deferred => held += 1,
            Noted::Resumed | Noted::Nothing => {}
        }
    }
    assert_eq!(
        (created, held),
        (2, 3),
        "only two wait for triage; the rest are held back"
    );
    // once the queue drains they are picked up (the sync cursor never moved past them)
    drive(&w).await;
    for n in 0..5 {
        let _ = w
            .orch
            .note_issue("org/repo", &format!("{}", 200 + n))
            .await
            .unwrap();
    }
    assert_eq!(count(&w.pool, "SELECT count(*) FROM tickets").await, 4);
}

#[tokio::test]
async fn plugins_ready_is_true_for_live_plugins_even_if_they_do_not_know_describe() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let w = world(pool, |_| {}).await;
    assert!(w.orch.plugins_ready().await);
}

#[tokio::test]
async fn a_ticket_over_its_token_budget_stops_before_another_model_run() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let w = world(pool, |c| c.max_tokens_per_ticket = 1000).await;
    let t = ticket(&w, "T-costly").await;
    drive(&w).await;
    assert_eq!(state(&w.pool, t).await, "failed");
    let (used, why): (i64, String) =
        sqlx::query_as("SELECT tokens_used, last_error FROM tickets WHERE id = $1")
            .bind(t)
            .fetch_one(&w.pool)
            .await
            .unwrap();
    assert_eq!(used, 1100);
    assert!(why.contains("budget"), "{why}");
    assert_eq!(
        count(
            &w.pool,
            "SELECT count(*) FROM tickets WHERE branch IS NOT NULL"
        )
        .await,
        0,
        "no development run started"
    );
}

#[tokio::test]
async fn a_ticket_nobody_answers_is_closed_as_stale_but_a_recent_one_is_left_alone() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let w = world(pool, |_| {}).await;
    let old = ticket(&w, "T-ask-old").await;
    let fresh = ticket(&w, "T-ask-fresh").await;
    drive(&w).await;
    assert_eq!(state(&w.pool, old).await, "needs_info");
    sqlx::query("UPDATE tickets SET updated_at = now() - interval '15 days' WHERE id = $1")
        .bind(old)
        .execute(&w.pool)
        .await
        .unwrap();
    assert_eq!(w.orch.close_stale().await.unwrap(), 1);
    assert_eq!(state(&w.pool, old).await, "closed_stale");
    assert_eq!(state(&w.pool, fresh).await, "needs_info");
    assert!(audit_actions(&w.pool, old)
        .await
        .contains(&"ticket.closed_stale".to_string()));
}

#[tokio::test]
async fn a_retry_never_merges_a_head_the_reviewer_did_not_approve() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let w = world(pool, |_| {}).await;
    add_principal(&w.pool, "boss", "owner").await;
    let t = ticket(&w, "T-plain").await;
    sqlx::query("UPDATE tickets SET state = 'failed', branch = 'b', pr_ref = '7', head_sha = 'h1', pushed_head = 'h1', reviewed_head = NULL WHERE id = $1")
        .bind(t)
        .execute(&w.pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM jobs")
        .execute(&w.pool)
        .await
        .unwrap();
    retry_or_close(&w.pool, t, &console("boss"), false)
        .await
        .unwrap();
    assert_eq!(
        count(&w.pool, "SELECT count(*) FROM jobs WHERE kind = 'merge'").await,
        0,
        "an unreviewed head goes back through CI and review"
    );
    assert_eq!(
        count(&w.pool, "SELECT count(*) FROM jobs WHERE kind = 'pr'").await,
        1
    );
    // a head the reviewer did approve may be merged again
    sqlx::query("UPDATE tickets SET state = 'failed', reviewed_head = 'h1' WHERE id = $1")
        .bind(t)
        .execute(&w.pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM jobs")
        .execute(&w.pool)
        .await
        .unwrap();
    retry_or_close(&w.pool, t, &console("boss"), false)
        .await
        .unwrap();
    assert_eq!(
        count(&w.pool, "SELECT count(*) FROM jobs WHERE kind = 'merge'").await,
        1
    );
}
