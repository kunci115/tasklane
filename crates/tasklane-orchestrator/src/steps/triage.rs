use serde::Deserialize;
use serde_json::{json, Value};
use tasklane_core::{floor_from_text, transition, Actor, Level, State};
use tasklane_queue::ClaimedJob;

use super::{sha256_hex, text_of};
use crate::{Error, Orchestrator, RunResult};

/// What the engine returns for the `triage` role.
#[derive(Debug, Deserialize)]
struct TriageOutput {
    level: u8,
    plan: String,
    #[serde(default)]
    questions: Vec<String>,
}

pub(crate) async fn run(o: &Orchestrator, job: &ClaimedJob) -> Result<RunResult, Error> {
    let t = o.load_ticket(job.ticket_id).await?;
    if t.state != State::New {
        return Err(Error::WrongState {
            ticket_id: t.id,
            found: t.state,
            wanted: "new",
        });
    }
    let item = o.item(&t).await?;
    let (title, body) = (text_of(&item, "title"), text_of(&item, "body"));
    o.queue
        .checkpoint(job, &json!({"stage": "fetched"}))
        .await?;

    // The agent reads the code of the repository (a detached, read-only checkout of the base branch).
    // If the checkout cannot be made (an empty repository, a network problem) it still triages from the text.
    let workspace = match triage_checkout(o, &t, job).await {
        Ok(p) => Some(p),
        Err(e) => {
            eprintln!("triage: no checkout for ticket {}: {e}", t.id);
            None
        }
    };
    let out = o
        .engine
        .call(
            "run",
            json!({
                "role": "triage",
                "ticket": {"service": t.service, "ref": t.external_ref, "title": title, "body": body,
                           "comments": item.get("comments").cloned().unwrap_or(Value::Array(vec![]))},
                "profile": t.agent_profile,
                "workspace": workspace.as_ref().map(|p| p.worktree.clone()),
                "budget": {"max_secs": o.cfg.call_timeout.as_secs(), "max_tokens": 100000},
                "read_only": true
            }),
            o.cfg.dev_timeout,
        )
        .await;
    if let Some(p) = &workspace {
        o.ws.remove_worktree(p).await;
    }
    let out = out?;
    let out: TriageOutput =
        serde_json::from_value(out).map_err(|e| Error::BadOutput(e.to_string()))?;
    let suggestion = Level::try_from(out.level).map_err(Error::BadOutput)?;
    o.queue
        .checkpoint(job, &json!({"stage": "triaged", "level": out.level}))
        .await?;

    let (floor, mut why) = floor_from_text(&format!("{title}\n{body}"));
    // Strangers can open issues on a public repository. Their tickets always need a person,
    // whatever the level, so an outsider cannot start an unattended change.
    let untrusted = item.get("author_trusted").and_then(Value::as_bool) == Some(false);
    if untrusted {
        why.push("author is not a collaborator");
    }
    let level = Level::with_model_suggestion(floor, suggestion);
    let to = if !out.questions.is_empty() {
        State::NeedsInfo
    } else if level.needs_human() || t.mode != "full" || untrusted {
        State::AwaitingApproval
    } else {
        State::Queued
    };

    let mut tx = o.begin(job).await?;
    if to == State::NeedsInfo {
        transition(State::New, State::NeedsInfo, Actor::Orchestrator)?;
    } else {
        transition(State::New, State::Triaged, Actor::Orchestrator)?;
        transition(State::Triaged, to, Actor::Orchestrator)?;
    }
    let plan_hash = sha256_hex(&out.plan);
    let moved = sqlx::query(
        "UPDATE tickets SET state = $2, level = $3, level_reasons = $4, plan_hash = $5, plan_text = $6, updated_at = now()
         WHERE id = $1 AND state = 'new'",
    )
    .bind(t.id)
    .bind(to.as_str())
    .bind(i16::from(u8::from(level)))
    .bind(json!(why))
    .bind(&plan_hash)
    .bind(super::develop::cap(&out.plan))
    .execute(&mut *tx)
    .await?;
    if moved.rows_affected() != 1 {
        return Err(Error::WrongState {
            ticket_id: t.id,
            found: State::New,
            wanted: "new",
        });
    }
    o.audit(
        &mut tx,
        t.id,
        "ticket.triaged",
        json!({"from": "new", "to": to.as_str(), "level": u8::from(level), "reasons": why, "plan_hash": plan_hash, "questions": out.questions}),
    )
    .await?;
    if to == State::Queued {
        o.enqueue(&mut tx, t.id, "develop", 0.0).await?;
    }
    o.finish_job(&mut tx, job).await?;
    tx.commit().await?;

    let note = match to {
        State::Queued => format!("Triaged as level {}. Plan: {}\n\nStarting development.", u8::from(level), out.plan),
        State::AwaitingApproval => format!(
            "Triaged as level {} ({}). Plan: {}\n\nWaiting for approval by someone with the right role.",
            u8::from(level),
            if why.is_empty() { "model judgement".to_string() } else { why.join(", ") },
            out.plan
        ),
        _ => format!("I need more information before I can start:\n- {}", out.questions.join("\n- ")),
    };
    o.say(&t, &note).await;
    Ok(RunResult::Moved {
        ticket_id: t.id,
        to,
    })
}

async fn triage_checkout(
    o: &Orchestrator,
    t: &crate::ctx::Ticket,
    job: &ClaimedJob,
) -> Result<crate::workspace::Prepared, Error> {
    let token = o.run_token(t, &["contents:read"]).await?;
    let url = o.repo_url(t, Some(&token)).await?;
    o.ws.prepare_readonly(&t.service, &url, &t.base_branch(), job.id)
        .await
}
