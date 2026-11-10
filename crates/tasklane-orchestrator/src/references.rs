//! Reference sources: documents the owner pointed the agents at. The daemon fetches them (through the
//! engine plug-in, which is the only part with network access) and hands the text to the agent as data;
//! the agent itself never gets network access or the credential used to read them.

use std::time::Duration;

use serde_json::{json, Value};

use crate::Orchestrator;

/// At most this many documents go to one run, and this many bytes of text in all.
const MAX_DOCS: i64 = 8;
const MAX_TOTAL: usize = 300_000;

impl Orchestrator {
    fn reference_secret(&self, id: i64) -> Option<String> {
        std::fs::read_to_string(self.cfg.data_dir.join("secrets").join(format!("ref_{id}")))
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    /// Fetch one source now and remember the result. Returns the text or the reason it failed.
    pub async fn fetch_reference(&self, id: i64) -> Result<String, String> {
        let row: Option<(String, Option<String>, bool, i32)> = sqlx::query_as(
            "SELECT url, header_name, has_secret, max_bytes FROM reference_sources WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| e.to_string())?;
        let Some((url, header_name, has_secret, max_bytes)) = row else {
            return Err("unknown source".into());
        };
        let value = if has_secret {
            self.reference_secret(id)
        } else {
            None
        };
        let r = self
            .engine_call_with(
                "reference",
                json!({"url": url, "header_name": header_name, "header_value": value, "max_bytes": max_bytes}),
                Duration::from_secs(30),
            )
            .await;
        match r {
            Ok(v) => {
                let text = v
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let _ = sqlx::query("UPDATE reference_sources SET last_text = $2, last_status = 'ok', last_fetched = now() WHERE id = $1")
                    .bind(id)
                    .bind(&text)
                    .execute(&self.pool)
                    .await;
                Ok(text)
            }
            Err(e) => {
                let why: String = e.to_string().chars().take(200).collect();
                let _ = sqlx::query("UPDATE reference_sources SET last_status = $2, last_fetched = now() WHERE id = $1")
                    .bind(id)
                    .bind(format!("failed: {why}"))
                    .execute(&self.pool)
                    .await;
                Err(why)
            }
        }
    }

    /// The documents for a run in `service` (an `external_id`; empty for work that belongs to no
    /// repository): the sources for every repository and for this one, each refetched when older than
    /// its lifetime. A source that cannot be fetched falls back to the last text it gave, else is left out.
    pub async fn references_for(&self, service: &str) -> Vec<Value> {
        let rows: Vec<(i64, String, bool, String)> = sqlx::query_as(
            "SELECT r.id, r.name, (r.last_fetched IS NULL OR r.last_fetched < now() - make_interval(secs => r.ttl_secs)) AS stale, r.last_text
             FROM reference_sources r LEFT JOIN services s ON s.id = r.service_id
             WHERE r.enabled AND (r.service_id IS NULL OR (s.source = $1 AND s.external_id = $2))
             ORDER BY r.service_id NULLS FIRST, r.id LIMIT $3",
        )
        .bind(&self.cfg.source_name)
        .bind(service)
        .bind(MAX_DOCS)
        .fetch_all(&self.pool)
        .await
        .unwrap_or_default();
        let mut out = vec![];
        let mut total = 0;
        for (id, name, stale, cached) in rows {
            let text = if stale {
                match self.fetch_reference(id).await {
                    Ok(t) => t,
                    Err(_) => cached,
                }
            } else {
                cached
            };
            if text.trim().is_empty() || total + text.len() > MAX_TOTAL {
                continue;
            }
            total += text.len();
            out.push(json!({"name": name, "text": text}));
        }
        out
    }
}
