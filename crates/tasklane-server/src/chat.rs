//! The chat bridge: people in registered groups can ask questions and start
//! tasks; people with an approver role can approve or reject with buttons.
//!
//! * Being in a group is enough to ask a question or start a task (the ticket
//!   still goes through triage and approval like any other).
//! * Approving needs a registered principal; typed text never approves.
//! * Unregistered groups get one hint with their id and nothing else.
//! * Each group has a daily cap on questions.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use sqlx::Row;
use tasklane_orchestrator::approval::{decide, Decision, Principal};
use tasklane_orchestrator::host::PluginClient;
use tasklane_orchestrator::Orchestrator;
use tokio::sync::watch;

use crate::tickets::{create_ticket, CreateError, Created};

pub struct ChatBridge {
    pub orch: Arc<Orchestrator>,
    pub chat: PluginClient,
    pub channel: String,
    pub user_daily_limit: i64,
}

const SHORT: Duration = Duration::from_secs(30);
const MAX_ANSWER_CHARS: usize = 3500;

fn clip(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        format!("{}...", s.chars().take(n).collect::<String>())
    }
}

fn field<'a>(ev: &'a Value, k: &str) -> &'a str {
    ev.get(k).and_then(Value::as_str).unwrap_or_default()
}

impl ChatBridge {
    async fn say(&self, group: &str, text: &str, buttons: Option<Value>) -> bool {
        let mut p = json!({"group": group, "text": text});
        if let Some(b) = buttons {
            p["buttons"] = b;
        }
        match self.chat.call("send", p, SHORT).await {
            Ok(_) => true,
            Err(e) => {
                eprintln!("chat: send to {group}: {e}");
                false
            }
        }
    }

    async fn group(&self, group: &str) -> Result<Option<(i64, String, i32)>, sqlx::Error> {
        let r = sqlx::query("SELECT g.id, s.external_id, g.daily_cap FROM chat_groups g JOIN services s ON s.id = g.service_id WHERE g.channel = $1 AND g.group_ref = $2 AND NOT s.archived")
            .bind(&self.channel)
            .bind(group)
            .fetch_optional(self.orch.pool())
            .await?;
        Ok(r.map(|r| (r.get(0), r.get(1), r.get(2))))
    }

    pub async fn handle_event(&self, ev: &Value) {
        let kind = field(ev, "kind");
        let group = field(ev, "group");
        if group.is_empty() || !matches!(kind, "command" | "button" | "joined") {
            return; // plain messages are not for us
        }
        let known = match self.group(group).await {
            Ok(g) => g,
            Err(e) => return eprintln!("chat: {e}"),
        };
        let Some((_, service, cap)) = known else {
            return self.hint_unregistered(group).await;
        };
        match kind {
            "command" => {
                let cmd = field(ev, "command").trim_start_matches('/');
                let args = field(ev, "args").trim();
                match cmd {
                    "ask" => self.ask(group, &service, cap, ev, args).await,
                    "task" => self.task(group, &service, ev, args).await,
                    "help" | "start" => {
                        self.say(group, &format!("I work on {service}. /ask <question> answers from the code. /task <title> opens a ticket. Approvals come as buttons."), None).await;
                    }
                    _ => {}
                }
            }
            "button" => self.button(group, ev).await,
            _ => {}
        }
    }

    /// An unregistered group gets its id once an hour, so an admin can register it. Nothing else.
    async fn hint_unregistered(&self, group: &str) {
        let first: Option<String> = sqlx::query_scalar(
            "INSERT INTO sync_state (key, value) VALUES ($1, 'x')
             ON CONFLICT (key) DO UPDATE SET updated_at = now() WHERE sync_state.updated_at < now() - interval '1 hour'
             RETURNING key",
        )
        .bind(format!("chat_hint:{}:{group}", self.channel))
        .fetch_optional(self.orch.pool())
        .await
        .unwrap_or(None);
        if first.is_some() {
            self.say(group, &format!("This group is not registered with tasklane. Group id: {group}. An administrator can register it in the console under Chat groups, then add me again. I am leaving now."), None).await;
            if let Err(e) = self
                .chat
                .call("leave", json!({"group": group}), SHORT)
                .await
            {
                eprintln!("chat: leave {group}: {e}");
            }
        }
    }

    async fn ask(&self, group: &str, service: &str, cap: i32, ev: &Value, question: &str) {
        if question.is_empty() {
            self.say(group, "Usage: /ask <your question>", None).await;
            return;
        }
        let actor = format!("{}:{}:{}", self.channel, group, field(ev, "user"));
        let used: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_events WHERE action = 'chat.ask' AND actor LIKE $1 AND at > date_trunc('day', now())")
            .bind(format!("{}:{}:%", self.channel, group))
            .fetch_one(self.orch.pool())
            .await
            .unwrap_or(i64::MAX);
        if used >= i64::from(cap) {
            self.say(group, "This group has used its questions for today.", None)
                .await;
            return;
        }
        let _ = sqlx::query(
            "INSERT INTO audit_events (actor, action, detail) VALUES ($1, 'chat.ask', $2)",
        )
        .bind(&actor)
        .bind(json!({"service": service, "question_chars": question.chars().count()}))
        .execute(self.orch.pool())
        .await;
        match self.orch.ask(service, question, &actor).await {
            Ok(answer) => {
                self.say(group, &clip(&answer, MAX_ANSWER_CHARS), None)
                    .await;
            }
            Err(e) => {
                eprintln!("chat: ask: {e}");
                self.say(group, "I could not answer that right now.", None)
                    .await;
            }
        }
    }

    async fn task(&self, group: &str, service: &str, ev: &Value, args: &str) {
        let (title, rest) = args.split_once('\n').unwrap_or((args, ""));
        let who = field(ev, "user_name");
        let reporter = format!("{}:{}", self.channel, field(ev, "user"));
        let via = format!(
            "{} chat by {}",
            self.channel,
            if who.is_empty() {
                field(ev, "user")
            } else {
                who
            }
        );
        let msg = match create_ticket(
            &self.orch,
            service,
            &reporter,
            title,
            rest,
            &via,
            self.user_daily_limit,
        )
        .await
        {
            Ok(Created::New(r)) => {
                let _ = sqlx::query(
                    "UPDATE tickets SET notify_ref = $3 WHERE external_ref = $2
                       AND service_id = (SELECT id FROM services WHERE source = $4 AND external_id = $1)",
                )
                .bind(service)
                .bind(&r)
                .bind(format!("{}:{group}", self.channel))
                .bind(&self.orch.config().source_name)
                .execute(self.orch.pool())
                .await;
                format!("Created ticket #{r} in {service}. It will be triaged now.")
            }
            Ok(Created::Duplicate(r)) => format!("You already reported this: #{r}."),
            Err(CreateError::BadTitle) => {
                "Usage: /task <title> (up to 200 characters, details on the following lines)"
                    .to_string()
            }
            Err(CreateError::LimitReached) => {
                "You have reached your daily limit for new tickets.".to_string()
            }
            Err(CreateError::ReadOnlyService) => {
                "This repository is not accepting tickets.".to_string()
            }
            Err(_) => "I could not create the ticket right now.".to_string(),
        };
        self.say(group, &msg, None).await;
    }

    /// `a:<ticket>:<first 16 of the plan hash>` approves, `r:...` rejects.
    async fn button(&self, group: &str, ev: &Value) {
        let data = field(ev, "data");
        let parts: Vec<&str> = data.split(':').collect();
        let [verb @ ("a" | "r"), id, prefix] = parts[..] else {
            return;
        };
        let Ok(ticket_id) = id.parse::<i64>() else {
            return;
        };
        // The button must belong to a ticket of this group's service.
        let hash: Option<String> = sqlx::query_scalar(
            "SELECT t.plan_hash FROM tickets t JOIN chat_groups g ON g.service_id = t.service_id
             WHERE t.id = $1 AND g.channel = $2 AND g.group_ref = $3",
        )
        .bind(ticket_id)
        .bind(&self.channel)
        .bind(group)
        .fetch_optional(self.orch.pool())
        .await
        .unwrap_or(None);
        let Some(hash) = hash.filter(|h| h.starts_with(prefix) && prefix.len() >= 8) else {
            self.say(
                group,
                &format!("That button is out of date for ticket {ticket_id}."),
                None,
            )
            .await;
            return;
        };
        let who = Principal {
            channel: self.channel.clone(),
            reference: field(ev, "user").to_string(),
        };
        let decision = if verb == "a" {
            Decision::Approve
        } else {
            Decision::Reject
        };
        let name = field(ev, "user_name");
        let msg = match decide(self.orch.pool(), ticket_id, &who, "chat", decision, &hash).await {
            Ok(_) => format!(
                "Ticket {ticket_id} {} by {}.",
                if verb == "a" { "approved" } else { "rejected" },
                if name.is_empty() {
                    &who.reference
                } else {
                    name
                }
            ),
            Err(e) => format!("Not done: {e}"),
        };
        self.say(group, &msg, None).await;
    }

    /// Tell every group of a service when one of its tickets needs approval (once per plan).
    pub async fn announce_approvals(&self) {
        let rows = sqlx::query(
            "SELECT t.id, t.external_ref, t.title, t.level, t.plan_hash, s.external_id, g.group_ref
             FROM tickets t JOIN services s ON s.id = t.service_id JOIN chat_groups g ON g.service_id = s.id AND g.channel = $1
             WHERE t.state = 'awaiting_approval' AND t.plan_hash IS NOT NULL AND t.approval_notified_hash IS DISTINCT FROM t.plan_hash
             ORDER BY t.id LIMIT 20",
        )
        .bind(&self.channel)
        .fetch_all(self.orch.pool())
        .await
        .unwrap_or_default();
        let mut done: Vec<(i64, String)> = vec![];
        for r in rows {
            let (id, ext, level): (i64, String, Option<i16>) =
                (r.get("id"), r.get("external_ref"), r.get("level"));
            let (title, hash, service, group): (Option<String>, String, String, String) = (
                r.get("title"),
                r.get("plan_hash"),
                r.get("external_id"),
                r.get("group_ref"),
            );
            let p16 = &hash[..16.min(hash.len())];
            let text = format!(
                "{service} #{ext}{} needs approval (level {}). Plan {}. Only registered approvers can press the buttons.",
                title.map(|t| format!(": {}", clip(&t, 100))).unwrap_or_default(),
                level.unwrap_or(0),
                &hash[..8.min(hash.len())]
            );
            let buttons = json!([{"label": "Approve", "data": format!("a:{id}:{p16}")}, {"label": "Reject", "data": format!("r:{id}:{p16}")}]);
            if self.say(&group, &text, Some(buttons)).await {
                done.push((id, hash));
            }
        }
        for (id, hash) in done {
            let _ = sqlx::query("UPDATE tickets SET approval_notified_hash = $2 WHERE id = $1")
                .bind(id)
                .bind(hash)
                .execute(self.orch.pool())
                .await;
        }
    }

    /// Tell the chat group a ticket was reported from how it ended or what it needs, once per state.
    pub async fn announce_outcomes(&self) {
        let rows = sqlx::query(
            "SELECT t.id, t.external_ref, t.title, t.state, t.last_error, s.external_id, t.notify_ref
             FROM tickets t JOIN services s ON s.id = t.service_id
             WHERE t.notify_ref LIKE $1 AND t.notified_state IS DISTINCT FROM t.state
               AND t.state IN ('needs_info', 'duplicate_proposed', 'merged', 'failed', 'closed_duplicate', 'closed_rejected', 'closed_stale', 'closed_done')
             ORDER BY t.id LIMIT 20",
        )
        .bind(format!("{}:%", self.channel))
        .fetch_all(self.orch.pool())
        .await
        .unwrap_or_default();
        for r in rows {
            let (id, ext, state, service): (i64, String, String, String) = (
                r.get("id"),
                r.get("external_ref"),
                r.get("state"),
                r.get("external_id"),
            );
            let title: Option<String> = r.get("title");
            let notify: String = r.get("notify_ref");
            let group = notify
                .split_once(':')
                .map(|x| x.1)
                .unwrap_or("")
                .to_string();
            let what = match state.as_str() {
                "needs_info" => {
                    "needs more information from you; the question is on the issue".to_string()
                }
                "duplicate_proposed" => "looks like a duplicate; a person will decide".to_string(),
                "merged" => "was merged into the development branch".to_string(),
                "failed" => "stopped and needs a person; the reason is in the console".to_string(),
                "closed_duplicate" => "was closed as a duplicate".to_string(),
                "closed_rejected" => "was rejected".to_string(),
                "closed_stale" => "was closed because nobody answered in time".to_string(),
                _ => "is done".to_string(),
            };
            let text = format!(
                "{service} #{ext}{} {what}.",
                title
                    .map(|t| format!(" ({})", clip(&t, 80)))
                    .unwrap_or_default()
            );
            if self.say(&group, &text, None).await {
                let _ = sqlx::query("UPDATE tickets SET notified_state = $2 WHERE id = $1")
                    .bind(id)
                    .bind(&state)
                    .execute(self.orch.pool())
                    .await;
            }
        }
    }

    pub async fn run(self: Arc<Self>, mut stop: watch::Receiver<bool>) {
        let asks = Arc::new(tokio::sync::Semaphore::new(2));
        let quick = Arc::new(tokio::sync::Semaphore::new(8));
        loop {
            if *stop.borrow() {
                return;
            }
            self.announce_approvals().await;
            self.announce_outcomes().await;
            let got = tokio::select! {
                r = self.chat.call("receive", json!({"timeout_secs": 20}), Duration::from_secs(40)) => r,
                _ = stop.changed() => return,
            };
            match got {
                Ok(v) => {
                    for ev in v
                        .get("events")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default()
                    {
                        // Questions are slow; they run on their own small pool so that
                        // approval buttons and the like are never stuck behind them.
                        let slow = field(&ev, "kind") == "command"
                            && field(&ev, "command").trim_start_matches('/') == "ask";
                        let sem = if slow { asks.clone() } else { quick.clone() };
                        let Ok(permit) = sem.acquire_owned().await else {
                            continue;
                        };
                        let me = self.clone();
                        tokio::spawn(async move {
                            me.handle_event(&ev).await;
                            drop(permit);
                        });
                    }
                }
                Err(e) => {
                    eprintln!("chat: receive: {e}");
                    tokio::select! {
                        _ = tokio::time::sleep(Duration::from_secs(5)) => {}
                        _ = stop.changed() => return,
                    }
                }
            }
        }
    }
}
