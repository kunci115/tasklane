//! A read-only AI review of a pull request somebody opened. No ticket, no state change: the reviewer
//! sees the diff, and its verdict is posted on the pull request as a comment.

use serde_json::{json, Value};

use crate::{Error, Orchestrator, MARKER};

/// What the reviewer said.
pub struct PullReview {
    pub verdict: String,
    pub notes: String,
}

fn say(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

impl Orchestrator {
    /// Review pull request `pr` of `service` (a registry `external_id` of a repository that is not
    /// in watch mode), post the result on the pull request and record it in the audit log.
    pub async fn review_pull(
        &self,
        service: &str,
        pr: &str,
        by: &str,
    ) -> Result<PullReview, Error> {
        let row: Option<(Value, Value, String)> = sqlx::query_as(
            "SELECT facts, agent_profile, mode FROM services WHERE source = $1 AND external_id = $2 AND NOT archived",
        )
        .bind(&self.cfg.source_name)
        .bind(service)
        .fetch_optional(&self.pool)
        .await?;
        let Some((facts, profile, mode)) = row else {
            return Err(Error::Fatal(format!("unknown repository {service}")));
        };
        if mode == "watch" {
            return Err(Error::Fatal(format!(
                "{service} is in watch mode: set it to triage or full first"
            )));
        }
        let base = facts
            .get("default_branch")
            .and_then(Value::as_str)
            .unwrap_or("main")
            .to_string();
        let call = self.cfg.call_timeout;
        let token = self
            .source
            .call(
                "mint_run_token",
                json!({"service": service, "permissions": ["contents:read"], "ttl_secs": self.cfg.token_ttl_secs}),
                call,
            )
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
                call,
            )
            .await?;
        let url = url
            .get("url")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::BadOutput("repo_url without url".into()))?
            .to_string();
        let (diff, _files) = self.ws.pull_diff(service, &url, &base, pr).await?;
        if diff.trim().is_empty() {
            return Err(Error::Fatal(
                "the pull request has no changes to review".into(),
            ));
        }
        let item = self
            .source
            .call("get_item", json!({"service": service, "ref": pr}), call)
            .await?;
        let out = self
            .engine
            .call(
                "run",
                json!({
                    "role": "reviewer",
                    "ticket": {"service": service, "ref": pr, "title": say(&item, "title"), "body": say(&item, "body"), "comments": []},
                    "profile": crate::memory::with_notes(&self.pool, &self.cfg.source_name, service, &profile).await.unwrap_or(profile.clone()),
                    "diff": crate::steps::develop::cap(&diff),
                    "budget": {"max_secs": call.as_secs(), "max_tokens": 100000},
                    "read_only": true
                }),
                call,
            )
            .await?;
        let review = PullReview {
            verdict: say(&out, "verdict"),
            notes: say(&out, "notes"),
        };
        if !matches!(review.verdict.as_str(), "approve" | "changes" | "escalate") {
            return Err(Error::BadOutput(format!(
                "unknown reviewer verdict {:?}",
                review.verdict
            )));
        }
        let headline = match review.verdict.as_str() {
            "approve" => "The AI reviewer found nothing to change.",
            "changes" => "The AI reviewer asks for changes.",
            _ => "The AI reviewer wants a person to look at this.",
        };
        let body = format!(
            "{MARKER}\n**{headline}**\n\n{}\n\n_Read-only AI review requested by {by} in tasklane. It does not approve or merge anything._",
            review.notes
        );
        if let Err(e) = self
            .source
            .call(
                "comment",
                json!({"service": service, "ref": pr, "body": body}),
                call,
            )
            .await
        {
            eprintln!("pull review {service}#{pr}: could not post the comment: {e}");
        }
        sqlx::query(
            "INSERT INTO audit_events (actor, action, detail) VALUES ($1, 'pull.reviewed', $2)",
        )
        .bind(by)
        .bind(
            json!({"service": service, "pr": pr, "verdict": review.verdict, "notes": review.notes}),
        )
        .execute(&self.pool)
        .await?;
        Ok(review)
    }
}
