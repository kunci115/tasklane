//! Which tasklane install may act on a repository.
//!
//! Two installs on one repository would both triage every issue, comment twice and open duplicate pull
//! requests. Each install that wants to act on a repository claims it on the ticket source (for GitHub: a
//! ref created atomically; the oldest claim wins and every install computes the same winner), and an
//! install that does not own a repository leaves it alone. The claim is made when the first issue arrives
//! and looked at again every few minutes, so a repository nobody writes to costs nothing.

use std::time::Duration;

use serde_json::{json, Value};
use tasklane_plugin::ErrorKind;

use crate::host::CallError;
use crate::{Error, Orchestrator};

/// How long a result is trusted before the source is asked again.
const RECHECK_MINE_SECS: i64 = 600;
const RECHECK_FOREIGN_SECS: i64 = 300;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Instance {
    pub id: String,
    pub name: String,
}

impl Instance {
    /// The name this install uses on the ticket source: letters, digits, dot, dash, underscore.
    pub fn tag(&self) -> String {
        format!("{}_{}", self.name, self.id)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Owner {
    Mine,
    /// Another install owns it; its name.
    Foreign(String),
}

/// A short, safe name for this machine: lowercase letters and digits.
fn clean_name(raw: &str) -> String {
    let s: String = raw
        .trim()
        .to_ascii_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(20)
        .collect();
    if s.len() >= 2 {
        s
    } else {
        "tasklane".to_string()
    }
}

fn host_name() -> String {
    std::env::var("TASKLANE_INSTANCE_NAME")
        .ok()
        .or_else(|| std::fs::read_to_string("/etc/hostname").ok())
        .map(|s| clean_name(&s))
        .unwrap_or_else(|| "tasklane".to_string())
}

fn random_id() -> String {
    let mut b = [0u8; 4];
    let _ = getrandom::fill(&mut b);
    b.iter().map(|x| format!("{x:02x}")).collect()
}

impl Orchestrator {
    /// This install's identity, made on first use and kept in the settings table.
    pub async fn instance(&self) -> Result<Instance, Error> {
        if let Some(i) = self.instance.get() {
            return Ok(i.clone());
        }
        let stored: Option<Value> =
            sqlx::query_scalar("SELECT value FROM settings WHERE key = 'instance'")
                .fetch_optional(&self.pool)
                .await?;
        let found = stored.and_then(|v| {
            Some(Instance {
                id: v.get("id")?.as_str()?.to_string(),
                name: v.get("name")?.as_str()?.to_string(),
            })
        });
        let inst = match found {
            Some(i) => i,
            None => {
                let fresh = Instance {
                    id: random_id(),
                    name: host_name(),
                };
                // two workers starting at once: the first writer's identity stands
                sqlx::query("INSERT INTO settings (key, value) VALUES ('instance', $1) ON CONFLICT (key) DO NOTHING")
                    .bind(json!({"id": fresh.id, "name": fresh.name}))
                    .execute(&self.pool)
                    .await?;
                let v: Value =
                    sqlx::query_scalar("SELECT value FROM settings WHERE key = 'instance'")
                        .fetch_one(&self.pool)
                        .await?;
                Instance {
                    id: v["id"].as_str().unwrap_or(&fresh.id).to_string(),
                    name: v["name"].as_str().unwrap_or(&fresh.name).to_string(),
                }
            }
        };
        let _ = self.instance.set(inst.clone());
        Ok(inst)
    }

    /// Whether this install may act on the repository. Cheap when the answer is recent.
    pub async fn ensure_owner(&self, service_id: i64, external_id: &str) -> Result<Owner, Error> {
        let row: Option<(String, Option<String>, Option<i64>)> = sqlx::query_as(
            "SELECT owner_state, owner_name, extract(epoch FROM now() - owner_checked_at)::bigint
             FROM services WHERE id = $1",
        )
        .bind(service_id)
        .fetch_optional(&self.pool)
        .await?;
        if let Some((state, name, age)) = &row {
            let fresh = |limit: i64| age.is_some_and(|a| a < limit);
            if state == "mine" && fresh(RECHECK_MINE_SECS) {
                return Ok(Owner::Mine);
            }
            if state == "foreign" && fresh(RECHECK_FOREIGN_SECS) {
                return Ok(Owner::Foreign(name.clone().unwrap_or_default()));
            }
        }
        self.claim(service_id, external_id, "claim_repo").await
    }

    /// A person decided this install takes the repository over from the other one.
    pub async fn take_over(&self, service_id: i64) -> Result<Owner, Error> {
        let ext = self.external_id(service_id).await?;
        let owner = self.claim(service_id, &ext, "takeover_repo").await?;
        let _ = sqlx::query("INSERT INTO audit_events (actor, action, detail) VALUES ('console:admin', 'service.takeover', $1)")
            .bind(json!({"service_id": service_id, "owner_now": match &owner { Owner::Mine => "this install".to_string(), Owner::Foreign(n) => n.clone() }}))
            .execute(&self.pool)
            .await;
        Ok(owner)
    }

    /// Give the claim back (the repository went to `watch`). Best effort: a failure is only logged.
    pub async fn release_owner(&self, service_id: i64) {
        let Ok(ext) = self.external_id(service_id).await else {
            return;
        };
        if let Ok(inst) = self.instance().await {
            let r = self
                .source
                .call(
                    "release_repo",
                    json!({"service": ext, "instance": inst.tag()}),
                    self.cfg.call_timeout,
                )
                .await;
            if let Err(e) = r {
                if !unsupported(&e) {
                    eprintln!("ownership: release {ext}: {e}");
                }
            }
        }
        let _ = sqlx::query("UPDATE services SET owner_state = 'unknown', owner_name = NULL, owner_checked_at = NULL WHERE id = $1")
            .bind(service_id)
            .execute(&self.pool)
            .await;
    }

    pub(crate) async fn external_id(&self, service_id: i64) -> Result<String, Error> {
        Ok(
            sqlx::query_scalar("SELECT external_id FROM services WHERE id = $1")
                .bind(service_id)
                .fetch_one(&self.pool)
                .await?,
        )
    }

    async fn claim(&self, service_id: i64, external_id: &str, op: &str) -> Result<Owner, Error> {
        let inst = self.instance().await?;
        let answer = self
            .source
            .call(
                op,
                json!({"service": external_id, "instance": inst.tag()}),
                Duration::from_secs(60),
            )
            .await;
        let owner = match answer {
            Ok(v) => {
                if v.get("mine").and_then(Value::as_bool).unwrap_or(true) {
                    Owner::Mine
                } else {
                    Owner::Foreign(
                        v.get("owner")
                            .and_then(Value::as_str)
                            .unwrap_or("another install")
                            .to_string(),
                    )
                }
            }
            // a source without this guard (or a plug-in that is not updated yet) is not blocked
            Err(e) if unsupported(&e) => Owner::Mine,
            Err(e) => return Err(e.into()),
        };
        let (state, name) = match &owner {
            Owner::Mine => ("mine", None),
            Owner::Foreign(n) => ("foreign", Some(n.as_str())),
        };
        sqlx::query("UPDATE services SET owner_state = $2, owner_name = $3, owner_checked_at = now() WHERE id = $1")
            .bind(service_id)
            .bind(state)
            .bind(name)
            .execute(&self.pool)
            .await?;
        if let Owner::Foreign(n) = &owner {
            let _ = sqlx::query("INSERT INTO audit_events (actor, action, detail) SELECT 'sync', 'service.owned_elsewhere', $2 WHERE NOT EXISTS (SELECT 1 FROM audit_events WHERE action = 'service.owned_elsewhere' AND detail->>'service_id' = $1::text AND at > now() - interval '1 day')")
                .bind(service_id.to_string())
                .bind(json!({"service_id": service_id, "owner": n}))
                .execute(&self.pool)
                .await;
        }
        Ok(owner)
    }
}

fn unsupported(e: &CallError) -> bool {
    matches!(e, CallError::Plugin(p) if p.kind == ErrorKind::Permanent && p.message.starts_with("unknown op"))
}
