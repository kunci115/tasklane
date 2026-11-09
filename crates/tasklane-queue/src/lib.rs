//! Job queue on Postgres. See docs/design/01-flow-and-state-machine.md, section 5.
//!
//! * `claim` takes one job with `FOR UPDATE SKIP LOCKED`, so two workers never
//!   get the same job and nobody waits for a lock.
//! * Every claim bumps `fence`. Every later write (heartbeat, checkpoint,
//!   complete, fail) must present the fence it was given; a stale worker's write
//!   changes nothing and returns `Error::LeaseLost`.
//! * `reap` returns jobs whose lease ran out to the queue (with backoff), or
//!   fails them when attempts are used up.

use serde_json::Value;
use sqlx::{FromRow, PgPool};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("lease lost: the job was reclaimed, finished or reaped")]
    LeaseLost,
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

/// A job held by one worker. Keep it; every call after `claim` needs it.
#[derive(Debug, Clone)]
pub struct ClaimedJob {
    pub id: i64,
    pub ticket_id: i64,
    pub kind: String,
    pub fence: i64,
    pub attempts: i16,
    pub checkpoint: Value,
    owner: String,
}

impl ClaimedJob {
    /// The worker name this job was claimed under.
    pub fn owner(&self) -> &str {
        &self.owner
    }
}

#[derive(Debug, FromRow)]
struct ClaimRow {
    id: i64,
    ticket_id: i64,
    kind: String,
    fence: i64,
    attempts: i16,
    checkpoint: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Requeued,
    Failed,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct ReapReport {
    pub requeued: u64,
    pub failed: u64,
}

#[derive(Clone)]
pub struct Queue {
    pool: PgPool,
    max_attempts: i16,
}

/// Delay before a requeued job may run again: 10 s, doubling, capped at 1 hour.
pub fn backoff_secs(attempts: i16) -> f64 {
    (10.0 * 2f64.powi(attempts.max(0) as i32 - 1)).min(3600.0)
}

impl Queue {
    pub fn new(pool: PgPool, max_attempts: i16) -> Self {
        Queue { pool, max_attempts }
    }

    pub async fn enqueue(
        &self,
        ticket_id: i64,
        kind: &str,
        priority: i16,
        delay_secs: f64,
    ) -> Result<i64, Error> {
        let (id,): (i64,) = sqlx::query_as(
            "INSERT INTO jobs (ticket_id, kind, priority, run_after)
             VALUES ($1, $2, $3, now() + make_interval(secs => $4)) RETURNING id",
        )
        .bind(ticket_id)
        .bind(kind)
        .bind(priority)
        .bind(delay_secs)
        .fetch_one(&self.pool)
        .await?;
        Ok(id)
    }

    /// Claim the best runnable job of the given kinds (empty = any kind).
    pub async fn claim(
        &self,
        owner: &str,
        lease_secs: f64,
        kinds: &[&str],
    ) -> Result<Option<ClaimedJob>, Error> {
        let kinds: Vec<String> = kinds.iter().map(|k| k.to_string()).collect();
        let row: Option<ClaimRow> = sqlx::query_as(
            "UPDATE jobs
             SET lease_owner = $1,
                 lease_until = now() + make_interval(secs => $2),
                 fence       = fence + 1,
                 attempts    = attempts + 1,
                 status      = 'running'
             WHERE id = (
               SELECT id FROM jobs
               WHERE status = 'queued' AND run_after <= now()
                 AND (cardinality($3::text[]) = 0 OR kind = ANY($3))
               ORDER BY priority DESC, run_after, id
               FOR UPDATE SKIP LOCKED
               LIMIT 1)
             RETURNING id, ticket_id, kind, fence, attempts, checkpoint",
        )
        .bind(owner)
        .bind(lease_secs)
        .bind(&kinds)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| ClaimedJob {
            id: r.id,
            ticket_id: r.ticket_id,
            kind: r.kind,
            fence: r.fence,
            attempts: r.attempts,
            checkpoint: r.checkpoint,
            owner: owner.to_string(),
        }))
    }

    pub async fn heartbeat(&self, job: &ClaimedJob, lease_secs: f64) -> Result<(), Error> {
        let r = sqlx::query(
            "UPDATE jobs SET lease_until = now() + make_interval(secs => $4)
             WHERE id = $1 AND fence = $2 AND lease_owner = $3 AND status = 'running'",
        )
        .bind(job.id)
        .bind(job.fence)
        .bind(&job.owner)
        .bind(lease_secs)
        .execute(&self.pool)
        .await?;
        held(r.rows_affected())
    }

    pub async fn checkpoint(&self, job: &ClaimedJob, data: &Value) -> Result<(), Error> {
        let r = sqlx::query(
            "UPDATE jobs SET checkpoint = $4
             WHERE id = $1 AND fence = $2 AND lease_owner = $3 AND status = 'running'",
        )
        .bind(job.id)
        .bind(job.fence)
        .bind(&job.owner)
        .bind(data)
        .execute(&self.pool)
        .await?;
        held(r.rows_affected())
    }

    pub async fn complete(&self, job: &ClaimedJob) -> Result<(), Error> {
        let r = sqlx::query(
            "UPDATE jobs SET status = 'done', lease_owner = NULL, lease_until = NULL
             WHERE id = $1 AND fence = $2 AND lease_owner = $3 AND status = 'running'",
        )
        .bind(job.id)
        .bind(job.fence)
        .bind(&job.owner)
        .execute(&self.pool)
        .await?;
        held(r.rows_affected())
    }

    /// The worker gives the job back: it is retried with backoff, or failed if
    /// attempts are used up.
    pub async fn fail(&self, job: &ClaimedJob) -> Result<Outcome, Error> {
        let row: Option<(String,)> = sqlx::query_as(
            "UPDATE jobs
             SET status = CASE WHEN attempts >= $4 THEN 'failed' ELSE 'queued' END,
                 run_after = now() + make_interval(secs => $5),
                 lease_owner = NULL, lease_until = NULL
             WHERE id = $1 AND fence = $2 AND lease_owner = $3 AND status = 'running'
             RETURNING status",
        )
        .bind(job.id)
        .bind(job.fence)
        .bind(&job.owner)
        .bind(self.max_attempts)
        .bind(backoff_secs(job.attempts))
        .fetch_optional(&self.pool)
        .await?;
        match row {
            Some((s,)) if s == "failed" => Ok(Outcome::Failed),
            Some(_) => Ok(Outcome::Requeued),
            None => Err(Error::LeaseLost),
        }
    }

    /// Put a running job back without counting this claim as an attempt, to run
    /// again after `delay_secs` (used to wait for CI or for a free development slot).
    pub async fn defer(&self, job: &ClaimedJob, delay_secs: f64) -> Result<(), Error> {
        let r = sqlx::query(
            "UPDATE jobs
             SET status = 'queued', attempts = attempts - 1,
                 run_after = now() + make_interval(secs => $4),
                 lease_owner = NULL, lease_until = NULL
             WHERE id = $1 AND fence = $2 AND lease_owner = $3 AND status = 'running'",
        )
        .bind(job.id)
        .bind(job.fence)
        .bind(&job.owner)
        .bind(delay_secs)
        .execute(&self.pool)
        .await?;
        held(r.rows_affected())
    }

    /// Free every job whose lease ran out. Safe to run from many processes.
    pub async fn reap(&self) -> Result<ReapReport, Error> {
        Ok(self.reap_detailed().await?.0)
    }

    /// Like `reap`, and also returns `(job id, ticket id)` of every job that was
    /// failed for good, so the caller can fail the ticket and tell a person.
    pub async fn reap_detailed(&self) -> Result<(ReapReport, Vec<(i64, i64)>), Error> {
        let rows: Vec<(String, i64, i64)> = sqlx::query_as(
            "UPDATE jobs
             SET status = CASE WHEN attempts >= $1 THEN 'failed' ELSE 'queued' END,
                 run_after = now() + make_interval(secs => LEAST(10 * power(2, GREATEST(attempts - 1, 0)), 3600)),
                 lease_owner = NULL, lease_until = NULL
             WHERE status = 'running' AND lease_until < now()
             RETURNING status, id, ticket_id",
        )
        .bind(self.max_attempts)
        .fetch_all(&self.pool)
        .await?;
        let dead: Vec<(i64, i64)> = rows
            .iter()
            .filter(|(s, _, _)| s == "failed")
            .map(|(_, j, t)| (*j, *t))
            .collect();
        let failed = dead.len() as u64;
        Ok((
            ReapReport {
                requeued: rows.len() as u64 - failed,
                failed,
            },
            dead,
        ))
    }
}

fn held(rows: u64) -> Result<(), Error> {
    if rows == 1 {
        Ok(())
    } else {
        Err(Error::LeaseLost)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_and_is_capped() {
        assert_eq!(backoff_secs(1), 10.0);
        assert_eq!(backoff_secs(2), 20.0);
        assert_eq!(backoff_secs(3), 40.0);
        assert_eq!(backoff_secs(20), 3600.0);
    }
}
