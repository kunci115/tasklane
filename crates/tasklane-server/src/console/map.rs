//! The live map: repositories, tickets and agents as a graph, live or replayed.
//! The page only draws; this module serves the data it draws from.

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Json, Response};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::Row;

use super::ui::{head, icon, render};
use super::{guard, AppState};

/// Which agent a running job belongs to.
pub fn agent_for_job(kind: &str) -> Option<&'static str> {
    match kind {
        "triage" => Some("triage"),
        "develop" => Some("develop"),
        "review" => Some("review"),
        "ci" => Some("ci"),
        "pr" | "merge" => Some("runner"),
        _ => None,
    }
}

pub async fn page(State(st): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let sess = match guard(&st, &headers).await {
        Ok(s) => s,
        Err(r) => return r,
    };
    let mut b = head(
        "Live map",
        "Repositories, tickets and the agents working on them. Glowing agents are busy right now.",
        "<div class=\"seg\" role=\"group\" aria-label=\"Mode\"><button type=\"button\" data-map-mode=\"live\" aria-pressed=\"true\">Live</button><button type=\"button\" data-map-mode=\"replay\" aria-pressed=\"false\">Replay</button></div>",
    );
    b.push_str(&format!(
        "<div class=\"mapwrap\"><canvas id=\"map\" aria-label=\"Map of repositories, tickets and agents\" role=\"img\"></canvas>\
<div class=\"maplegend\" id=\"maplegend\"></div><div class=\"maptip\" id=\"maptip\" hidden></div>\
<div class=\"mapnow\" id=\"mapnow\"><h3>Working now</h3><div id=\"mapnow-list\" class=\"small\">Nothing running.</div></div>\
<div class=\"mapbar\" id=\"mapbar\" hidden><button class=\"btn small\" id=\"mp-play\" type=\"button\">{} <span>Play</span></button>\
<input id=\"mp-slider\" type=\"range\" min=\"0\" max=\"1000\" value=\"0\" aria-label=\"Time\"><span id=\"mp-clock\" class=\"small muted mono\">--</span>\
<select id=\"mp-speed\" aria-label=\"Speed\"><option value=\"1\">1x</option><option value=\"10\">10x</option><option value=\"60\" selected>60x</option><option value=\"300\">300x</option><option value=\"1200\">1200x</option></select>\
<select id=\"mp-range\" aria-label=\"Range\"><option value=\"1\">last hour</option><option value=\"6\">last 6 hours</option><option value=\"24\" selected>last 24 hours</option><option value=\"168\">last 7 days</option></select></div></div>\
<p class=\"hint\">Click a ticket to open it. Dots are tickets coloured by state; a line from a dot to an agent means that agent has it.</p><script src=\"/static/map.js\" defer></script>",
        icon("play")
    ));
    render(&st, &sess.csrf, "map", "Live map", &b)
        .await
        .into_response()
}

/// Everything the live view needs, in one small document.
pub async fn snapshot(State(st): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if let Err(r) = guard(&st, &headers).await {
        return r;
    }
    let pool = st.orch.pool();
    let paused = st.orch.is_paused().await.unwrap_or(false);
    let repos = sqlx::query(
        "SELECT s.id, s.external_id, s.mode,
                (SELECT count(*) FROM tickets t WHERE t.service_id = s.id AND t.state NOT LIKE 'closed_%' AND t.state <> 'merged') AS open
         FROM services s WHERE NOT s.archived ORDER BY s.external_id",
    )
    .fetch_all(pool)
    .await
    .unwrap_or_default()
    .into_iter()
    .map(|r| json!({"id": r.get::<i64, _>("id"), "name": r.get::<String, _>("external_id"), "mode": r.get::<String, _>("mode"), "open": r.get::<i64, _>("open")}))
    .collect::<Vec<_>>();
    let tickets = sqlx::query(
        "SELECT t.id, t.service_id, coalesce(t.title, '#' || t.external_ref) AS title, t.state, t.level, extract(epoch FROM now() - t.updated_at)::bigint AS age
         FROM tickets t JOIN services s ON s.id = t.service_id
         WHERE NOT s.archived AND (t.state NOT LIKE 'closed_%' AND t.state <> 'merged' OR t.updated_at > now() - interval '6 hours')
         ORDER BY t.updated_at DESC LIMIT 300",
    )
    .fetch_all(pool)
    .await
    .unwrap_or_default()
    .into_iter()
    .map(|r| json!({"id": r.get::<i64, _>("id"), "repo": r.get::<i64, _>("service_id"), "title": r.get::<String, _>("title"), "state": r.get::<String, _>("state"), "level": r.get::<Option<i16>, _>("level"), "age": r.get::<i64, _>("age")}))
    .collect::<Vec<_>>();
    let running = sqlx::query("SELECT j.kind, j.ticket_id, extract(epoch FROM now() - j.lease_until)::bigint * -1 AS left_secs, coalesce(t.title, '#' || t.external_ref) AS title FROM jobs j JOIN tickets t ON t.id = j.ticket_id WHERE j.status = 'running'")
        .fetch_all(pool)
        .await
        .unwrap_or_default();
    let mut busy: Vec<Value> = vec![];
    for r in running {
        let kind: String = r.get("kind");
        if let Some(a) = agent_for_job(&kind) {
            busy.push(json!({"agent": a, "kind": kind, "ticket": r.get::<i64, _>("ticket_id"), "title": r.get::<String, _>("title")}));
        }
    }
    let recent = sqlx::query("SELECT ticket_id, action, extract(epoch FROM now() - at)::bigint AS secs FROM audit_events WHERE ticket_id IS NOT NULL AND at > now() - interval '90 seconds' ORDER BY id")
        .fetch_all(pool)
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|r| json!({"ticket": r.get::<i64, _>("ticket_id"), "action": r.get::<String, _>("action"), "secs": r.get::<i64, _>("secs")}))
        .collect::<Vec<_>>();
    Json(json!({"paused": paused, "repos": repos, "tickets": tickets, "busy": busy, "recent": recent})).into_response()
}

#[derive(Deserialize)]
pub struct ReplayQuery {
    hours: Option<i64>,
}

/// The audit events of the last N hours with just enough about each ticket, in time order.
pub async fn replay(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<ReplayQuery>,
) -> Response {
    if let Err(r) = guard(&st, &headers).await {
        return r;
    }
    let hours = q.hours.unwrap_or(24).clamp(1, 24 * 14);
    let pool = st.orch.pool();
    let events = sqlx::query(
        "SELECT extract(epoch FROM a.at)::bigint AS t, a.ticket_id, a.action, a.detail
         FROM audit_events a WHERE a.ticket_id IS NOT NULL AND a.at > now() - make_interval(hours => $1::int)
         ORDER BY a.id LIMIT 4000",
    )
    .bind(hours as i32)
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    let ev: Vec<Value> = events
        .iter()
        .map(|r| {
            let d: Value = r.get("detail");
            // only the few fields the replay needs, never free text
            let small = json!({"to": d.get("to"), "decision": d.get("decision"), "verdict": d.get("verdict"), "run": d.get("run")});
            json!({"t": r.get::<i64, _>("t"), "ticket": r.get::<i64, _>("ticket_id"), "action": r.get::<String, _>("action"), "d": small})
        })
        .collect();
    let ids: Vec<i64> = events
        .iter()
        .filter_map(|r| r.get::<Option<i64>, _>("ticket_id"))
        .collect();
    let tickets = sqlx::query("SELECT t.id, t.service_id, coalesce(t.title, '#' || t.external_ref) AS title, t.level FROM tickets t WHERE t.id = ANY($1)")
        .bind(&ids)
        .fetch_all(pool)
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|r| json!({"id": r.get::<i64, _>("id"), "repo": r.get::<i64, _>("service_id"), "title": r.get::<String, _>("title"), "level": r.get::<Option<i16>, _>("level")}))
        .collect::<Vec<_>>();
    let repos = sqlx::query("SELECT id, external_id, mode FROM services WHERE NOT archived ORDER BY external_id")
        .fetch_all(pool)
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|r| json!({"id": r.get::<i64, _>("id"), "name": r.get::<String, _>("external_id"), "mode": r.get::<String, _>("mode"), "open": 0}))
        .collect::<Vec<_>>();
    let now: i64 = sqlx::query_scalar("SELECT extract(epoch FROM now())::bigint")
        .fetch_one(pool)
        .await
        .unwrap_or(0);
    Json(json!({"now": now, "hours": hours, "events": ev, "tickets": tickets, "repos": repos}))
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_job_kind_belongs_to_one_agent() {
        assert_eq!(agent_for_job("triage"), Some("triage"));
        assert_eq!(agent_for_job("develop"), Some("develop"));
        assert_eq!(agent_for_job("pr"), Some("runner"));
        assert_eq!(agent_for_job("merge"), Some("runner"));
        assert_eq!(agent_for_job("ci"), Some("ci"));
        assert_eq!(agent_for_job("review"), Some("review"));
        assert_eq!(agent_for_job("unknown"), None);
    }
}
