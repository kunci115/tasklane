use std::future::Future;
use std::path::PathBuf;
use std::time::Duration;

use serde_json::{json, Value};
use sqlx::{PgPool, Postgres, Transaction};
use tasklane_core::{transition, Actor, Level, State};
use tasklane_queue::{ClaimedJob, Outcome, Queue};

use crate::host::PluginClient;
use crate::steps;
use crate::workspace::Workspaces;
use crate::Error;

pub struct Config {
    /// Worker name written into the lease.
    pub owner: String,
    pub lease_secs: f64,
    /// Timeout for quick plug-in calls (source API, triage, review).
    pub call_timeout: Duration,
    /// Timeout for the development agent run.
    pub dev_timeout: Duration,
    /// Name of the ticket source in the registry (`services.source`), for example `github`.
    pub source_name: String,
    /// Where mirrors and worktrees live.
    pub data_dir: PathBuf,
    /// At most this many development runs at once (admission control).
    pub max_dev_parallel: i64,
    /// Seconds between CI checks.
    pub ci_poll_secs: f64,
    /// Give up on CI that stays pending this long.
    pub ci_timeout_secs: i64,
    /// CI failures allowed per ticket before a human takes over.
    pub max_ci_runs: i16,
    /// Development attempts per ticket.
    pub max_dev_attempts: i16,
    /// A diff with more changed lines than this raises the level to at least 2.
    pub max_diff_lines: usize,
    /// A diff larger than this many bytes counts as big too (long lines cannot hide in few lines).
    pub max_diff_bytes: usize,
    /// Treat a pull request with no checks at all as green. Off by default: a PR
    /// whose checks have not registered yet must not be merged on an empty list.
    pub allow_no_checks: bool,
    /// Stop creating tickets from the source while this many triage jobs are already waiting
    /// (a flood of issues cannot queue unlimited model runs). The skipped issues are picked up
    /// again later because the sync cursor does not move past them.
    pub max_pending_triage: i64,
    /// Lifetime of the per-run source tokens.
    pub token_ttl_secs: u64,
}

impl Config {
    pub fn new(owner: impl Into<String>, data_dir: PathBuf) -> Self {
        Config {
            owner: owner.into(),
            lease_secs: 60.0,
            call_timeout: Duration::from_secs(120),
            dev_timeout: Duration::from_secs(1800),
            source_name: "github".into(),
            data_dir,
            max_dev_parallel: 1,
            ci_poll_secs: 30.0,
            ci_timeout_secs: 3600,
            max_ci_runs: 2,
            max_dev_attempts: 3,
            max_diff_lines: 800,
            max_diff_bytes: 200_000,
            allow_no_checks: false,
            max_pending_triage: 25,
            token_ttl_secs: 3600,
        }
    }
}

pub struct Orchestrator {
    pub(crate) pool: PgPool,
    pub(crate) queue: Queue,
    pub(crate) source: PluginClient,
    pub(crate) engine: PluginClient,
    pub(crate) cfg: Config,
    pub(crate) ws: Workspaces,
}

#[derive(Debug, PartialEq, Eq)]
pub enum RunResult {
    /// No runnable job.
    Idle,
    /// The ticket moved to this state and the job is done.
    Moved { ticket_id: i64, to: State },
    /// The job is waiting (for CI or for a free development slot).
    Waiting,
    /// The job was not for a ticket in the expected state; it was closed without changes.
    Skipped,
    /// The plug-in failed; the job went back to the queue.
    Retried,
    /// Attempts are used up; the ticket went to `failed`.
    GaveUp,
    /// Another worker owns the job now; this worker changed nothing.
    LeaseLost,
}

pub(crate) struct Ticket {
    pub id: i64,
    pub state: State,
    pub external_ref: String,
    pub service: String,
    pub level: Option<Level>,
    #[allow(dead_code)]
    pub plan_hash: Option<String>,
    pub ci_runs: i16,
    pub dev_attempts: i16,
    pub head_sha: Option<String>,
    pub branch: Option<String>,
    pub pr_ref: Option<String>,
    pub last_error: Option<String>,
    pub facts: Value,
    pub agent_profile: Value,
    /// `watch`, `triage` or `full` (how far the system may go in this repo).
    pub mode: String,
}

impl Ticket {
    pub fn base_branch(&self) -> String {
        self.facts
            .get("default_branch")
            .and_then(Value::as_str)
            .unwrap_or("main")
            .to_string()
    }
    pub fn work_branch(&self) -> String {
        format!("tasklane/{}", self.id)
    }
}

/// Hidden marker on every comment tasklane posts, so its own comments are never mistaken for a person's reply.
pub const MARKER: &str = "<!-- tasklane -->";

pub(crate) type Tx = Transaction<'static, Postgres>;

pub(crate) const KINDS: [&str; 6] = ["triage", "develop", "pr", "ci", "review", "merge"];

impl Orchestrator {
    pub fn new(
        pool: PgPool,
        queue: Queue,
        source: PluginClient,
        engine: PluginClient,
        cfg: Config,
    ) -> Self {
        let ws = Workspaces::new(cfg.data_dir.clone());
        Orchestrator {
            pool,
            queue,
            source,
            engine,
            cfg,
            ws,
        }
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    pub fn queue(&self) -> &Queue {
        &self.queue
    }

    pub fn config(&self) -> &Config {
        &self.cfg
    }

    /// Start the ticket-source plug-in again with new settings.
    pub async fn reconfigure_source(&self, spec: crate::host::PluginSpec) -> Result<(), Error> {
        Ok(self.source.reconfigure(spec).await?)
    }

    /// Start the agent-engine plug-in again with new settings.
    pub async fn reconfigure_engine(&self, spec: crate::host::PluginSpec) -> Result<(), Error> {
        Ok(self.engine.reconfigure(spec).await?)
    }

    /// Call an operation on the ticket-source plug-in.
    pub async fn source_call(&self, op: &str, params: Value) -> Result<Value, Error> {
        Ok(self.source.call(op, params, self.cfg.call_timeout).await?)
    }

    /// A short call to the engine (for the setup page's checks).
    pub async fn engine_call_with(
        &self,
        op: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, Error> {
        Ok(self.engine.call(op, params, timeout).await?)
    }

    /// A call to the source with its own timeout.
    pub async fn source_call_with(
        &self,
        op: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, Error> {
        Ok(self.source.call(op, params, timeout).await?)
    }

    /// Call an operation on the agent-engine plug-in with the long (development) timeout.
    pub async fn engine_call(&self, op: &str, params: Value) -> Result<Value, Error> {
        Ok(self.engine.call(op, params, self.cfg.dev_timeout).await?)
    }

    /// The global kill switch: while paused, no job is claimed.
    pub async fn is_paused(&self) -> Result<bool, Error> {
        let v: Option<String> =
            sqlx::query_scalar("SELECT value FROM sync_state WHERE key = 'paused'")
                .fetch_optional(&self.pool)
                .await?;
        Ok(v.as_deref() == Some("1"))
    }

    pub async fn set_paused(&self, paused: bool) -> Result<(), Error> {
        sqlx::query("INSERT INTO sync_state (key, value) VALUES ('paused', $1) ON CONFLICT (key) DO UPDATE SET value = $1, updated_at = now()")
            .bind(if paused { "1" } else { "0" })
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Claim and run at most one job.
    pub async fn run_one(&self) -> Result<RunResult, Error> {
        if self.is_paused().await? {
            return Ok(RunResult::Idle);
        }
        let Some(job) = self
            .queue
            .claim(&self.cfg.owner, self.cfg.lease_secs, &KINDS)
            .await?
        else {
            return Ok(RunResult::Idle);
        };
        // A repository switched to `watch` stops all activity, including tickets already in flight.
        let mode: Option<String> = sqlx::query_scalar(
            "SELECT s.mode FROM tickets t JOIN services s ON s.id = t.service_id WHERE t.id = $1",
        )
        .bind(job.ticket_id)
        .fetch_optional(&self.pool)
        .await?;
        if mode.as_deref() == Some("watch") {
            self.queue.defer(&job, 120.0).await?;
            return Ok(RunResult::Waiting);
        }
        let work = async {
            match job.kind.as_str() {
                "triage" => steps::triage::run(self, &job).await,
                "develop" => steps::develop::run(self, &job).await,
                "pr" => steps::pr::run(self, &job).await,
                "ci" => steps::ci::run(self, &job).await,
                "review" => steps::review::run(self, &job).await,
                "merge" => steps::merge::run(self, &job).await,
                other => Err(Error::Fatal(format!("unknown job kind {other}"))),
            }
        };
        match self.hold_lease(&job, work).await {
            Err(Error::Queue(tasklane_queue::Error::LeaseLost)) => Ok(RunResult::LeaseLost),
            Err(Error::WrongState { .. }) => {
                self.finish_job_quietly(&job).await?;
                Ok(RunResult::Skipped)
            }
            Err(Error::Fatal(msg)) => self.fail_ticket(&job, &msg, true).await,
            Err(e @ (Error::Plugin(_) | Error::BadOutput(_) | Error::Git(_))) => {
                eprintln!("job {} ({}): {e}", job.id, job.kind);
                match self.queue.fail(&job).await {
                    Ok(Outcome::Requeued) => Ok(RunResult::Retried),
                    Ok(Outcome::Failed) => self.fail_ticket(&job, &e.to_string(), false).await,
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

    pub(crate) async fn load_ticket(&self, ticket_id: i64) -> Result<Ticket, Error> {
        #[allow(clippy::type_complexity)]
        let r: (String, String, String, Option<i16>, Option<String>, i16, i16, Option<String>, Option<String>, Option<String>, Option<String>, Value, Value, String) =
            sqlx::query_as(
                "SELECT t.state, t.external_ref, s.external_id, t.level, t.plan_hash, t.ci_runs, t.dev_attempts,
                        t.head_sha, t.branch, t.pr_ref, t.last_error, s.facts, s.agent_profile, s.mode
                 FROM tickets t JOIN services s ON s.id = t.service_id WHERE t.id = $1",
            )
            .bind(ticket_id)
            .fetch_one(&self.pool)
            .await?;
        let state = State::parse(&r.0).ok_or_else(|| Error::UnknownState(r.0.clone()))?;
        let level =
            r.3.and_then(|l| u8::try_from(l).ok())
                .and_then(|l| Level::try_from(l).ok());
        Ok(Ticket {
            id: ticket_id,
            state,
            external_ref: r.1,
            service: r.2,
            level,
            plan_hash: r.4,
            ci_runs: r.5,
            dev_attempts: r.6,
            head_sha: r.7,
            branch: r.8,
            pr_ref: r.9,
            last_error: r.10,
            facts: r.11,
            agent_profile: r.12,
            mode: r.13,
        })
    }

    /// Begin a transaction that holds the job row; fails with `LeaseLost` if
    /// the lease or fence no longer matches.
    pub(crate) async fn begin(&self, job: &ClaimedJob) -> Result<Tx, Error> {
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
        Ok(tx)
    }

    /// Move a ticket if (and only if) the state machine allows it.
    pub(crate) async fn set_state(
        &self,
        tx: &mut Tx,
        ticket_id: i64,
        from: State,
        to: State,
        actor: Actor,
    ) -> Result<(), Error> {
        transition(from, to, actor)?;
        let r = sqlx::query(
            "UPDATE tickets SET state = $3, updated_at = now() WHERE id = $1 AND state = $2",
        )
        .bind(ticket_id)
        .bind(from.as_str())
        .bind(to.as_str())
        .execute(&mut **tx)
        .await?;
        if r.rows_affected() != 1 {
            return Err(Error::WrongState {
                ticket_id,
                found: from,
                wanted: "the state this step started from",
            });
        }
        Ok(())
    }

    pub(crate) async fn audit(
        &self,
        tx: &mut Tx,
        ticket_id: i64,
        action: &str,
        detail: Value,
    ) -> Result<(), Error> {
        sqlx::query("INSERT INTO audit_events (actor, action, ticket_id, detail) VALUES ('orchestrator', $1, $2, $3)")
            .bind(action)
            .bind(ticket_id)
            .bind(detail)
            .execute(&mut **tx)
            .await?;
        Ok(())
    }

    pub(crate) async fn enqueue(
        &self,
        tx: &mut Tx,
        ticket_id: i64,
        kind: &str,
        delay_secs: f64,
    ) -> Result<(), Error> {
        sqlx::query("INSERT INTO jobs (ticket_id, kind, run_after) VALUES ($1, $2, now() + make_interval(secs => $3))")
            .bind(ticket_id)
            .bind(kind)
            .bind(delay_secs)
            .execute(&mut **tx)
            .await?;
        Ok(())
    }

    pub(crate) async fn finish_job(&self, tx: &mut Tx, job: &ClaimedJob) -> Result<(), Error> {
        sqlx::query(
            "UPDATE jobs SET status = 'done', lease_owner = NULL, lease_until = NULL WHERE id = $1",
        )
        .bind(job.id)
        .execute(&mut **tx)
        .await?;
        Ok(())
    }

    async fn finish_job_quietly(&self, job: &ClaimedJob) -> Result<(), Error> {
        match self.queue.complete(job).await {
            Ok(()) | Err(tasklane_queue::Error::LeaseLost) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    /// The job ran out of attempts or hit a dead end: the ticket goes to `failed`
    /// with the reason, and a person is told on the ticket.
    /// `job_held` is true when the job is still running under this worker's lease
    /// (a dead end); false when the queue already marked it failed (attempts used up).
    async fn fail_ticket(
        &self,
        job: &ClaimedJob,
        reason: &str,
        job_held: bool,
    ) -> Result<RunResult, Error> {
        let mut tx = if job_held {
            match self.begin(job).await {
                Ok(tx) => tx,
                Err(Error::Queue(tasklane_queue::Error::LeaseLost)) => {
                    return Ok(RunResult::LeaseLost)
                }
                Err(e) => return Err(e),
            }
        } else {
            self.pool.begin().await?
        };
        let t = self.load_ticket(job.ticket_id).await?;
        if t.state.is_terminal() || t.state == State::Failed {
            if job_held {
                self.finish_job(&mut tx, job).await?;
            }
            tx.commit().await?;
            return Ok(RunResult::Skipped);
        }
        match self
            .set_state(&mut tx, t.id, t.state, State::Failed, Actor::Orchestrator)
            .await
        {
            Ok(()) => {}
            Err(Error::WrongState { .. }) => {
                if job_held {
                    self.finish_job(&mut tx, job).await?;
                }
                tx.commit().await?;
                return Ok(RunResult::Skipped);
            }
            Err(e) => return Err(e),
        }
        sqlx::query("UPDATE tickets SET last_error = $2 WHERE id = $1")
            .bind(t.id)
            .bind(reason)
            .execute(&mut *tx)
            .await?;
        self.audit(
            &mut tx,
            t.id,
            "ticket.failed",
            json!({"from": t.state.as_str(), "job": job.kind, "reason": reason}),
        )
        .await?;
        if job_held {
            // mark the job failed (not done) so the console can show it
            sqlx::query("UPDATE jobs SET status = 'failed', lease_owner = NULL, lease_until = NULL WHERE id = $1")
                .bind(job.id)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        self.say(
            &t,
            &format!("tasklane stopped on this ticket and needs a person: {reason}"),
        )
        .await;
        Ok(RunResult::GaveUp)
    }

    /// Fail a ticket whose last job was given up by the reaper (the worker kept dying).
    pub async fn fail_orphaned(
        &self,
        job_id: i64,
        ticket_id: i64,
        reason: &str,
    ) -> Result<(), Error> {
        let t = self.load_ticket(ticket_id).await?;
        if t.state.is_terminal() || t.state == State::Failed {
            return Ok(());
        }
        let mut tx = self.pool.begin().await?;
        if self
            .set_state(&mut tx, t.id, t.state, State::Failed, Actor::Orchestrator)
            .await
            .is_err()
        {
            return Ok(());
        }
        sqlx::query("UPDATE tickets SET last_error = $2 WHERE id = $1")
            .bind(t.id)
            .bind(reason)
            .execute(&mut *tx)
            .await?;
        self.audit(
            &mut tx,
            t.id,
            "ticket.failed",
            json!({"from": t.state.as_str(), "job": job_id, "reason": reason}),
        )
        .await?;
        tx.commit().await?;
        self.say(
            &t,
            "tasklane stopped on this ticket and needs a person. The details are in the console.",
        )
        .await;
        Ok(())
    }

    /// What the two plug-ins say about themselves (`None` if one does not answer in time).
    pub async fn describe_plugins(&self) -> (Option<Value>, Option<Value>) {
        let t = Duration::from_secs(5);
        let src = self.source.call("describe", json!({}), t).await.ok();
        let eng = self.engine.call("describe", json!({}), t).await.ok();
        (src, eng)
    }

    /// Is the ticket source set up? (Plug-ins that do not say are assumed to be.)
    pub async fn source_configured(&self) -> bool {
        match self
            .source
            .call("describe", json!({}), Duration::from_secs(5))
            .await
        {
            Ok(v) => v.get("configured").and_then(Value::as_bool).unwrap_or(true),
            Err(_) => false,
        }
    }

    /// Both plug-ins answer? Any reply counts (even "unknown operation"); silence or a dead process does not.
    pub async fn plugins_ready(&self) -> bool {
        use crate::host::CallError;
        let t = Duration::from_secs(5);
        let alive = |r: Result<Value, CallError>| {
            !matches!(
                r,
                Err(CallError::Timeout) | Err(CallError::Closed) | Err(CallError::Io(_))
            )
        };
        alive(self.source.call("describe", json!({}), t).await)
            && alive(self.engine.call("describe", json!({}), t).await)
    }

    /// Remove checkouts left behind by crashed runs.
    pub async fn sweep_workspaces(&self) {
        self.ws.sweep(Duration::from_secs(3600)).await;
    }

    /// Old bookkeeping rows: finished jobs, audit events, console sessions, chat hints.
    pub async fn prune_old_rows(&self, retention_days: i64) -> Result<(), Error> {
        sqlx::query("DELETE FROM audit_events WHERE at < now() - make_interval(days => $1::int)")
            .bind(retention_days)
            .execute(&self.pool)
            .await?;
        sqlx::query("DELETE FROM jobs WHERE status IN ('done','failed') AND created_at < now() - interval '30 days'").execute(&self.pool).await?;
        sqlx::query("DELETE FROM console_sessions WHERE expires_at < now()")
            .execute(&self.pool)
            .await?;
        sqlx::query("DELETE FROM sync_state WHERE key LIKE 'chat_hint:%' AND updated_at < now() - interval '7 days'").execute(&self.pool).await?;
        Ok(())
    }

    /// Post a short status on the ticket. Never fails the step.
    pub(crate) async fn say(&self, t: &Ticket, text: &str) {
        let r = self
            .source
            .call(
                "comment",
                json!({"service": t.service, "ref": t.external_ref, "body": text}),
                self.cfg.call_timeout,
            )
            .await;
        if let Err(e) = r {
            eprintln!("ticket {}: could not post status: {e}", t.id);
        }
    }

    pub(crate) async fn run_token(
        &self,
        t: &Ticket,
        permissions: &[&str],
    ) -> Result<String, Error> {
        let r = self
            .source
            .call(
                "mint_run_token",
                json!({"service": t.service, "permissions": permissions, "ttl_secs": self.cfg.token_ttl_secs}),
                self.cfg.call_timeout,
            )
            .await?;
        r.get("token")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| Error::BadOutput("mint_run_token without token".into()))
    }

    pub(crate) async fn repo_url(&self, t: &Ticket, token: Option<&str>) -> Result<String, Error> {
        let r = self
            .source
            .call(
                "repo_url",
                json!({"service": t.service, "token": token}),
                self.cfg.call_timeout,
            )
            .await?;
        r.get("url")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| Error::BadOutput("repo_url without url".into()))
    }

    pub(crate) async fn item(&self, t: &Ticket) -> Result<Value, Error> {
        Ok(self
            .source
            .call(
                "get_item",
                json!({"service": t.service, "ref": t.external_ref}),
                self.cfg.call_timeout,
            )
            .await?)
    }
}
