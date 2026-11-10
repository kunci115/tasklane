use serde_json::{json, Value};
use tasklane_core::{Actor, State};
use tasklane_queue::ClaimedJob;

use crate::{Error, Orchestrator, RunResult};

/// Merges the pull request, but only if its head is still the one that was
/// reviewed. Production is never touched; a person promotes from the dev branch.
pub(crate) async fn run(o: &Orchestrator, job: &ClaimedJob) -> Result<RunResult, Error> {
    let t = o.load_ticket(job.ticket_id).await?;
    if !matches!(t.state, State::MergeReady | State::Queued) {
        return Err(Error::WrongState {
            ticket_id: t.id,
            found: t.state,
            wanted: "merge_ready or queued",
        });
    }
    let pr = t
        .pr_ref
        .clone()
        .ok_or_else(|| Error::Fatal("merge step without a pull request".into()))?;
    let head = t
        .head_sha
        .clone()
        .ok_or_else(|| Error::Fatal("merge step without a reviewed head".into()))?;
    let token = o
        .run_token(&t, &["contents:write", "pull_requests:write"])
        .await?;
    let r = o
        .source
        .call(
            "merge_pr",
            json!({"service": t.service, "pr": pr, "expected_head_sha": head, "token": token}),
            o.cfg.call_timeout,
        )
        .await?;
    let merged = r.get("merged").and_then(Value::as_bool).unwrap_or(false);
    let reason = r
        .get("reason")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();

    // Closing the issue later needs the merge commit; ask for it when the merge answer lacked it.
    let merge_sha = match r
        .get("sha")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        Some(sha) => Some(sha.to_string()),
        None if merged => o
            .source
            .call(
                "get_pull",
                json!({"service": t.service, "pr": pr}),
                o.cfg.call_timeout,
            )
            .await
            .ok()
            .and_then(|v| {
                v.get("merge_sha")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
            }),
        None => None,
    };
    let mut tx = o.begin(job).await?;
    if t.state == State::Queued {
        o.set_state(
            &mut tx,
            t.id,
            State::Queued,
            State::MergeReady,
            Actor::Orchestrator,
        )
        .await?;
    }
    let to = if merged {
        o.set_state(
            &mut tx,
            t.id,
            State::MergeReady,
            State::Merged,
            Actor::Runner,
        )
        .await?;
        o.audit(
            &mut tx,
            t.id,
            "ticket.merged",
            json!({"pr": pr, "head": head, "merge_sha": merge_sha, "into": t.base_branch(), "closes_on": t.default_branch()}),
        )
        .await?;
        State::Merged
    } else if (reason == "head_moved" || reason == "checks_failing")
        && {
            // a required check tasklane cannot see (or a head that keeps moving) would loop for ever
            let refused: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM audit_events WHERE ticket_id = $1 AND action = 'ticket.merge_refused'",
        )
        .bind(t.id)
        .fetch_one(&mut *tx)
        .await?;
            refused >= 3
        }
    {
        tx.rollback().await?;
        return Err(Error::Fatal(format!(
            "the merge was refused three times ({reason}); a check that tasklane cannot see may be required, or the branch keeps changing"
        )));
    } else if reason == "head_moved" || reason == "checks_failing" {
        // Something changed under us: look at CI again for the new head, then review again.
        o.set_state(
            &mut tx,
            t.id,
            State::MergeReady,
            State::CiRunning,
            Actor::Runner,
        )
        .await?;
        o.audit(
            &mut tx,
            t.id,
            "ticket.merge_refused",
            json!({"reason": reason, "pr": pr}),
        )
        .await?;
        o.enqueue(&mut tx, t.id, "ci", o.cfg.ci_poll_secs).await?;
        State::CiRunning
    } else {
        tx.rollback().await?;
        return Err(Error::Fatal(if reason == "merge_queue" {
            "this repository merges through GitHub's merge queue, which tasklane does not drive yet: add the reviewed pull request to the queue yourself".to_string()
        } else {
            format!("the pull request cannot be merged: {reason}")
        }));
    };
    o.finish_job(&mut tx, job).await?;
    tx.commit().await?;
    match to {
        State::Merged if t.base_branch() == t.default_branch() => {
            o.say(&t, &format!("Merged pull request {pr} into `{}`. This issue is closed once the merge is confirmed.", t.base_branch())).await
        }
        State::Merged => {
            o.say(&t, &format!("Merged pull request {pr} into `{}`. This issue stays open until the change reaches `{}`; tasklane closes it then.", t.base_branch(), t.default_branch())).await
        }
        _ => o.say(&t, &format!("Merge was refused ({reason}); checking again.")).await,
    }
    Ok(RunResult::Moved {
        ticket_id: t.id,
        to,
    })
}
