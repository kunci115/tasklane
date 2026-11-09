use serde_json::{json, Value};
use tasklane_core::{floor_from_text, protected_hits, Actor, Level, State};
use tasklane_queue::ClaimedJob;

use super::{sha256_hex, text_of};
use crate::{Error, Orchestrator, RunResult};

/// What a diff needs from a person, by deterministic rules.
pub(crate) struct DiffCheck {
    pub lines: usize,
    pub needs_person: bool,
    pub reasons: Vec<String>,
}

pub(crate) fn check_diff(
    o: &Orchestrator,
    facts: &Value,
    diff: &str,
    files: &[String],
) -> DiffCheck {
    let extra: Vec<String> = facts
        .get("protected_paths")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let hits = protected_hits(files, &extra);
    let lines = diff
        .lines()
        .filter(|l| {
            (l.starts_with('+') || l.starts_with('-'))
                && !l.starts_with("+++")
                && !l.starts_with("---")
        })
        .count();
    let mut reasons = Vec::new();
    if !hits.is_empty() {
        reasons.push(format!("touches protected paths: {}", hits.join(", ")));
    }
    if lines > o.cfg.max_diff_lines {
        reasons.push(format!(
            "diff has {lines} changed lines (limit {})",
            o.cfg.max_diff_lines
        ));
    }
    if diff.len() > o.cfg.max_diff_bytes {
        reasons.push(format!(
            "diff is {} bytes (limit {})",
            diff.len(),
            o.cfg.max_diff_bytes
        ));
    }
    DiffCheck {
        lines,
        needs_person: !reasons.is_empty(),
        reasons,
    }
}

pub(crate) fn cap(s: &str) -> String {
    s.chars().take(200_000).collect()
}

/// Runs the development agent in a fresh worktree, commits what it did, scores
/// the real diff, and either asks for approval of that exact diff or hands over to the PR step.
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

    o.check_budget(t.id).await?;
    // The ticket may have been edited, or commented on, since it was triaged and approved.
    // Run the text rules again on what the agent is about to read.
    let item = o.item(&t).await?;
    let (title, body) = (text_of(&item, "title"), text_of(&item, "body"));
    let comments = item
        .get("comments")
        .cloned()
        .unwrap_or(Value::Array(vec![]));
    // tasklane's own comments (its plan, its status lines) are not part of what a person asked for,
    // and they often contain words like "secrets" ("needs no secrets") that would trip the rules.
    let comment_text: String = comments
        .as_array()
        .map(|a| {
            a.iter()
                .filter(|c| c.get("mine").and_then(Value::as_bool) != Some(true))
                .filter_map(|c| c.get("body").and_then(Value::as_str))
                .filter(|b| !b.contains(crate::MARKER))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();
    let (text_floor, text_why) = floor_from_text(&format!("{title}\n{body}\n{comment_text}"));
    if text_floor.needs_human() && t.level.is_none_or(|l| text_floor > l) {
        let level = text_floor.max(t.level.unwrap_or(Level::L1));
        let mut tx = o.begin(job).await?;
        o.set_state(
            &mut tx,
            t.id,
            State::Developing,
            State::Rescoring,
            Actor::Orchestrator,
        )
        .await?;
        o.set_state(
            &mut tx,
            t.id,
            State::Rescoring,
            State::AwaitingApproval,
            Actor::Runner,
        )
        .await?;
        let text = format!("{title}\n{body}\n{comment_text}");
        sqlx::query("UPDATE tickets SET level = $2, level_reasons = $3, plan_hash = $4, plan_text = $5 WHERE id = $1")
            .bind(t.id)
            .bind(i16::from(u8::from(level)))
            .bind(json!(text_why))
            .bind(sha256_hex(&text))
            .bind(cap(&format!("The ticket text changed after triage and now matches: {}.\n\n{text}", text_why.join(", "))))
            .execute(&mut *tx)
            .await?;
        o.audit(
            &mut tx,
            t.id,
            "ticket.text_changed",
            json!({"level": u8::from(level), "reasons": text_why}),
        )
        .await?;
        o.finish_job(&mut tx, job).await?;
        tx.commit().await?;
        o.say(&t, &format!("The ticket text changed since it was approved and now matches: {}. It needs approval again before I change anything.", text_why.join(", "))).await;
        return Ok(RunResult::Moved {
            ticket_id: t.id,
            to: State::AwaitingApproval,
        });
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

    let out = o
        .engine
        .call(
            "run",
            json!({
                "role": "dev",
                "ticket": {"service": t.service, "ref": t.external_ref, "title": title, "body": body, "comments": comments},
                "profile": t.agent_profile,
                "workspace": prepared.worktree,
                "feedback": t.last_error,
                "budget": {"max_secs": o.cfg.dev_timeout.as_secs(), "max_tokens": 500000},
                "read_only": false
            }),
            o.cfg.dev_timeout,
        )
        .await;
    let out = match out {
        Ok(v) => v,
        Err(e) => {
            o.ws.remove_worktree(&prepared).await;
            return Err(e.into());
        }
    };
    o.charge(t.id, &out).await?;
    let summary = out
        .get("summary")
        .and_then(Value::as_str)
        .unwrap_or("(no summary)")
        .to_string();

    let committed =
        o.ws.commit_all(
            &prepared,
            &format!("tasklane: {title} ({})", t.external_ref),
        )
        .await;
    o.ws.remove_worktree(&prepared).await;
    let committed = committed?;
    if !committed && o.ws.diff(&t.service, &base, &branch).await?.0.is_empty() {
        let said: String = summary.chars().take(500).collect();
        return Err(Error::BadOutput(format!(
            "the development agent made no change. It said: {said}"
        )));
    }
    let (diff, files) = o.ws.diff(&t.service, &base, &branch).await?;
    let head = o.ws.head(&t.service, &branch).await?;
    o.queue
        .checkpoint(job, &json!({"stage": "committed", "head": head}))
        .await?;

    // Re-score the real diff with deterministic rules. A diff that needs a person
    // is approved as that exact diff, whatever was approved before.
    let chk = check_diff(o, &t.facts, &diff, &files);
    let diff_hash = sha256_hex(&diff);
    let ask = chk.needs_person && t.plan_hash.as_deref() != Some(diff_hash.as_str());
    let level = if chk.needs_person {
        t.level.unwrap_or(Level::L2).max(Level::L2)
    } else {
        t.level.unwrap_or(Level::L1)
    };

    let mut tx = o.begin(job).await?;
    o.set_state(
        &mut tx,
        t.id,
        State::Developing,
        State::Rescoring,
        Actor::Orchestrator,
    )
    .await?;
    sqlx::query("UPDATE tickets SET branch = $2, head_sha = $3, last_error = NULL, diff_text = $4 WHERE id = $1")
        .bind(t.id)
        .bind(&branch)
        .bind(&head)
        .bind(cap(&diff))
        .execute(&mut *tx)
        .await?;
    let to = if ask {
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
            .bind(&diff_hash)
            .execute(&mut *tx)
            .await?;
        State::AwaitingApproval
    } else {
        o.enqueue(&mut tx, t.id, "pr", 0.0).await?;
        State::Rescoring
    };
    o.audit(&mut tx, t.id, "ticket.developed", json!({"files": files, "lines": chk.lines, "level": u8::from(level), "reasons": chk.reasons, "to": to.as_str()})).await?;
    o.finish_job(&mut tx, job).await?;
    tx.commit().await?;

    if ask {
        o.say(&t, &format!("The change is ready but {}. A person with the right role must approve this exact change (see the console) before I open a pull request.\n\nSummary: {summary}", chk.reasons.join("; "))).await;
    }
    Ok(RunResult::Moved {
        ticket_id: t.id,
        to,
    })
}
