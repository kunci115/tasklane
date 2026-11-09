use std::future::Future;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use tasklane_core::{floor_from_text, transition, Actor, Level, State};
use tasklane_queue::{ClaimedJob, Outcome, Queue};

use crate::host::PluginClient;
use crate::Error;

pub struct Config {
    /// Worker name written into the lease.
    pub owner: String,
    pub lease_secs: f64,
    pub call_timeout: Duration,
}

pub struct Orchestrator {
    pool: PgPool,
    queue: Queue,
    source: PluginClient,
    engine: PluginClient,
    cfg: Config,
}

#[derive(Debug, PartialEq, Eq)]
pub enum RunResult {
    /// No runnable job.
    Idle,
    /// The ticket moved to this state and the job is done.
    Moved { ticket_id: i64, to: State },
    /// The job was not for a ticket in the expected state; it was closed without changes.
    Skipped,
    /// The plug-in failed; the job went back to the queue.
    Retried,
    /// The plug-in failed and attempts are used up.
    GaveUp,
    /// Another worker owns the job now; this worker changed nothing.
    LeaseLost,
}

/// What the engine returns for the `triage` role.
#[derive(Debug, Deserialize)]
struct TriageOutput {
    level: u8,
    plan: String,
    #[serde(default)]
    questions: Vec<String>,
}

struct TicketRow {
    state: State,
    external_ref: String,
    service: String,
}

impl Orchestrator {
    pub fn new(
        pool: PgPool,
        queue: Queue,
        source: PluginClient,
        engine: PluginClient,
        cfg: Config,
    ) -> Self {
        Orchestrator {
            pool,
            queue,
            source,
            engine,
            cfg,
        }
    }

    /// Claim and run at most one triage job.
    pub async fn run_one(&self) -> Result<RunResult, Error> {
        let Some(job) = self
            .queue
            .claim(&self.cfg.owner, self.cfg.lease_secs, &["triage"])
            .await?
        else {
            return Ok(RunResult::Idle);
        };
        match self.hold_lease(&job, self.triage(&job)).await {
            Err(Error::Queue(tasklane_queue::Error::LeaseLost)) => Ok(RunResult::LeaseLost),
            Err(e @ (Error::Plugin(_) | Error::BadOutput(_))) => {
                eprintln!("job {}: {e}", job.id);
                match self.queue.fail(&job).await {
                    Ok(Outcome::Requeued) => Ok(RunResult::Retried),
                    Ok(Outcome::Failed) => Ok(RunResult::GaveUp),
                    Err(tasklane_queue::Error::LeaseLost) => Ok(RunResult::LeaseLost),
                    Err(e) => Err(e.into()),
                }
            }
            other => other,
        }
    }

    /// Keep the lease alive while `work` runs; give up if it is lost.
    async fn hold_lease<T>(
        &self,
        job: &ClaimedJob,
        work: impl Future<Output = Result<T, Error>>,
    ) -> Result<T, Error> {
        let mut tick = tokio::time::interval(Duration::from_secs_f64(self.cfg.lease_secs / 3.0));
        tick.tick().await; // the first tick is immediate
        tokio::pin!(work);
        loop {
            tokio::select! {
                r = &mut work => return r,
                _ = tick.tick() => self.queue.heartbeat(job, self.cfg.lease_secs).await?,
            }
        }
    }

    async fn load_ticket(&self, ticket_id: i64) -> Result<TicketRow, Error> {
        let (state, external_ref, service): (String, String, String) = sqlx::query_as(
            "SELECT t.state, t.external_ref, s.external_id
             FROM tickets t JOIN services s ON s.id = t.service_id WHERE t.id = $1",
        )
        .bind(ticket_id)
        .fetch_one(&self.pool)
        .await?;
        let state = State::parse(&state).ok_or(Error::UnknownState(state))?;
        Ok(TicketRow {
            state,
            external_ref,
            service,
        })
    }

    async fn triage(&self, job: &ClaimedJob) -> Result<RunResult, Error> {
        let t = self.load_ticket(job.ticket_id).await?;
        if t.state != State::New {
            return self.close_job_without_changes(job).await;
        }
        let item = self
            .source
            .call(
                "get_item",
                json!({"service": t.service, "ref": t.external_ref}),
                self.cfg.call_timeout,
            )
            .await?;
        let title = item
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let body = item
            .get("body")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        self.queue
            .checkpoint(job, &json!({"stage": "fetched"}))
            .await?;

        let out = self
            .engine
            .call(
                "run",
                json!({"role": "triage", "service": t.service, "ref": t.external_ref, "title": title, "body": body}),
                self.cfg.call_timeout,
            )
            .await?;
        let out: TriageOutput =
            serde_json::from_value(out).map_err(|e| Error::BadOutput(e.to_string()))?;
        let suggestion = Level::try_from(out.level).map_err(Error::BadOutput)?;
        self.queue
            .checkpoint(job, &json!({"stage": "triaged", "level": out.level}))
            .await?;

        let (floor, why) = floor_from_text(&format!("{title}\n{body}"));
        let level = Level::with_model_suggestion(floor, suggestion);
        let to = if !out.questions.is_empty() {
            State::NeedsInfo
        } else if level.needs_human() {
            State::AwaitingApproval
        } else {
            State::Queued
        };
        self.commit(job, &t, level, &why, &out, to).await?;
        Ok(RunResult::Moved {
            ticket_id: job.ticket_id,
            to,
        })
    }

    /// One transaction: re-check lease and fence, move the ticket, write audit,
    /// enqueue the next job, finish this job. All or nothing.
    async fn commit(
        &self,
        job: &ClaimedJob,
        t: &TicketRow,
        level: Level,
        why: &[&str],
        out: &TriageOutput,
        to: State,
    ) -> Result<(), Error> {
        let mut tx = self.pool.begin().await?;
        let held: Option<(i64,)> = sqlx::query_as(
            "SELECT id FROM jobs WHERE id = $1 AND fence = $2 AND lease_owner = $3 AND status = 'running' FOR UPDATE",
        )
        .bind(job.id)
        .bind(job.fence)
        .bind(job.owner())
        .fetch_optional(&mut *tx)
        .await?;
        if held.is_none() {
            return Err(tasklane_queue::Error::LeaseLost.into());
        }

        // Triage always passes through Triaged, then branches.
        if to == State::NeedsInfo {
            transition(State::New, State::NeedsInfo, Actor::Orchestrator)?;
        } else {
            transition(State::New, State::Triaged, Actor::Orchestrator)?;
            transition(State::Triaged, to, Actor::Orchestrator)?;
        }
        let plan_hash = format!("{:x}", Sha256::digest(out.plan.as_bytes()));
        let moved = sqlx::query(
            "UPDATE tickets SET state = $2, level = $3, level_reasons = $4, plan_hash = $5, updated_at = now()
             WHERE id = $1 AND state = 'new'",
        )
        .bind(job.ticket_id)
        .bind(to.as_str())
        .bind(i16::from(u8::from(level)))
        .bind(json!(why))
        .bind(&plan_hash)
        .execute(&mut *tx)
        .await?;
        if moved.rows_affected() != 1 {
            // Someone else moved the ticket; leave everything as it is.
            tx.rollback().await?;
            return Err(Error::BadOutput("ticket changed during triage".into()));
        }
        sqlx::query("INSERT INTO audit_events (actor, action, ticket_id, detail) VALUES ('orchestrator', 'ticket.triaged', $1, $2)")
            .bind(job.ticket_id)
            .bind(json!({"from": t.state.as_str(), "to": to.as_str(), "level": u8::from(level), "reasons": why, "plan_hash": plan_hash, "questions": out.questions}))
            .execute(&mut *tx)
            .await?;
        if to == State::Queued {
            sqlx::query("INSERT INTO jobs (ticket_id, kind) VALUES ($1, 'develop')")
                .bind(job.ticket_id)
                .execute(&mut *tx)
                .await?;
        }
        sqlx::query(
            "UPDATE jobs SET status = 'done', lease_owner = NULL, lease_until = NULL WHERE id = $1",
        )
        .bind(job.id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn close_job_without_changes(&self, job: &ClaimedJob) -> Result<RunResult, Error> {
        self.queue.complete(job).await?;
        Ok(RunResult::Skipped)
    }
}
