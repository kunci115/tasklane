use serde_json::{json, Value};
use tasklane_core::{Actor, Level, State};
use tasklane_queue::ClaimedJob;

use super::{sha256_hex, text_of};
use crate::{Error, Orchestrator, RunResult};

/// Runs the development agent in a fresh worktree, commits what it did, scores
/// the real diff, and either asks for approval again or hands over to the PR step.
pub(crate) async fn run(o: &Orchestrator, job: &ClaimedJob) -> Result<RunResult, Error> {
    let t = o.load_ticket(job.ticket_id).await?;
    if !matches!(t.state, State::Queued | State::Developing) {
        return Err(Error::WrongState {
            ticket_id: t.id,
            found: t.state,
            wanted: "queued or developing",
        });
    }
    // Admission control: only so many development runs at once.
    let (running,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM jobs WHERE kind = 'develop' AND status = 'running' AND id <> $1",
    )
    .bind(job.id)
    .fetch_one(&o.pool)
    .await?;
    if running >= o.cfg.max_dev_parallel {
        o.queue.defer(job, 20.0 + (job.id % 20) as f64).await?;
        return Ok(RunResult::Waiting);
    }
    if t.dev_attempts >= o.cfg.max_dev_attempts {
        return Err(Error::Fatal(format!(
            "development was tried {} times without a mergeable result",
            t.dev_attempts
        )));
    }

    // Start: Queued -> Developing (a retry after a crash is already Developing).
    {
        let mut tx = o.begin(job).await?;
        if t.state == State::Queued {
            o.set_state(
                &mut tx,
                t.id,
                State::Queued,
                State::Developing,
                Actor::Orchestrator,
            )
            .await?;
        }
        sqlx::query("UPDATE tickets SET dev_attempts = dev_attempts + 1 WHERE id = $1")
            .bind(t.id)
            .execute(&mut *tx)
            .await?;
        o.audit(
            &mut tx,
            t.id,
            "ticket.developing",
            json!({"attempt": t.dev_attempts + 1}),
        )
        .await?;
        tx.commit().await?;
    }

    let base = t.base_branch();
    let branch = t.work_branch();
    let read_token = o.run_token(&t, &["contents:read"]).await?;
    let url = o.repo_url(&t, Some(&read_token)).await?;
    let prepared =
        o.ws.prepare(&t.service, &url, &base, &branch, job.id)
            .await?;
    o.queue
        .checkpoint(job, &json!({"stage": "workspace"}))
        .await?;

    let item = o.item(&t).await?;
    let (title, body) = (text_of(&item, "title"), text_of(&item, "body"));
    let out = o
        .engine
        .call(
            "run",
            json!({
                "role": "dev",
                "ticket": {"service": t.service, "ref": t.external_ref, "title": title, "body": body,
                           "comments": item.get("comments").cloned().unwrap_or(Value::Array(vec![]))},
                "profile": t.agent_profile,
                "workspace": prepared.worktree,
                "feedback": t.last_error,
                "budget": {"max_secs": o.cfg.dev_timeout.as_secs(), "max_tokens": 500000},
                "read_only": false
            }),
            o.cfg.dev_timeout,
        )
        .await?;
    let summary = out
        .get("summary")
        .and_then(Value::as_str)
        .unwrap_or("(no summary)")
        .to_string();

    let committed =
        o.ws.commit_all(
            &prepared.worktree,
            &format!("tasklane: {title} ({})", t.external_ref),
        )
        .await?;
    o.ws.remove_worktree(&prepared).await;
    if !committed && o.ws.diff(&t.service, &base, &branch).await?.0.is_empty() {
        return Err(Error::BadOutput(
            "the development agent made no change".into(),
        ));
    }
    let (diff, files) = o.ws.diff(&t.service, &base, &branch).await?;
    let head = o.ws.head(&t.service, &branch).await?;
    o.queue
        .checkpoint(job, &json!({"stage": "committed", "head": head}))
        .await?;

    // Re-score the real diff with deterministic rules.
    let protected: Vec<String> = t
        .facts
        .get("protected_paths")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let hit: Vec<&String> = files
        .iter()
        .filter(|f| protected.iter().any(|p| f.starts_with(p.as_str())))
        .collect();
    let lines = diff
        .lines()
        .filter(|l| {
            (l.starts_with('+') || l.starts_with('-'))
                && !l.starts_with("+++")
                && !l.starts_with("---")
        })
        .count();
    let mut level = t.level.unwrap_or(Level::L2);
    let mut why: Vec<String> = Vec::new();
    if !hit.is_empty() {
        level = level.max(Level::L2);
        why.push(format!(
            "touches protected paths: {}",
            hit.iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if lines > o.cfg.max_diff_lines {
        level = level.max(Level::L2);
        why.push(format!(
            "diff has {lines} changed lines (limit {})",
            o.cfg.max_diff_lines
        ));
    }
    let raised = level > t.level.unwrap_or(Level::L1) && level.needs_human();

    let mut tx = o.begin(job).await?;
    o.set_state(
        &mut tx,
        t.id,
        State::Developing,
        State::Rescoring,
        Actor::Orchestrator,
    )
    .await?;
    sqlx::query("UPDATE tickets SET branch = $2, head_sha = $3, last_error = NULL WHERE id = $1")
        .bind(t.id)
        .bind(&branch)
        .bind(&head)
        .execute(&mut *tx)
        .await?;
    let to = if raised {
        // The approval on file covered a plan, not this diff. Ask again, bound to this diff.
        o.set_state(
            &mut tx,
            t.id,
            State::Rescoring,
            State::AwaitingApproval,
            Actor::Runner,
        )
        .await?;
        sqlx::query("UPDATE tickets SET level = $2, plan_hash = $3 WHERE id = $1")
            .bind(t.id)
            .bind(i16::from(u8::from(level)))
            .bind(sha256_hex(&diff))
            .execute(&mut *tx)
            .await?;
        State::AwaitingApproval
    } else {
        o.enqueue(&mut tx, t.id, "pr", 0.0).await?;
        State::Rescoring
    };
    o.audit(&mut tx, t.id, "ticket.developed", json!({"files": files, "lines": lines, "level": u8::from(level), "reasons": why, "to": to.as_str()})).await?;
    o.finish_job(&mut tx, job).await?;
    tx.commit().await?;

    if raised {
        o.say(&t, &format!("The change is ready but {}. A person with the right role must approve this exact change before I open a pull request.\n\nSummary: {summary}", why.join("; "))).await;
    }
    Ok(RunResult::Moved {
        ticket_id: t.id,
        to,
    })
}
