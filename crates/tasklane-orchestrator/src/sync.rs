//! The sync job: keeps the service registry and the ticket table in step with
//! the ticket source. Webhooks are the fast path; this polling is the backstop.

use serde_json::{json, Value};
use tasklane_core::{transition, Actor, State};

use crate::ctx::MARKER;
use crate::{Error, Orchestrator};

#[derive(Debug, Default, PartialEq, Eq)]
pub struct SyncReport {
    pub services_seen: usize,
    pub tickets_created: usize,
    pub tickets_resumed: usize,
}

impl Orchestrator {
    /// One pass: refresh the registry, then pick up new and updated tickets.
    pub async fn sync_once(&self) -> Result<SyncReport, Error> {
        let mut report = SyncReport::default();
        let r = self
            .source
            .call("list_services", json!({}), self.cfg.call_timeout)
            .await?;
        let services = r
            .get("services")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for s in &services {
            let Some(id) = s.get("external_id").and_then(Value::as_str) else {
                continue;
            };
            let archived = s.get("archived").and_then(Value::as_bool).unwrap_or(false);
            let facts = json!({
                "default_branch": s.get("default_branch").cloned().unwrap_or(json!("main")),
                "private": s.get("private").cloned().unwrap_or(json!(true)),
            });
            // New repos start in `triage` (read and plan, never change) unless archived.
            let inserted: Option<(i64,)> = sqlx::query_as(
                "INSERT INTO services (kind, source, external_id, mode, facts, archived)
                 VALUES ('repo', $1, $2, $3, $4, $5)
                 ON CONFLICT (source, external_id) DO NOTHING RETURNING id",
            )
            .bind(&self.cfg.source_name)
            .bind(id)
            .bind(if archived { "watch" } else { "triage" })
            .bind(&facts)
            .bind(archived)
            .fetch_optional(&self.pool)
            .await?;
            if inserted.is_some() && !archived {
                // Facts that need a look at the repo: best effort, never blocks the sync.
                match self
                    .source
                    .call(
                        "detect_facts",
                        json!({"service": id}),
                        self.cfg.call_timeout,
                    )
                    .await
                {
                    Ok(f) => {
                        sqlx::query("UPDATE services SET facts = facts || $2 WHERE source = $1 AND external_id = $3")
                            .bind(&self.cfg.source_name)
                            .bind(&f)
                            .bind(id)
                            .execute(&self.pool)
                            .await?;
                    }
                    Err(e) => eprintln!("sync: detect_facts for {id}: {e}"),
                }
            } else {
                sqlx::query("UPDATE services SET archived = $3, facts = facts || $4 WHERE source = $1 AND external_id = $2")
                    .bind(&self.cfg.source_name)
                    .bind(id)
                    .bind(archived)
                    .bind(&facts)
                    .execute(&self.pool)
                    .await?;
            }
            report.services_seen += 1;
        }
        // A repository the source no longer lists (the account or App was switched, access
        // was removed) must not stay on the board. Archived, not deleted: history stays.
        let listed: Vec<&str> = services
            .iter()
            .filter_map(|s| s.get("external_id").and_then(Value::as_str))
            .collect();
        sqlx::query(
            "UPDATE services SET archived = true
             WHERE source = $1 AND NOT archived AND NOT (external_id = ANY($2))",
        )
        .bind(&self.cfg.source_name)
        .bind(&listed)
        .execute(&self.pool)
        .await?;

        let cursor: Option<String> =
            sqlx::query_scalar("SELECT value FROM sync_state WHERE key = 'changes_cursor'")
                .fetch_optional(&self.pool)
                .await?;
        let r = self
            .source
            .call(
                "list_changes",
                json!({"cursor": cursor}),
                self.cfg.call_timeout,
            )
            .await?;
        let items = r
            .get("items")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut deferred = false;
        for it in &items {
            if it.get("kind").and_then(Value::as_str) != Some("issue") {
                continue;
            }
            let (Some(service), Some(reference)) = (
                it.get("service").and_then(Value::as_str),
                it.get("ref").and_then(Value::as_str),
            ) else {
                continue;
            };
            match self.note_issue(service, reference).await? {
                Noted::Created => report.tickets_created += 1,
                Noted::Resumed => report.tickets_resumed += 1,
                Noted::Deferred => deferred = true,
                Noted::Nothing => {}
            }
        }
        // Advance the cursor only after every item was handled (not while some were held back).
        if deferred {
            return Ok(report);
        }
        if let Some(c) = r.get("cursor").and_then(Value::as_str) {
            sqlx::query("INSERT INTO sync_state (key, value) VALUES ('changes_cursor', $1) ON CONFLICT (key) DO UPDATE SET value = $1, updated_at = now()")
                .bind(c)
                .execute(&self.pool)
                .await?;
        }
        Ok(report)
    }

    /// A person asked for the issues that were already open to become tickets (the first poll
    /// only sets a baseline, so they were never imported). Returns (created, held back, more on the source).
    pub async fn import_open_issues(&self, service_id: i64) -> Result<(usize, usize, bool), Error> {
        let ext = self.external_id(service_id).await?;
        let r = self
            .source
            .call(
                "list_open_issues",
                json!({"service": ext}),
                self.cfg.call_timeout,
            )
            .await?;
        let refs: Vec<String> = r
            .get("refs")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        let more = r.get("more").and_then(Value::as_bool) == Some(true);
        let (mut created, mut held) = (0, 0);
        for reference in &refs {
            match self.note_issue(&ext, reference).await? {
                Noted::Created => created += 1,
                Noted::Deferred => held += 1,
                _ => {}
            }
        }
        let _ = sqlx::query("INSERT INTO audit_events (actor, action, detail) VALUES ('console:admin', 'service.import', $1)")
            .bind(json!({"service_id": service_id, "created": created, "held_back": held}))
            .execute(&self.pool)
            .await;
        Ok((created, held, more))
    }

    /// An issue exists or changed on the source. Create its ticket the first
    /// time; resume it if it was waiting for the reporter and a person answered.
    pub async fn note_issue(&self, service: &str, reference: &str) -> Result<Noted, Error> {
        let svc: Option<(i64, String, bool)> = sqlx::query_as(
            "SELECT id, CASE WHEN overrides->>'mode_cap' = 'watch' THEN 'watch' ELSE mode END, archived FROM services WHERE source = $1 AND external_id = $2",
        )
        .bind(&self.cfg.source_name)
        .bind(service)
        .fetch_optional(&self.pool)
        .await?;
        let Some((service_id, mode, archived)) = svc else {
            return Ok(Noted::Nothing);
        };
        if archived || mode == "watch" {
            return Ok(Noted::Nothing);
        }
        // Another install owns this repository: leave its issues alone.
        if let crate::Owner::Foreign(who) = self.ensure_owner(service_id, service).await? {
            eprintln!("sync: {service} is handled by {who}; skipping issue {reference}");
            return Ok(Noted::Nothing);
        }
        let known: Option<i64> = sqlx::query_scalar(
            "SELECT id FROM tickets WHERE service_id = $1 AND external_ref = $2",
        )
        .bind(service_id)
        .bind(reference)
        .fetch_optional(&self.pool)
        .await?;
        if known.is_none() {
            let waiting: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM jobs WHERE kind = 'triage' AND status = 'queued'",
            )
            .fetch_one(&self.pool)
            .await?;
            if waiting >= self.cfg.max_pending_triage {
                return Ok(Noted::Deferred);
            }
            // Only open issues become tickets: not pull requests (events for them look like issues), not closed ones.
            let item = self
                .source
                .call(
                    "get_item",
                    json!({"service": service, "ref": reference}),
                    self.cfg.call_timeout,
                )
                .await?;
            if item.get("is_pull_request").and_then(Value::as_bool) == Some(true)
                || item.get("state").and_then(Value::as_str) == Some("closed")
            {
                return Ok(Noted::Nothing);
            }
        }
        let mut tx = self.pool.begin().await?;
        let created: Option<(i64,)> = sqlx::query_as(
            "INSERT INTO tickets (service_id, external_ref) VALUES ($1, $2)
             ON CONFLICT (service_id, external_ref) DO NOTHING RETURNING id",
        )
        .bind(service_id)
        .bind(reference)
        .fetch_optional(&mut *tx)
        .await?;
        if let Some((id,)) = created {
            sqlx::query("INSERT INTO jobs (ticket_id, kind) VALUES ($1, 'triage')")
                .bind(id)
                .execute(&mut *tx)
                .await?;
            sqlx::query("INSERT INTO audit_events (actor, action, ticket_id, detail) VALUES ('sync', 'ticket.created', $1, $2)")
                .bind(id)
                .bind(json!({"service": service, "ref": reference}))
                .execute(&mut *tx)
                .await?;
            tx.commit().await?;
            return Ok(Noted::Created);
        }
        // Existing ticket: only one case matters here.
        let row: Option<(i64, String)> = sqlx::query_as(
            "SELECT id, state FROM tickets WHERE service_id = $1 AND external_ref = $2 FOR UPDATE",
        )
        .bind(service_id)
        .bind(reference)
        .fetch_optional(&mut *tx)
        .await?;
        let Some((id, state)) = row else {
            return Ok(Noted::Nothing);
        };
        if State::parse(&state) != Some(State::NeedsInfo) {
            return Ok(Noted::Nothing);
        }
        drop(tx); // do the slow source call without holding the row lock
        let item = self
            .source
            .call(
                "get_item",
                json!({"service": service, "ref": reference}),
                self.cfg.call_timeout,
            )
            .await?;
        let last = item
            .get("comments")
            .and_then(Value::as_array)
            .and_then(|c| c.last());
        let ours = |c: &Value| {
            c.get("mine").and_then(Value::as_bool) == Some(true)
                || c.get("body")
                    .and_then(Value::as_str)
                    .is_some_and(|b| b.contains(MARKER))
        };
        match last {
            Some(c) if !ours(c) => {}
            _ => return Ok(Noted::Nothing), // our own question is still the last word
        }
        let mut tx = self.pool.begin().await?;
        transition(State::NeedsInfo, State::New, Actor::Reporter)?;
        let r = sqlx::query("UPDATE tickets SET state = 'new', updated_at = now() WHERE id = $1 AND state = 'needs_info'")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        if r.rows_affected() != 1 {
            return Ok(Noted::Nothing);
        }
        sqlx::query("INSERT INTO jobs (ticket_id, kind) VALUES ($1, 'triage')")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("INSERT INTO audit_events (actor, action, ticket_id, detail) VALUES ('sync', 'ticket.resumed', $1, '{}')").bind(id).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(Noted::Resumed)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Noted {
    /// Held back because too many triage jobs are waiting; looked at again on a later pass.
    Deferred,
    Created,
    Resumed,
    Nothing,
}
