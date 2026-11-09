//! Questions about a repository, answered by a read-only agent run. No ticket,
//! no approval, no change: the agent sees a detached checkout of the base branch.

use serde_json::{json, Value};

use crate::{Error, Orchestrator};

impl Orchestrator {
    /// Answer `question` about `service` (a registry `external_id`). `asker` is
    /// only recorded in the audit log.
    pub async fn ask(&self, service: &str, question: &str, _asker: &str) -> Result<String, Error> {
        let row: Option<(i64, Value, Value)> = sqlx::query_as("SELECT id, facts, agent_profile FROM services WHERE source = $1 AND external_id = $2 AND NOT archived")
            .bind(&self.cfg.source_name)
            .bind(service)
            .fetch_optional(&self.pool)
            .await?;
        let Some((service_id, facts, profile)) = row else {
            return Err(Error::Fatal(format!("unknown service {service}")));
        };
        let base = facts
            .get("default_branch")
            .and_then(Value::as_str)
            .unwrap_or("main")
            .to_string();

        let token = self
            .source
            .call("mint_run_token", json!({"service": service, "permissions": ["contents:read"], "ttl_secs": self.cfg.token_ttl_secs}), self.cfg.call_timeout)
            .await?;
        let token = token
            .get("token")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::BadOutput("mint_run_token without token".into()))?
            .to_string();
        let url = self
            .source
            .call(
                "repo_url",
                json!({"service": service, "token": token}),
                self.cfg.call_timeout,
            )
            .await?;
        let url = url
            .get("url")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::BadOutput("repo_url without url".into()))?
            .to_string();

        let prepared = self
            .ws
            .prepare_readonly(service, &url, &base, service_id)
            .await?;
        let out = self
            .engine
            .call(
                "run",
                json!({
                    "role": "ask",
                    "ticket": {"service": service, "ref": "", "title": "", "body": "", "comments": []},
                    "question": question,
                    "profile": crate::memory::with_notes(&self.pool, &self.cfg.source_name, service, &profile).await.unwrap_or(profile.clone()),
                    "workspace": prepared.worktree,
                    "budget": {"max_secs": self.cfg.call_timeout.as_secs(), "max_tokens": 100000},
                    "read_only": true
                }),
                self.cfg.call_timeout,
            )
            .await;
        self.ws.remove_worktree(&prepared).await;
        let out = out?;
        let answer = out
            .get("answer")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::BadOutput("ask without answer".into()))?
            .to_string();
        Ok(answer)
    }
}
