//! Messages to the agent on a ticket. A person with approval rights writes one; the next run on the
//! ticket is given it as a team message. A run that is going on can be stopped so that the work starts
//! again at once with the message (the plug-in ends the run; the job goes back to the queue and no
//! attempt is used up).

use serde_json::{json, Value};

use crate::{Error, Orchestrator};

pub const MAX_CHARS: usize = 10_000;

impl Orchestrator {
    /// Record a message. `author` is `channel:ref`.
    pub async fn add_message(
        &self,
        ticket_id: i64,
        author: &str,
        body: &str,
    ) -> Result<i64, Error> {
        let body = body.trim();
        let n = body.chars().count();
        if n == 0 || n > MAX_CHARS {
            return Err(Error::Fatal(format!(
                "a message needs 1 to {MAX_CHARS} characters"
            )));
        }
        Ok(sqlx::query_scalar(
            "INSERT INTO ticket_messages (ticket_id, author, body) VALUES ($1, $2, $3) RETURNING id",
        )
        .bind(ticket_id)
        .bind(author)
        .bind(body)
        .fetch_one(&self.pool)
        .await?)
    }

    /// The messages not yet given to a run, oldest first: the engine's `messages` and their ids.
    pub(crate) async fn pending_messages(&self, ticket_id: i64) -> (Vec<Value>, Vec<i64>) {
        let rows: Vec<(i64, String, String)> = sqlx::query_as(
            "SELECT id, author, body FROM ticket_messages WHERE ticket_id = $1 AND delivered_at IS NULL ORDER BY id LIMIT 10",
        )
        .bind(ticket_id)
        .fetch_all(&self.pool)
        .await
        .unwrap_or_default();
        let ids = rows.iter().map(|r| r.0).collect();
        let msgs = rows
            .into_iter()
            .map(|(_, from, body)| json!({"from": from, "body": body}))
            .collect();
        (msgs, ids)
    }

    pub(crate) async fn mark_messages_delivered(&self, ids: &[i64]) {
        if ids.is_empty() {
            return;
        }
        let _ = sqlx::query("UPDATE ticket_messages SET delivered_at = now() WHERE id = ANY($1)")
            .bind(ids)
            .execute(&self.pool)
            .await;
    }

    /// Stop the run going on for this ticket, if any. The plug-in is asked by a one-shot helper of the
    /// same program (the engine itself is busy with the run). Returns whether a run was found.
    pub async fn stop_running(&self, ticket_id: i64) -> bool {
        let spec = self.engine.spec();
        let key = format!("t{ticket_id}");
        let out = tokio::task::spawn_blocking(move || {
            std::process::Command::new(&spec.program)
                .args(&spec.args)
                .arg("--cancel")
                .arg(&key)
                .env_clear()
                .envs(spec.env.iter().map(|(k, v)| (k, v)))
                .output()
        })
        .await;
        match out {
            Ok(Ok(o)) => String::from_utf8_lossy(&o.stdout).contains("\"cancelled\": true"),
            _ => false,
        }
    }
}
