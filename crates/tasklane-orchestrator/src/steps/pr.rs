use serde_json::{json, Value};
use tasklane_core::{Actor, State};
use tasklane_queue::ClaimedJob;

use super::text_of;
use crate::{Error, Orchestrator, RunResult};

/// Pushes the branch and opens the pull request, with a token that lives for this run only.
pub(crate) async fn run(o: &Orchestrator, job: &ClaimedJob) -> Result<RunResult, Error> {
    let t = o.load_ticket(job.ticket_id).await?;
    if !matches!(t.state, State::Rescoring | State::Queued) {
        return Err(Error::WrongState {
            ticket_id: t.id,
            found: t.state,
            wanted: "rescoring or queued",
        });
    }
    let branch = t
        .branch
        .clone()
        .ok_or_else(|| Error::Fatal("no branch recorded for this ticket".into()))?;
    let token = o
        .run_token(&t, &["contents:write", "pull_requests:write"])
        .await?;
    let url = o.repo_url(&t, Some(&token)).await?;
    if let Err(e) = o.ws.push(&t.service, &url, &branch).await {
        let msg = e.to_string();
        if msg.to_lowercase().contains("workflow") && msg.to_lowercase().contains("permission") {
            return Err(Error::Fatal(
                "the ticket source refuses a push that changes CI workflow files without the Workflows permission. \
                 tasklane does not ask for it by default. Make this change by hand, or grant the App the Workflows permission on purpose."
                    .into(),
            ));
        }
        return Err(e);
    }
    o.queue.checkpoint(job, &json!({"stage": "pushed"})).await?;

    let pr_ref = match &t.pr_ref {
        // A re-developed branch updates the pull request that already exists.
        Some(existing) => existing.clone(),
        None => {
            let item = o.item(&t).await?;
            let title = format!("{} (#{})", text_of(&item, "title"), t.external_ref);
            let credit = o.credit_for(t.id, &text_of(&item, "author")).await;
            let who = if credit.is_empty() {
                String::new()
            } else {
                format!("\n\n{}", credit.pr_line())
            };
            let body = format!("Automated change for #{}.\n\nThis pull request was produced by tasklane and is reviewed and merged by it only after CI passes.{who}", t.external_ref);
            let pr = o
                .source
                .call(
                    "open_pr",
                    json!({"service": t.service, "branch": branch, "base": t.base_branch(), "title": title, "body": body, "token": token}),
                    o.cfg.call_timeout,
                )
                .await?;
            pr.get("pr")
                .and_then(Value::as_str)
                .ok_or_else(|| Error::BadOutput("open_pr without pr".into()))?
                .to_string()
        }
    };

    let mut tx = o.begin(job).await?;
    if t.state == State::Queued {
        o.set_state(
            &mut tx,
            t.id,
            State::Queued,
            State::Rescoring,
            Actor::Orchestrator,
        )
        .await?;
    }
    o.set_state(
        &mut tx,
        t.id,
        State::Rescoring,
        State::PrOpen,
        Actor::Runner,
    )
    .await?;
    o.set_state(
        &mut tx,
        t.id,
        State::PrOpen,
        State::CiRunning,
        Actor::Runner,
    )
    .await?;
    sqlx::query("UPDATE tickets SET pr_ref = $2, pushed_head = head_sha WHERE id = $1")
        .bind(t.id)
        .bind(&pr_ref)
        .execute(&mut *tx)
        .await?;
    o.audit(
        &mut tx,
        t.id,
        "ticket.pr_opened",
        json!({"pr": pr_ref, "branch": branch}),
    )
    .await?;
    o.enqueue(&mut tx, t.id, "ci", o.cfg.ci_poll_secs).await?;
    o.finish_job(&mut tx, job).await?;
    tx.commit().await?;
    o.say(
        &t,
        &format!("Opened pull request {pr_ref}. Waiting for CI."),
    )
    .await;
    Ok(RunResult::Moved {
        ticket_id: t.id,
        to: State::CiRunning,
    })
}
