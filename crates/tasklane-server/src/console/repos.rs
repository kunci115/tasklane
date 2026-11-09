//! Repositories: how far tasklane may go in each, and how the agent should behave there.

use std::sync::Arc;

use axum::extract::{Form, Path, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::Row;

use super::ui::{ago, empty, head, icon, render, repo_link};
use super::{csrf_ok, forbidden, guard, redirect, AppState};
use crate::esc;

pub async fn page(State(st): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let sess = match guard(&st, &headers).await {
        Ok(s) => s,
        Err(r) => return r,
    };
    let source = st.orch.config().source_name.clone();
    let rows = sqlx::query(
        "SELECT s.id, s.external_id, s.mode, s.archived, s.facts, s.agent_profile,
                (SELECT count(*) FROM tickets t WHERE t.service_id = s.id AND t.state NOT LIKE 'closed_%' AND t.state <> 'merged') AS open,
                (SELECT count(*) FROM tickets t WHERE t.service_id = s.id AND t.state = 'merged') AS merged,
                (SELECT count(*) FROM tickets t WHERE t.service_id = s.id AND t.state IN ('awaiting_approval','failed')) AS attention,
                (SELECT extract(epoch FROM now() - max(t.updated_at))::bigint FROM tickets t WHERE t.service_id = s.id) AS last
         FROM services s ORDER BY s.archived, s.external_id",
    )
    .fetch_all(st.orch.pool())
    .await
    .unwrap_or_default();

    let mut b = head(
        "Repositories",
        "Choose how far tasklane may go in each one. New repositories start in <strong>triage</strong>.",
        "<input type=\"text\" placeholder=\"Filter repositories\" data-filter=\".repo-card\" style=\"width:16rem\" aria-label=\"Filter repositories\">",
    );
    b.push_str(
        "<div class=\"card\" style=\"margin-bottom:1rem\"><div class=\"grid g3\">\
<div><span class=\"pill\">watch</span><p class=\"small muted\">Listed only. tasklane does nothing: no reading, no model runs.</p></div>\
<div><span class=\"pill info\">triage</span><p class=\"small muted\">New issues are read and planned. A person must approve before anything changes. The first approval switches the repository to full.</p></div>\
<div><span class=\"pill ok\">full</span><p class=\"small muted\">Small, safe changes by collaborators run to a merge on their own. Anything riskier, or from a stranger, waits for approval.</p></div></div></div>",
    );
    if rows.is_empty() {
        b.push_str(&format!("<div class=\"card\">{}</div>", empty("repo", "No repositories yet. Install your GitHub App on a repository; it shows up here within a minute.")));
    }
    b.push_str("<div class=\"grid g2\">");
    for r in rows {
        let id: i64 = r.get("id");
        let name: String = r.get("external_id");
        let mode: String = r.get("mode");
        let archived: bool = r.get("archived");
        let facts: Value = r.get("facts");
        let profile: Value = r.get("agent_profile");
        let (open, merged, attention): (i64, i64, i64) =
            (r.get("open"), r.get("merged"), r.get("attention"));
        let last: Option<i64> = r.get("last");
        let link = repo_link(&source, &name);
        let seg = |m: &str| {
            format!(
                "<button name=\"mode\" value=\"{m}\" type=\"submit\" aria-pressed=\"{}\">{m}</button>",
                if mode == m { "true" } else { "false" }
            )
        };
        let lang = facts.get("language").and_then(Value::as_str).unwrap_or("");
        let private = facts
            .get("private")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let protected: String = facts
            .get("protected_paths")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(|p| format!("<span class=\"chip\">{}</span>", esc(p)))
                    .collect()
            })
            .unwrap_or_default();
        let sys = profile.get("system").and_then(Value::as_str).unwrap_or("");
        let testc = profile
            .get("test_command")
            .and_then(Value::as_str)
            .unwrap_or("");
        b.push_str(&format!(
            "<div class=\"card repo-card\"><div class=\"row tight\" style=\"align-items:center;gap:.5rem\"><h2 style=\"margin:0\">{}</h2>{}{}{}{}</div>\
<form method=\"post\" action=\"/services/{id}/mode\" style=\"margin:.8rem 0\"><input type=\"hidden\" name=\"csrf\" value=\"{}\"><div class=\"seg\" role=\"group\" aria-label=\"Mode\">{}{}{}</div></form>\
<div class=\"row tight muted small\" style=\"gap:1rem;margin-bottom:.6rem\"><span>{open} open</span><span>{merged} merged</span>{}<span>{}</span></div>\
<h3>Protected paths</h3><div>{}</div><p class=\"hint\">Changes to these always need approval of the exact diff, on top of the built-in list (CI config, Dockerfiles, migrations, keys).</p>\
<details style=\"margin-top:.7rem\"><summary class=\"btn small\">Agent settings</summary><form method=\"post\" action=\"/services/{id}/profile\" style=\"margin-top:.8rem\"><input type=\"hidden\" name=\"csrf\" value=\"{}\">\
<div class=\"field\"><label for=\"s{id}\">Instructions for the agent</label><textarea id=\"s{id}\" name=\"system\" maxlength=\"4000\" placeholder=\"For example: follow the existing code style; never change generated files.\">{}</textarea></div>\
<div class=\"field\"><label for=\"t{id}\">Test command the agent may run</label><input id=\"t{id}\" type=\"text\" name=\"test_command\" maxlength=\"200\" value=\"{}\" placeholder=\"pytest -q\"><div class=\"hint\">One command, no parentheses or line breaks.</div></div>\
<button class=\"btn primary small\">Save</button></form></details></div>",
            if link.is_empty() { esc(&name) } else { format!("<a href=\"{}\" target=\"_blank\" rel=\"noopener\">{} {}</a>", esc(&link), esc(&name), icon("external")) },
            if archived { "<span class=\"pill\">archived</span>" } else { "" },
            if private { "<span class=\"pill\">private</span>" } else { "<span class=\"pill warn\" title=\"Anyone can open issues on a public repository\">public</span>" },
            if lang.is_empty() { String::new() } else { format!("<span class=\"pill info\">{}</span>", esc(lang)) },
            if attention > 0 { format!("<span class=\"pill warn\">{attention} need you</span>") } else { String::new() },
            sess.csrf,
            seg("watch"),
            seg("triage"),
            seg("full"),
            last.map(|s| format!("<span>active {}</span>", ago(s))).unwrap_or_else(|| "<span>no activity yet</span>".into()),
            if mode == "full" { "<span class=\"pill ok\">working on its own</span>" } else if mode == "watch" { "<span class=\"pill\">idle</span>" } else { "<span class=\"pill info\">asks first</span>" },
            if protected.is_empty() { "<span class=\"muted small\">none detected</span>".to_string() } else { protected },
            sess.csrf,
            esc(sys),
            esc(testc)
        ));
    }
    b.push_str("</div>");
    render(&st, &sess.csrf, "repos", "Repositories", &b)
        .await
        .into_response()
}

#[derive(Deserialize)]
pub struct ModeIn {
    csrf: String,
    mode: String,
}

pub async fn set_mode(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Form(f): Form<ModeIn>,
) -> Response {
    if !csrf_ok(&st, &headers, &f.csrf).await {
        return forbidden();
    }
    if !matches!(f.mode.as_str(), "watch" | "triage" | "full") {
        return (axum::http::StatusCode::UNPROCESSABLE_ENTITY, "unknown mode").into_response();
    }
    let _ = sqlx::query("UPDATE services SET mode = $2 WHERE id = $1")
        .bind(id)
        .bind(&f.mode)
        .execute(st.orch.pool())
        .await;
    let _ = sqlx::query("INSERT INTO audit_events (actor, action, detail) VALUES ('console:admin', 'service.mode', $1)")
        .bind(json!({"service_id": id, "mode": f.mode}))
        .execute(st.orch.pool())
        .await;
    redirect("/services")
}

#[derive(Deserialize)]
pub struct ProfileIn {
    csrf: String,
    system: String,
    test_command: String,
}

pub async fn set_profile(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Form(f): Form<ProfileIn>,
) -> Response {
    if !csrf_ok(&st, &headers, &f.csrf).await {
        return forbidden();
    }
    let system = f.system.trim();
    let cmd = f.test_command.trim();
    if system.chars().count() > 4000
        || cmd.chars().count() > 200
        || cmd.contains('\n')
        || cmd.contains('\r')
        || cmd.contains('(')
        || cmd.contains(')')
    {
        return (
            axum::http::StatusCode::UNPROCESSABLE_ENTITY,
            "invalid agent settings",
        )
            .into_response();
    }
    let _ = sqlx::query("UPDATE services SET agent_profile = agent_profile || $2 WHERE id = $1")
        .bind(id)
        .bind(json!({"system": system, "test_command": cmd}))
        .execute(st.orch.pool())
        .await;
    let _ = sqlx::query("INSERT INTO audit_events (actor, action, detail) VALUES ('console:admin', 'service.profile', $1)")
        .bind(json!({"service_id": id}))
        .execute(st.orch.pool())
        .await;
    redirect("/services")
}
