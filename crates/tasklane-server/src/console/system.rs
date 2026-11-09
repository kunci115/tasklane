//! Health, setup checklist and the activity feed.

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::Value;
use sqlx::Row;

use super::ui::{ago, empty, head, icon, render};
use super::{guard, AppState};
use crate::esc;

pub struct Step {
    pub ok: bool,
    pub optional: bool,
    pub title: &'static str,
    pub detail: String,
    pub link: Option<(&'static str, &'static str)>,
}

pub struct Setup {
    pub steps: Vec<Step>,
    pub src: Option<Value>,
    pub eng: Option<Value>,
}

impl Setup {
    pub fn done(&self) -> usize {
        self.steps.iter().filter(|s| s.ok).count()
    }
    pub fn complete(&self) -> bool {
        self.steps.iter().all(|s| s.ok || s.optional)
    }
}

pub async fn setup_status(st: &AppState) -> Setup {
    let pool = st.orch.pool();
    let (src, eng) = st.orch.describe_plugins().await;
    let repos: i64 = sqlx::query_scalar("SELECT count(*) FROM services WHERE NOT archived")
        .fetch_one(pool)
        .await
        .unwrap_or(0);
    let active: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM services WHERE mode IN ('triage','full') AND NOT archived",
    )
    .fetch_one(pool)
    .await
    .unwrap_or(0);
    let tickets: i64 = sqlx::query_scalar("SELECT count(*) FROM tickets")
        .fetch_one(pool)
        .await
        .unwrap_or(0);
    let people: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM principals WHERE NOT (channel = 'console' AND ref = 'admin')",
    )
    .fetch_one(pool)
    .await
    .unwrap_or(0);
    let sandbox = eng
        .as_ref()
        .and_then(|e| e.get("sandbox"))
        .and_then(Value::as_str)
        .unwrap_or("none")
        .to_string();
    let allowed = eng
        .as_ref()
        .and_then(|e| e.get("unsandboxed_allowed"))
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let mut steps = vec![
        Step {
            ok: src.is_some() && repos > 0 && src.as_ref().and_then(|s| s.get("configured")).and_then(Value::as_bool).unwrap_or(true),
            optional: false,
            title: "Connect your ticket source",
            detail: if src.is_none() {
                "The ticket-source plug-in is not answering. Check the daemon log.".into()
            } else if src.as_ref().and_then(|s| s.get("configured")).and_then(Value::as_bool) == Some(false) {
                "GitHub is not connected yet. It takes one click: tasklane creates the App for you.".into()
            } else if repos == 0 {
                "The plug-in answers but no repository was found yet. Install the App on a repository; the list updates within a minute.".into()
            } else {
                format!("{repos} repositor{} found.", if repos == 1 { "y" } else { "ies" })
            },
            link: Some(("/connections", "Connect GitHub")),
        },
        Step {
            ok: eng.is_some() && (sandbox == "docker" || allowed) && eng.as_ref().and_then(|e| e.get("credential")).and_then(Value::as_bool).unwrap_or(true),
            optional: false,
            title: "Connect the agent and make it safe",
            detail: match (eng.is_some(), sandbox.as_str(), allowed) {
                (false, _, _) => "The agent engine is not answering. Check its settings and the daemon log.".into(),
                (true, "docker", _) => "The agent runs inside a Docker sandbox.".into(),
                (true, _, true) => "The agent runs on this host without a sandbox because you allowed it. Only do that on a host that holds nothing else.".into(),
                (true, _, false) => "The agent will not run until you choose how it is isolated, and give it a key.".into(),
            },
            link: Some(("/connect/agent", "Set up the agent")),
        },
        Step {
            ok: active > 0,
            optional: false,
            title: "Choose where tasklane may work",
            detail: if active > 0 { format!("{active} repositor{} in triage or full mode.", if active == 1 { "y is" } else { "ies are" }) } else { "Every repository is still in watch mode (nothing happens). Set one to triage.".into() },
            link: Some(("/services", "Choose repositories")),
        },
        Step {
            ok: tickets > 0,
            optional: false,
            title: "Get a first ticket",
            detail: if tickets > 0 { format!("{tickets} ticket{} so far.", if tickets == 1 { "" } else { "s" }) } else { "Open an issue on a repository that is not in watch mode. It appears here within a minute.".into() },
            link: None,
        },
        Step {
            ok: people > 0,
            optional: true,
            title: "Add teammates who can approve (optional)",
            detail: if people > 0 { format!("{people} approver{} besides you.", if people == 1 { "" } else { "s" }) } else { "Only you can approve right now. Add people and choose what they may approve.".into() },
            link: Some(("/people", "People")),
        },
    ];
    steps.retain(|s| !s.title.is_empty());
    Setup { steps, src, eng }
}

pub fn checklist(s: &Setup) -> String {
    let total = s.steps.len();
    let pct = s.done() * 100 / total.max(1);
    let mut h = format!("<div class=\"progress\"><i style=\"width:{pct}%\"></i></div>");
    for (n, step) in s.steps.iter().enumerate() {
        let class = if step.ok {
            "ok"
        } else if step.optional {
            "todo"
        } else {
            "bad"
        };
        h.push_str(&format!(
            "<div class=\"check {class}\"><div class=\"ck\">{}</div><div style=\"flex:1\"><div style=\"font-weight:600\">{}</div><div class=\"muted small\">{}</div></div>{}</div>",
            if step.ok { icon("check") } else { (n + 1).to_string() },
            esc(step.title),
            esc(&step.detail),
            step.link.map(|(href, label)| format!("<a class=\"btn small\" href=\"{href}\">{}</a>", esc(label))).unwrap_or_default()
        ));
    }
    h
}

pub async fn health(State(st): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let sess = match guard(&st, &headers).await {
        Ok(s) => s,
        Err(r) => return r,
    };
    let pool = st.orch.pool();
    let setup = setup_status(&st).await;
    let db_ok = sqlx::query_scalar::<_, i32>("SELECT 1")
        .fetch_one(pool)
        .await
        .is_ok();
    let q = sqlx::query(
        "SELECT count(*) FILTER (WHERE status='queued') AS queued, count(*) FILTER (WHERE status='running') AS running,
                count(*) FILTER (WHERE status='failed') AS failed,
                coalesce(max(extract(epoch FROM now()-run_after)) FILTER (WHERE status='queued' AND run_after <= now()),0)::bigint AS oldest
         FROM jobs",
    )
    .fetch_one(pool)
    .await;
    let (queued, running, failed, oldest): (i64, i64, i64, i64) = q
        .map(|r| {
            (
                r.get("queued"),
                r.get("running"),
                r.get("failed"),
                r.get("oldest"),
            )
        })
        .unwrap_or((0, 0, 0, 0));
    let paused = st.orch.is_paused().await.unwrap_or(false);

    let plug = |label: &str, v: &Option<Value>| -> String {
        match v {
            None => format!("<tr><td>{label}</td><td><span class=\"pill bad\">not answering</span></td><td class=\"muted\">-</td></tr>"),
            Some(d) => format!(
                "<tr><td>{label}</td><td><span class=\"pill ok\">running</span></td><td>{} <span class=\"muted\">v{}</span></td></tr>",
                esc(d.get("name").and_then(Value::as_str).unwrap_or("?")),
                esc(d.get("version").and_then(Value::as_str).unwrap_or("?"))
            ),
        }
    };
    let sandbox = setup
        .eng
        .as_ref()
        .and_then(|e| e.get("sandbox"))
        .and_then(Value::as_str)
        .unwrap_or("none");
    let mut b = head(
        "Health & setup",
        "Is everything connected, and what to do next.",
        "",
    );
    b.push_str("<div class=\"grid g2\"><div>");
    b.push_str(&format!(
        "<div class=\"card\"><h2>Setup checklist</h2>{}</div>",
        checklist(&setup)
    ));
    b.push_str("</div><div>");
    b.push_str(&format!(
        "<div class=\"card flush\"><div style=\"padding:1rem 1.2rem .2rem\"><h2>Connections</h2></div><div class=\"scroll\"><table><thead><tr><th>Part</th><th>Status</th><th>Detail</th></tr></thead><tbody>\
<tr><td>Database</td><td><span class=\"pill {}\">{}</span></td><td class=\"muted\">Postgres</td></tr>{}{}\
<tr><td>Agent isolation</td><td><span class=\"pill {}\">{}</span></td><td class=\"muted\">{}</td></tr></tbody></table></div></div>",
        if db_ok { "ok" } else { "bad" },
        if db_ok { "connected" } else { "unreachable" },
        plug("Ticket source", &setup.src),
        plug("Agent engine", &setup.eng),
        if sandbox == "docker" { "ok" } else { "warn" },
        if sandbox == "docker" { "sandboxed" } else { "not sandboxed" },
        if sandbox == "docker" { "Runs in a locked-down Docker container" } else { "Runs as the daemon's user (you accepted the risk)" }
    ));
    b.push_str(&format!(
        "<div class=\"card\"><h2>Work queue</h2><div class=\"grid g3\"><div class=\"stat\"><span class=\"n\">{queued}</span><span class=\"l\">waiting</span></div>\
<div class=\"stat\"><span class=\"n\">{running}</span><span class=\"l\">running now</span></div><div class=\"stat {}\"><span class=\"n\">{failed}</span><span class=\"l\">gave up</span></div></div>\
<p class=\"muted small\" style=\"margin-top:.8rem\">{}{}</p></div>",
        if failed > 0 { "bad" } else { "" },
        if oldest > 30 { format!("The oldest waiting job has waited {}. ", ago(oldest).replace(" ago", "")) } else { "Nothing is stuck. ".into() },
        if paused { "Work is paused." } else { "" }
    ));
    // recent failures
    let rows = sqlx::query("SELECT t.id, t.title, t.external_ref, s.external_id, t.last_error, t.updated_at FROM tickets t JOIN services s ON s.id=t.service_id WHERE t.state='failed' ORDER BY t.updated_at DESC LIMIT 5").fetch_all(pool).await.unwrap_or_default();
    if !rows.is_empty() {
        b.push_str("<div class=\"card\"><h2>Recent failures</h2>");
        for r in rows {
            let id: i64 = r.get("id");
            b.push_str(&format!(
                "<p><a href=\"/tickets/{id}\"><strong>{} #{}</strong></a><br><span class=\"muted small\">{}</span></p>",
                esc(&r.get::<String, _>("external_id")),
                esc(&r.get::<String, _>("external_ref")),
                esc(&r.get::<Option<String>, _>("last_error").unwrap_or_default().chars().take(220).collect::<String>())
            ));
        }
        b.push_str("</div>");
    }
    b.push_str("</div></div>");
    render(&st, &sess.csrf, "health", "Health & setup", &b)
        .await
        .into_response()
}

#[derive(Deserialize)]
pub struct ActivityQuery {
    kind: Option<String>,
}

pub async fn activity(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<ActivityQuery>,
) -> Response {
    let sess = match guard(&st, &headers).await {
        Ok(s) => s,
        Err(r) => return r,
    };
    let kind = q.kind.as_deref().unwrap_or("all");
    let like = match kind {
        "tickets" => "ticket.%",
        "chat" => "chat.%",
        "admin" => "service.%",
        _ => "%",
    };
    let rows = sqlx::query(
        "SELECT a.id, a.actor, a.action, a.ticket_id, a.detail, extract(epoch FROM now()-a.at)::bigint AS secs, a.at::text AS at
         FROM audit_events a WHERE a.action LIKE $1 OR ($1 = '%') ORDER BY a.id DESC LIMIT 200",
    )
    .bind(like)
    .fetch_all(st.orch.pool())
    .await
    .unwrap_or_default();
    let tab = |key: &str, label: &str| {
        format!(
            "<a class=\"btn small{}\" href=\"/activity?kind={key}\">{label}</a>",
            if key == kind { " primary" } else { "" }
        )
    };
    let mut b = head(
        "Activity",
        "Everything tasklane and people did, newest first.",
        &format!(
            "{}{}{}{}",
            tab("all", "All"),
            tab("tickets", "Tickets"),
            tab("chat", "Chat"),
            tab("admin", "Settings")
        ),
    );
    if rows.is_empty() {
        b.push_str(&format!(
            "<div class=\"card\">{}</div>",
            empty("activity", "Nothing yet.")
        ));
    } else {
        b.push_str("<div class=\"card flush\"><div class=\"scroll\"><table><thead><tr><th>When</th><th>Who</th><th>What</th><th>Ticket</th></tr></thead><tbody>");
        for r in rows {
            let secs: i64 = r.get("secs");
            let action: String = r.get("action");
            let (text, _, _) =
                super::tickets::describe_event(&action, &r.get::<Value, _>("detail"));
            let tid: Option<i64> = r.get("ticket_id");
            b.push_str(&format!(
                "<tr><td class=\"muted\" title=\"{}\">{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
                esc(&r.get::<String, _>("at")),
                ago(secs),
                esc(&super::tickets::who(&r.get::<String, _>("actor"))),
                text,
                tid.map(|t| format!("<a href=\"/tickets/{t}\">#{t}</a>")).unwrap_or_default()
            ));
        }
        b.push_str("</tbody></table></div></div>");
    }
    render(&st, &sess.csrf, "activity", "Activity", &b)
        .await
        .into_response()
}
