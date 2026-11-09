use serde_json::{json, Value};
use tasklane_core::{Actor, Level, State};
use tasklane_queue::ClaimedJob;

use super::{sha256_hex, text_of};
use crate::{Error, Orchestrator, RunResult};

/// A read-only agent looks at the diff that CI passed. Only the exact head it
/// reviewed can be merged afterwards.
pub(crate) async fn run(o: &Orchestrator, job: &ClaimedJob) -> Result<RunResult, Error> {
    let t = o.load_ticket(job.ticket_id).await?;
    if t.state != State::Reviewing {
        return Err(Error::WrongState {
            ticket_id: t.id,
            found: t.state,
            wanted: "reviewing",
        });
    }
    let branch = t
        .branch
        .clone()
        .ok_or_else(|| Error::Fatal("review step without a branch".into()))?;
    let reviewed_head = t
        .head_sha
        .clone()
        .ok_or_else(|| Error::Fatal("review step without a head".into()))?;
    // Bring the mirror up to date so the diff is exactly what CI saw.
    let token = o.run_token(&t, &["contents:read"]).await?;
    let url = o.repo_url(&t, Some(&token)).await?;
    o.ws.fetch_branch(&t.service, &url, &branch).await?;
    let head_now = o.ws.head(&t.service, &branch).await?;
    if head_now != reviewed_head {
        return Err(Error::Fatal(format!(
            "the branch moved after CI ({reviewed_head} -> {head_now})"
        )));
    }
    let (diff, files) = o.ws.diff(&t.service, &t.base_branch(), &branch).await?;
    // The same deterministic rules as at development time, on the diff CI actually ran.
    // A diff that needs a person and was not approved as this exact diff goes back to a person.
    let chk = super::develop::check_diff(o, &t.facts, &diff, &files);
    let diff_hash = sha256_hex(&diff);
    if chk.needs_person && t.plan_hash.as_deref() != Some(diff_hash.as_str()) {
        let level = t.level.unwrap_or(Level::L2).max(Level::L2);
        let mut tx = o.begin(job).await?;
        o.set_state(
            &mut tx,
            t.id,
            State::Reviewing,
            State::AwaitingApproval,
            Actor::Orchestrator,
        )
        .await?;
        sqlx::query("UPDATE tickets SET level = $2, plan_hash = $3, diff_text = $4 WHERE id = $1")
            .bind(t.id)
            .bind(i16::from(u8::from(level)))
            .bind(&diff_hash)
            .bind(super::develop::cap(&diff))
            .execute(&mut *tx)
            .await?;
        o.audit(
            &mut tx,
            t.id,
            "ticket.rescored_at_review",
            json!({"reasons": chk.reasons, "head": reviewed_head}),
        )
        .await?;
        o.finish_job(&mut tx, job).await?;
        tx.commit().await?;
        o.say(
            &t,
            &format!(
                "Before review: {}. A person must approve this exact change.",
                chk.reasons.join("; ")
            ),
        )
        .await;
        return Ok(RunResult::Moved {
            ticket_id: t.id,
            to: State::AwaitingApproval,
        });
    }
    o.check_budget(t.id).await?;
    let item = o.item(&t).await?;
    let out = o
        .engine
        .call(
            "run",
            json!({
                "role": "reviewer",
                "ticket": {"service": t.service, "ref": t.external_ref, "title": text_of(&item, "title"), "body": text_of(&item, "body"), "comments": []},
                "profile": t.agent_profile,
                "diff": diff,
                "budget": {"max_secs": o.cfg.call_timeout.as_secs(), "max_tokens": 100000},
                "read_only": true
            }),
            o.cfg.call_timeout,
        )
        .await?;
    o.charge(t.id, &out).await?;
    let verdict = out
        .get("verdict")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let notes = text_of(&out, "notes");

    let mut tx = o.begin(job).await?;
    let to = match verdict.as_str() {
        "approve" => {
            o.set_state(
                &mut tx,
                t.id,
                State::Reviewing,
                State::MergeReady,
                Actor::Orchestrator,
            )
            .await?;
            sqlx::query("UPDATE tickets SET reviewed_head = head_sha WHERE id = $1")
                .bind(t.id)
                .execute(&mut *tx)
                .await?;
            o.enqueue(&mut tx, t.id, "merge", 0.0).await?;
            State::MergeReady
        }
        "changes" => {
            sqlx::query("UPDATE tickets SET last_error = $2 WHERE id = $1")
                .bind(t.id)
                .bind(format!("Reviewer asked for changes: {notes}"))
                .execute(&mut *tx)
                .await?;
            o.set_state(
                &mut tx,
                t.id,
                State::Reviewing,
                State::Developing,
                Actor::Orchestrator,
            )
            .await?;
            o.enqueue(&mut tx, t.id, "develop", 0.0).await?;
            State::Developing
        }
        "escalate" => {
            let level = t.level.unwrap_or(Level::L2).max(Level::L2);
            o.set_state(
                &mut tx,
                t.id,
                State::Reviewing,
                State::AwaitingApproval,
                Actor::Orchestrator,
            )
            .await?;
            sqlx::query(
                "UPDATE tickets SET level = $2, plan_hash = $3, diff_text = $4 WHERE id = $1",
            )
            .bind(t.id)
            .bind(i16::from(u8::from(level)))
            .bind(&diff_hash)
            .bind(super::develop::cap(&diff))
            .execute(&mut *tx)
            .await?;
            State::AwaitingApproval
        }
        other => {
            return Err(Error::BadOutput(format!(
                "unknown reviewer verdict {other:?}"
            )))
        }
    };
    o.audit(
        &mut tx,
        t.id,
        "ticket.reviewed",
        json!({"verdict": verdict, "notes": notes, "head": reviewed_head}),
    )
    .await?;
    o.finish_job(&mut tx, job).await?;
    tx.commit().await?;
    match to {
        State::AwaitingApproval => {
            o.say(
                &t,
                &format!("The reviewer wants a person to decide: {notes}"),
            )
            .await
        }
        State::Developing => {
            o.say(&t, &format!("The reviewer asked for changes: {notes}"))
                .await
        }
        _ => {}
    }
    Ok(RunResult::Moved {
        ticket_id: t.id,
        to,
    })
}
