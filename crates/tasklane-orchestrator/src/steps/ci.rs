use serde_json::{json, Value};
use tasklane_core::{Actor, State};
use tasklane_queue::ClaimedJob;

use crate::{Error, Orchestrator, RunResult};

/// Looks at the pull request's checks. Pending: look again later. Green: on to
/// review. Red: back to development once, then a person takes over.
pub(crate) async fn run(o: &Orchestrator, job: &ClaimedJob) -> Result<RunResult, Error> {
    let t = o.load_ticket(job.ticket_id).await?;
    if t.state != State::CiRunning {
        return Err(Error::WrongState {
            ticket_id: t.id,
            found: t.state,
            wanted: "ci_running",
        });
    }
    let pr = t
        .pr_ref
        .clone()
        .ok_or_else(|| Error::Fatal("ci step without a pull request".into()))?;
    let r = o
        .source
        .call(
            "get_checks",
            json!({"service": t.service, "pr": pr}),
            o.cfg.call_timeout,
        )
        .await?;
    let mut state = r.get("state").and_then(Value::as_str).unwrap_or("pending");
    let no_checks = r
        .get("checks")
        .and_then(Value::as_array)
        .is_none_or(|a| a.is_empty());
    if state == "success" && no_checks && !o.cfg.allow_no_checks {
        state = "pending";
    }
    let head = r
        .get("head_sha")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();

    match state {
        "pending" => {
            let (age,): (i64,) = sqlx::query_as(
                "SELECT extract(epoch FROM now() - created_at)::bigint FROM jobs WHERE id = $1",
            )
            .bind(job.id)
            .fetch_one(&o.pool)
            .await?;
            // The job row is re-used between polls, so age counts from the first poll.
            if age > o.cfg.ci_timeout_secs {
                return Err(Error::Fatal(format!(
                    "CI stayed pending for more than {} seconds",
                    o.cfg.ci_timeout_secs
                )));
            }
            o.queue.defer(job, o.cfg.ci_poll_secs).await?;
            Ok(RunResult::Waiting)
        }
        "success" => {
            if head.is_empty() {
                return Err(Error::BadOutput("get_checks without head_sha".into()));
            }
            let mut tx = o.begin(job).await?;
            o.set_state(
                &mut tx,
                t.id,
                State::CiRunning,
                State::Reviewing,
                Actor::Orchestrator,
            )
            .await?;
            sqlx::query("UPDATE tickets SET head_sha = $2 WHERE id = $1")
                .bind(t.id)
                .bind(&head)
                .execute(&mut *tx)
                .await?;
            o.audit(&mut tx, t.id, "ticket.ci_green", json!({"head": head}))
                .await?;
            o.enqueue(&mut tx, t.id, "review", 0.0).await?;
            o.finish_job(&mut tx, job).await?;
            tx.commit().await?;
            Ok(RunResult::Moved {
                ticket_id: t.id,
                to: State::Reviewing,
            })
        }
        _ => {
            let failed: Vec<String> = r
                .get("checks")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter(|c| c.get("state").and_then(Value::as_str) == Some("failure"))
                        .filter_map(|c| c.get("name").and_then(Value::as_str).map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            let reason = format!(
                "CI failed: {}",
                if failed.is_empty() {
                    "unknown check".to_string()
                } else {
                    failed.join(", ")
                }
            );
            let runs = t.ci_runs + 1;
            let mut tx = o.begin(job).await?;
            sqlx::query("UPDATE tickets SET ci_runs = $2, last_error = $3 WHERE id = $1")
                .bind(t.id)
                .bind(runs)
                .bind(&reason)
                .execute(&mut *tx)
                .await?;
            if runs >= o.cfg.max_ci_runs {
                // Out of CI runs: a person takes over. The job's end state is handled by the caller.
                tx.commit().await?;
                return Err(Error::Fatal(format!("{reason} ({runs} runs)")));
            }
            o.set_state(
                &mut tx,
                t.id,
                State::CiRunning,
                State::Developing,
                Actor::Orchestrator,
            )
            .await?;
            o.audit(
                &mut tx,
                t.id,
                "ticket.ci_red",
                json!({"failed": failed, "run": runs}),
            )
            .await?;
            o.enqueue(&mut tx, t.id, "develop", 0.0).await?;
            o.finish_job(&mut tx, job).await?;
            tx.commit().await?;
            o.say(&t, &format!("{reason}. Trying once more.")).await;
            Ok(RunResult::Moved {
                ticket_id: t.id,
                to: State::Developing,
            })
        }
    }
}
