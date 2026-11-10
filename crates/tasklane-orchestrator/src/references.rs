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

    fn tool_secret(&self, id: i64) -> Option<String> {
        std::fs::read_to_string(self.cfg.data_dir.join("secrets").join(format!("tool_{id}")))
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    /// The tools the owner granted for a run of `role` in `service`: the grants for every repository and
    /// for this one that name the role. The credential goes to the engine's broker, never to the agent.
    pub async fn tools_for(&self, service: &str, role: &str) -> Vec<Value> {
        #[allow(clippy::type_complexity)]
        let rows: Vec<(i64, String, String, String, Option<String>, bool, i32)> = sqlx::query_as(
            "SELECT t.id, t.name, t.description, t.base_url, t.header_name, t.has_secret, t.max_calls
             FROM agent_tools t LEFT JOIN services s ON s.id = t.service_id
             WHERE t.enabled AND $3 = ANY(t.roles) AND (t.service_id IS NULL OR (s.source = $1 AND s.external_id = $2))
             ORDER BY t.service_id NULLS FIRST, t.id LIMIT 12",
        )
        .bind(&self.cfg.source_name)
        .bind(service)
        .bind(role)
        .fetch_all(&self.pool)
        .await
        .unwrap_or_default();
        let mut seen = std::collections::HashSet::new();
        let mut out = vec![];
        // a repository's own grant of the same name replaces the global one
        for (id, name, description, base_url, header_name, has_secret, max_calls) in
            rows.into_iter().rev()
        {
            if !seen.insert(name.clone()) {
                continue;
            }
            let value = if has_secret {
                self.tool_secret(id)
            } else {
                None
            };
            out.push(json!({"id": id, "name": name, "description": description, "base_url": base_url,
                            "header_name": header_name, "header_value": value, "max_calls": max_calls}));
        }
        out.reverse();
        out
    }

    /// Count the calls an engine run reported (`usage.tool_calls`: grant id to calls).
    pub(crate) async fn count_tool_calls(&self, out: &Value) {
        let Some(calls) = out.pointer("/usage/tool_calls").and_then(Value::as_object) else {
            return;
        };
        for (id, n) in calls {
            if let (Ok(id), Some(n)) = (id.parse::<i64>(), n.as_i64()) {
                let _ = sqlx::query(
                    "UPDATE agent_tools SET calls_total = calls_total + $2 WHERE id = $1",
                )
                .bind(id)
                .bind(n.clamp(0, 1000))
                .execute(&self.pool)
                .await;
            }
        }
    }

    /// Try a tool as the agent would, with one path. Returns the text or the reason it failed.
    pub async fn test_tool(&self, id: i64, path: &str) -> Result<String, String> {
        let row: Option<(String, Option<String>, bool)> = sqlx::query_as(
            "SELECT base_url, header_name, has_secret FROM agent_tools WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| e.to_string())?;
        let Some((base_url, header_name, has_secret)) = row else {
            return Err("unknown tool".into());
        };
        let value = if has_secret {
            self.tool_secret(id)
        } else {
            None
        };
        self.engine_call_with(
            "tool_test",
            json!({"base_url": base_url, "header_name": header_name, "header_value": value, "path": path}),
            Duration::from_secs(30),
        )
        .await
        .map(|v| v.get("text").and_then(Value::as_str).unwrap_or("").to_string())
        .map_err(|e| e.to_string().chars().take(300).collect())
    }
}
