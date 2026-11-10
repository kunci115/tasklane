//! A read-only AI review of a pull request somebody opened. No ticket, no state change: the reviewer
//! sees the diff, and its verdict is posted on the pull request as a comment.

use serde_json::{json, Value};

use crate::{Error, Orchestrator, MARKER};

/// What the reviewer said.
pub struct PullReview {
    pub verdict: String,
    pub notes: String,
    /// The commit that was reviewed; a merge from the console needs this exact one.
    pub head_sha: String,
    /// The reviewer's Big-O analysis of the changed code (empty when it gave none).
    pub complexity: String,
}

/// A target branch name that is safe to hand to git.
fn usable_branch(base: &str) -> Result<String, Error> {
    if base.is_empty()
        || base.starts_with('-')
        || !base
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'/' | b'-'))
    {
        return Err(Error::Fatal(
            "the pull request has no usable target branch".into(),
        ));
    }
    Ok(base.to_string())
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
        let Some((_facts, profile, mode)) = row else {
            return Err(Error::Fatal(format!("unknown repository {service}")));
        };
        if mode == "watch" {
            return Err(Error::Fatal(format!(
                "{service} is in watch mode: set it to triage or full first"
            )));
        }
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
        // The pull request is compared with the branch it goes into, which is often not the
        // default branch (here `dev`); otherwise everything dev has and main lacks counts as its change.
        let info = self
            .source
            .call("get_pull", json!({"service": service, "pr": pr}), call)
            .await?;
        if say(&info, "state") != "open" {
            return Err(Error::Fatal("the pull request is not open".into()));
        }
        let base = usable_branch(&say(&info, "base"))?;
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
            head_sha: say(&info, "head_sha"),
            complexity: say(&out, "complexity"),
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
        let complexity = if review.complexity.is_empty() {
            String::new()
        } else {
            format!("\n\n**Complexity (Big-O)**\n\n{}", review.complexity)
        };
        let body = format!(
            "{MARKER}\n**{headline}**\n\n{}{complexity}\n\n_Read-only AI review requested by {by} in tasklane. It does not approve or merge anything._",
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
            json!({"service": service, "pr": pr, "verdict": review.verdict, "notes": review.notes, "head_sha": review.head_sha, "complexity": review.complexity}),
        )
        .execute(&self.pool)
        .await?;
        Ok(review)
    }

    /// Merge a pull request a person chose to merge from the console. Allowed only when the
    /// repository is in full mode with console merging switched on, the target is not the default
    /// branch (production is promoted by a person on the source), and the AI approved this exact
    /// commit. The source merges only if the head is still that commit.
    pub async fn merge_pull(&self, service: &str, pr: &str, by: &str) -> Result<(), Error> {
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
        if mode != "full" || profile.get("console_merge").and_then(Value::as_bool) != Some(true) {
            return Err(Error::Fatal(
                "merging from the console is off for this repository (it needs full mode and the switch on the Pull requests page)".into(),
            ));
        }
        let call = self.cfg.call_timeout;
        let info = self
            .source
            .call("get_pull", json!({"service": service, "pr": pr}), call)
            .await?;
        if say(&info, "state") != "open" {
            return Err(Error::Fatal("the pull request is not open".into()));
        }
        let base = usable_branch(&say(&info, "base"))?;
        let default = facts
            .get("default_branch")
            .and_then(Value::as_str)
            .unwrap_or("main");
        if base == default {
            return Err(Error::Fatal(format!(
                "it goes into {default}, the default branch: a person merges that one on the source"
            )));
        }
        let head = say(&info, "head_sha");
        let reviewed: Option<Value> = sqlx::query_scalar(
            "SELECT detail FROM audit_events WHERE action = 'pull.reviewed' AND detail->>'service' = $1 AND detail->>'pr' = $2 ORDER BY id DESC LIMIT 1",
        )
        .bind(service)
        .bind(pr)
        .fetch_optional(&self.pool)
        .await?;
        let approved = reviewed.as_ref().is_some_and(|d| {
            say(d, "verdict") == "approve" && !head.is_empty() && say(d, "head_sha") == head
        });
        if !approved {
            return Err(Error::Fatal(
                "no approving AI review of this exact commit: review it again".into(),
            ));
        }
        let token = self
            .source
            .call(
                "mint_run_token",
                json!({"service": service, "permissions": ["contents:write", "pull_requests:write"], "ttl_secs": self.cfg.token_ttl_secs}),
                call,
            )
            .await?;
        let token = token
            .get("token")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::BadOutput("mint_run_token without token".into()))?
            .to_string();
        let r = self
            .source
            .call(
                "merge_pr",
                json!({"service": service, "pr": pr, "expected_head_sha": head, "token": token}),
                call,
            )
            .await?;
        let merged = r.get("merged").and_then(Value::as_bool).unwrap_or(false);
        let reason = say(&r, "reason");
        sqlx::query("INSERT INTO audit_events (actor, action, detail) VALUES ($1, $2, $3)")
            .bind(by)
            .bind(if merged {
                "pull.merged"
            } else {
                "pull.merge_refused"
            })
            .bind(json!({"service": service, "pr": pr, "head_sha": head, "reason": reason}))
            .execute(&self.pool)
            .await?;
        if merged {
            Ok(())
        } else {
            Err(Error::Fatal(format!(
                "the source refused the merge: {}",
                if reason.is_empty() {
                    "unknown"
                } else {
                    &reason
                }
            )))
        }
    }
}
