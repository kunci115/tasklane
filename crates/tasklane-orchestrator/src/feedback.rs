//! Changes asked for on the pull request by people. A comment or a review that asks for changes,
//! from someone with a real relationship to the repository (the source plug-in leaves strangers and
//! tasklane's own comments out), sends the ticket back to the developer with their words as feedback.
//! The change then goes through the same re-score, CI and review as any other, so a protected path or
//! a bigger diff still asks a person.

use serde_json::{json, Value};
use tasklane_core::{Actor, State};

use crate::{Error, Orchestrator};

const LIVE: &str = "('pr_open', 'ci_running', 'reviewing', 'merge_ready')";

impl Orchestrator {
    /// Look at every open pull request of a ticket still on its way. Returns how many were sent back.
    pub async fn sync_pr_feedback(&self) -> Result<usize, Error> {
        let rows: Vec<(i64, String, Option<String>, String)> = sqlx::query_as(&format!(
            "SELECT t.id, t.pr_ref, t.feedback_since, to_char(t.created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"')
             FROM tickets t JOIN services s ON s.id = t.service_id
             WHERE t.state IN {LIVE} AND t.pr_ref IS NOT NULL AND s.mode <> 'watch' AND s.owner_state <> 'foreign'
             ORDER BY t.id LIMIT 50"
        ))
        .fetch_all(&self.pool)
        .await?;
        let mut sent_back = 0;
        for (id, pr, since, created) in rows {
            let Ok(t) = self.load_ticket(id).await else {
                continue;
            };
            let since = since.unwrap_or(created);
            let Ok(r) = self
                .source
                .call(
                    "list_pr_feedback",
                    json!({"service": t.service, "pr": pr, "since": since}),
                    self.cfg.call_timeout,
                )
                .await
            else {
                continue; // a source without this operation, or a failed call: try again later
            };
            let items = r
                .get("items")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let cursor = r
                .get("cursor")
                .and_then(Value::as_str)
                .unwrap_or(&since)
                .to_string();
            if items.is_empty() {
                continue;
            }
            let said: String = items
                .iter()
                .filter_map(|i| {
                    Some(format!(
                        "- {} ({}): {}",
                        i.get("author")?.as_str()?,
                        i.get("kind").and_then(Value::as_str).unwrap_or("comment"),
                        i.get("body")?.as_str()?
                    ))
                })
                .collect::<Vec<_>>()
                .join("\n");
            let mut tx = self.pool.begin().await?;
            sqlx::query("UPDATE tickets SET feedback_since = $2 WHERE id = $1")
                .bind(id)
                .bind(&cursor)
                .execute(&mut *tx)
                .await?;
            match self
                .set_state(&mut tx, id, t.state, State::Developing, Actor::Orchestrator)
                .await
            {
                Ok(()) => {}
                Err(Error::WrongState { .. }) => continue, // it moved meanwhile; the cursor is kept
                Err(e) => return Err(e),
            }
            sqlx::query("UPDATE tickets SET last_error = $2 WHERE id = $1")
                .bind(id)
                .bind(super::steps::develop::cap(&format!(
                    "People asked for changes on the pull request:\n{said}"
                )))
                .execute(&mut *tx)
                .await?;
            // work that was waiting (CI, review, merge) is no longer about this commit
            sqlx::query("UPDATE jobs SET status = 'done', lease_owner = NULL WHERE ticket_id = $1 AND status = 'queued'")
                .bind(id)
                .execute(&mut *tx)
                .await?;
            sqlx::query("INSERT INTO jobs (ticket_id, kind) VALUES ($1, 'develop')")
                .bind(id)
                .execute(&mut *tx)
                .await?;
            self.audit(
                &mut tx,
                id,
                "ticket.pr_feedback",
                json!({"comments": items.len()}),
            )
            .await?;
            tx.commit().await?;
            self.say(
                &t,
                "Thanks, I read the comments on the pull request and I am working on the changes asked for.",
            )
            .await;
            sent_back += 1;
        }
        Ok(sent_back)
    }
}
