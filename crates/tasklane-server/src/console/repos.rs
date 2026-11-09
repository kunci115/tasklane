//! Repositories: how far tasklane may go in each, and how the agent should behave there.

use std::sync::Arc;

use axum::extract::{Form, Path, Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::Row;

use super::ui::{ago, empty, head, icon, pager, render, repo_link, url_encode};
use super::{csrf_ok, forbidden, guard, redirect, AppState};
use crate::esc;

#[derive(Deserialize, Default)]
pub struct ListQuery {
    q: Option<String>,
    mode: Option<String>,
    page: Option<u32>,
    per: Option<u32>,
}

/// `%`, `_` and `\` mean something in a LIKE pattern; the search text is meant literally.
pub(super) fn like_literal(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

pub async fn page(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(lq): Query<ListQuery>,
) -> Response {
    let sess = match guard(&st, &headers).await {
        Ok(s) => s,
        Err(r) => return r,
    };
    let source = st.orch.config().source_name.clone();
    let q: String =
        lq.q.as_deref()
            .unwrap_or("")
            .trim()
            .chars()
            .take(100)
            .collect();
    let mode_f = lq
        .mode
        .as_deref()
        .filter(|m| matches!(*m, "watch" | "triage" | "full"))
        .unwrap_or("all");
    let per = lq.per.filter(|p| matches!(*p, 12 | 24 | 48)).unwrap_or(12);
    let pattern = format!("%{}%", like_literal(&q));
    // how many repositories each mode has, for the filter tabs (they follow the search text)
    let mut counts = std::collections::HashMap::new();
    for r in sqlx::query("SELECT mode, count(*) AS n FROM services WHERE external_id ILIKE $1 ESCAPE '\\' GROUP BY mode")
        .bind(&pattern)
        .fetch_all(st.orch.pool())
        .await
        .unwrap_or_default()
    {
        counts.insert(r.get::<String, _>("mode"), r.get::<i64, _>("n"));
    }
    let all_n: i64 = counts.values().sum();
    let total: i64 = if mode_f == "all" {
        all_n
    } else {
        *counts.get(mode_f).unwrap_or(&0)
    };
    let pages = ((total + per as i64 - 1) / per as i64).max(1) as u32;
    let page = lq.page.unwrap_or(1).clamp(1, pages);
    let rows = sqlx::query(
        "SELECT s.id, s.external_id, s.mode, s.archived, s.facts, s.agent_profile, s.owner_state, s.owner_name,
                (SELECT count(*) FROM tickets t WHERE t.service_id = s.id AND t.state NOT LIKE 'closed_%' AND t.state <> 'merged') AS open,
                (SELECT count(*) FROM tickets t WHERE t.service_id = s.id AND t.state = 'merged') AS merged,
                (SELECT count(*) FROM tickets t WHERE t.service_id = s.id AND t.state IN ('awaiting_approval','failed')) AS attention,
                (SELECT extract(epoch FROM now() - max(t.updated_at))::bigint FROM tickets t WHERE t.service_id = s.id) AS last
         FROM services s
         WHERE s.external_id ILIKE $1 ESCAPE '\\' AND ($2 = 'all' OR s.mode = $2)
         ORDER BY s.archived, s.external_id LIMIT $3 OFFSET $4",
    )
    .bind(&pattern)
    .bind(mode_f)
    .bind(per as i64)
    .bind((page as i64 - 1) * per as i64)
    .fetch_all(st.orch.pool())
    .await
    .unwrap_or_default();
    // the query string without the page number: used by the tabs, the pager and the return link
    let base_qs = |m: &str| {
        let mut v = Vec::new();
        if !q.is_empty() {
            v.push(format!("q={}", url_encode(&q)));
        }
        if m != "all" {
            v.push(format!("mode={m}"));
        }
        if per != 12 {
            v.push(format!("per={per}"));
        }
        v.join("&")
    };
    let here = {
        let b = base_qs(mode_f);
        if page > 1 {
            if b.is_empty() {
                format!("page={page}")
            } else {
                format!("{b}&page={page}")
            }
        } else {
            b
        }
    };

    let mut b = head(
        "Repositories",
        "Choose how far tasklane may go in each one. New repositories start in <strong>triage</strong>.",
        &format!(
            "<form method=\"get\" action=\"/services\" class=\"row tight\" role=\"search\"><input type=\"search\" name=\"q\" value=\"{}\" placeholder=\"Search repositories\" style=\"width:16rem\" aria-label=\"Search repositories\" maxlength=\"100\">{}{}<button class=\"btn\">Search</button>{}</form>",
            esc(&q),
            if mode_f != "all" { format!("<input type=\"hidden\" name=\"mode\" value=\"{mode_f}\">") } else { String::new() },
            if per != 12 { format!("<input type=\"hidden\" name=\"per\" value=\"{per}\">") } else { String::new() },
            if q.is_empty() { String::new() } else { "<a class=\"btn ghost\" href=\"/services\">Clear</a>".to_string() }
        ),
    );
    b.push_str(
        "<div class=\"card\" style=\"margin-bottom:1rem\"><div class=\"grid g3\">\
<div><span class=\"pill\">watch</span><p class=\"small muted\">Listed only. tasklane does nothing: no reading, no model runs.</p></div>\
<div><span class=\"pill info\">triage</span><p class=\"small muted\">New issues are read and planned. A person must approve before anything changes. The first approval switches the repository to full.</p></div>\
<div><span class=\"pill ok\">full</span><p class=\"small muted\">Small, safe changes by collaborators run to a merge on their own. Anything riskier, or from a stranger, waits for approval.</p></div></div></div>",
    );
    let tab = |key: &str, label: &str, n: i64| {
        let qs = base_qs(key);
        format!(
            "<a class=\"tab\" href=\"/services{}{}\" aria-current=\"{}\">{label} <span class=\"n\">{n}</span></a>",
            if qs.is_empty() { "" } else { "?" },
            qs,
            if mode_f == key { "true" } else { "false" }
        )
    };
    b.push_str(&format!(
        "<div class=\"tabs\" role=\"navigation\" aria-label=\"Filter by mode\">{}{}{}{}</div>",
        tab("all", "All", all_n),
        tab("full", "Full", *counts.get("full").unwrap_or(&0)),
        tab("triage", "Triage", *counts.get("triage").unwrap_or(&0)),
        tab("watch", "Watch", *counts.get("watch").unwrap_or(&0)),
    ));
    if let Ok(i) = st.orch.instance().await {
        b.push_str(&format!(
            "<p class=\"hint\" style=\"margin:-.4rem 0 1rem\">This install is called <span class=\"mono\">{}</span>. It claims a repository on the ticket source the first time an issue arrives, so a second tasklane on the same repository stays out of its way.</p>",
            esc(&i.tag())
        ));
    }
    if rows.is_empty() {
        b.push_str(&format!(
            "<div class=\"card\">{}</div>",
            if all_n == 0 && q.is_empty() {
                empty("repo", "No repositories yet. Install your GitHub App on a repository; it shows up here within a minute.")
            } else {
                empty("repo", "No repository matches. Clear the search or pick another mode.")
            }
        ));
    }
    b.push_str("<div class=\"grid g2\">");
    let mut notes_by: std::collections::HashMap<i64, Vec<(i64, String, String)>> =
        Default::default();
    for r in &rows {
        let id: i64 = r.get("id");
        notes_by.insert(
            id,
            tasklane_orchestrator::memory::list(st.orch.pool(), id)
                .await
                .unwrap_or_default(),
        );
    }
    for r in rows {
        let id: i64 = r.get("id");
        let name: String = r.get("external_id");
        let mode: String = r.get("mode");
        let archived: bool = r.get("archived");
        let owner_state: String = r.get("owner_state");
        let owner_name: Option<String> = r.get("owner_name");
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
<form method=\"post\" action=\"/services/{id}/mode\" style=\"margin:.8rem 0\"><input type=\"hidden\" name=\"csrf\" value=\"{}\"><input type=\"hidden\" name=\"ret\" value=\"{here}\"><div class=\"seg\" role=\"group\" aria-label=\"Mode\">{}{}{}</div></form>\
{}<div class=\"row tight muted small\" style=\"gap:1rem;margin-bottom:.6rem\"><span>{open} open</span><span>{merged} merged</span>{}<span>{}</span></div>\
<details style=\"margin-top:.7rem\"><summary class=\"btn small\">Settings and protected paths</summary><h3 style=\"margin-top:.9rem\">Protected paths</h3><div>{}</div><p class=\"hint\">Changes to these always need approval of the exact diff, on top of the built-in list (CI config, Dockerfiles, migrations, keys).</p><form method=\"post\" action=\"/services/{id}/profile\" style=\"margin-top:.8rem\"><input type=\"hidden\" name=\"csrf\" value=\"{}\"><input type=\"hidden\" name=\"ret\" value=\"{here}\">\
<div class=\"field\"><label for=\"s{id}\">Instructions for the agent</label><textarea id=\"s{id}\" name=\"system\" maxlength=\"4000\" placeholder=\"For example: follow the existing code style; never change generated files.\">{}</textarea></div>\
<div class=\"field\"><label for=\"t{id}\">Test command the agent may run</label><input id=\"t{id}\" type=\"text\" name=\"test_command\" maxlength=\"200\" value=\"{}\" placeholder=\"pytest -q\"><div class=\"hint\">One command, no parentheses or line breaks.</div></div>\
<button class=\"btn primary small\">Save</button></form>{}</details></div>",
            if link.is_empty() { esc(&name) } else { format!("<a href=\"{}\" target=\"_blank\" rel=\"noopener\">{} {}</a>", esc(&link), esc(&name), icon("external")) },
            if archived { "<span class=\"pill\">archived</span>" } else { "" },
            if private { "<span class=\"pill\">private</span>" } else { "<span class=\"pill warn\" title=\"Anyone can open issues on a public repository\">public</span>" },
            if lang.is_empty() { String::new() } else { format!("<span class=\"pill info\">{}</span>", esc(lang)) },
            if attention > 0 { format!("<span class=\"pill warn\">{attention} need you</span>") } else { String::new() },
            sess.csrf,
            seg("watch"),
            seg("triage"),
            seg("full"),
            if owner_state == "foreign" {
                format!("<div class=\"notice warn\"><strong>Handled by another tasklane install</strong> ({}). This one leaves the repository alone, so the two do not answer the same issues twice.\
<form method=\"post\" action=\"/services/{id}/takeover\" style=\"margin-top:.5rem\"><input type=\"hidden\" name=\"csrf\" value=\"{}\"><input type=\"hidden\" name=\"ret\" value=\"{here}\"><button class=\"btn small danger\" data-confirm=\"Take this repository over? The other install loses it and stops acting on it.\">Take over</button></form></div>", esc(owner_name.as_deref().unwrap_or("unknown")), sess.csrf)
            } else {
                String::new()
            },
            last.map(|s| format!("<span>active {}</span>", ago(s))).unwrap_or_else(|| "<span>no activity yet</span>".into()),
            if mode == "full" { "<span class=\"pill ok\">working on its own</span>" } else if mode == "watch" { "<span class=\"pill\">idle</span>" } else { "<span class=\"pill info\">asks first</span>" },
            if protected.is_empty() { "<span class=\"muted small\">none detected</span>".to_string() } else { protected },
            sess.csrf,
            esc(sys),
            esc(testc),
            notes_html(id, notes_by.get(&id).map(Vec::as_slice).unwrap_or(&[]), &sess.csrf, &here)
        ));
    }
    b.push_str("</div>");
    if total > 0 {
        let qs = base_qs(mode_f);
        b.push_str(&pager(
            &format!("/services{}{}", if qs.is_empty() { "" } else { "?" }, qs),
            page,
            pages,
            total,
            per,
        ));
    }
    render(&st, &sess.csrf, "repos", "Repositories", &b)
        .await
        .into_response()
}

/// The notes the agents are given for one repository, with a form to add one.
fn notes_html(id: i64, notes: &[(i64, String, String)], csrf: &str, here: &str) -> String {
    let mut h = String::from("<h3 style=\"margin-top:1.1rem\">Notes for the agents</h3><p class=\"hint\">Short facts every agent is told about this repository, for example &ldquo;the staging branch is develop&rdquo;. Only you write these.</p>");
    for (nid, fact, who) in notes {
        h.push_str(&format!(
            "<form method=\"post\" action=\"/services/{id}/notes/{nid}/delete\" class=\"row tight\" style=\"align-items:flex-start;gap:.5rem;margin:.3rem 0\"><input type=\"hidden\" name=\"csrf\" value=\"{}\"><input type=\"hidden\" name=\"ret\" value=\"{}\"><span style=\"flex:1\">{}<br><span class=\"muted small\">{}</span></span><button class=\"btn small ghost\" data-confirm=\"Remove this note?\">Remove</button></form>",
            esc(csrf),
            esc(here),
            esc(fact),
            esc(who)
        ));
    }
    h.push_str(&format!(
        "<form method=\"post\" action=\"/services/{id}/notes\" style=\"margin-top:.5rem\"><input type=\"hidden\" name=\"csrf\" value=\"{}\"><input type=\"hidden\" name=\"ret\" value=\"{}\"><div class=\"field\"><label for=\"n{id}\">New note</label><input id=\"n{id}\" type=\"text\" name=\"fact\" maxlength=\"1000\" placeholder=\"The staging branch is develop\"></div><button class=\"btn small\">Add note</button></form>",
        esc(csrf),
        esc(here)
    ));
    h
}

#[derive(Deserialize)]
pub struct NoteIn {
    csrf: String,
    fact: String,
    ret: Option<String>,
}

#[derive(Deserialize)]
pub struct NoteDeleteIn {
    csrf: String,
    ret: Option<String>,
}

pub async fn add_note(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Form(f): Form<NoteIn>,
) -> Response {
    if !csrf_ok(&st, &headers, &f.csrf).await {
        return forbidden();
    }
    match tasklane_orchestrator::memory::add(st.orch.pool(), id, &f.fact, "console:admin").await {
        Ok(_) => back(f.ret.as_deref()),
        Err(_) => (
            axum::http::StatusCode::UNPROCESSABLE_ENTITY,
            "a note needs 1 to 1000 characters",
        )
            .into_response(),
    }
}

pub async fn delete_note(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((id, nid)): Path<(i64, i64)>,
    Form(f): Form<NoteDeleteIn>,
) -> Response {
    if !csrf_ok(&st, &headers, &f.csrf).await {
        return forbidden();
    }
    let _ = tasklane_orchestrator::memory::delete(st.orch.pool(), id, nid).await;
    back(f.ret.as_deref())
}

#[derive(Deserialize)]
pub struct ModeIn {
    csrf: String,
    mode: String,
    ret: Option<String>,
}

/// Back to the same filtered page after a change. Only a plain query string is accepted.
fn back(ret: Option<&str>) -> Response {
    match ret.filter(|r| {
        r.len() <= 300
            && r.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"=&%+-_.~".contains(&b))
    }) {
        Some(r) if !r.is_empty() => redirect(&format!("/services?{r}")),
        _ => redirect("/services"),
    }
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
    if f.mode == "watch" {
        // a repository nobody acts on gives its claim back, so another install may take it
        let orch = st.orch.clone();
        tokio::spawn(async move { orch.release_owner(id).await });
    }
    back(f.ret.as_deref())
}

#[derive(Deserialize)]
pub struct TakeoverIn {
    csrf: String,
    ret: Option<String>,
}

pub async fn take_over(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Form(f): Form<TakeoverIn>,
) -> Response {
    if !csrf_ok(&st, &headers, &f.csrf).await {
        return forbidden();
    }
    match st.orch.take_over(id).await {
        Ok(_) => back(f.ret.as_deref()),
        Err(e) => {
            eprintln!("take over {id}: {e}");
            (
                axum::http::StatusCode::BAD_GATEWAY,
                "The take-over did not work. The ticket source refused or could not be reached; try again in a minute.",
            )
                .into_response()
        }
    }
}

#[derive(Deserialize)]
pub struct ProfileIn {
    csrf: String,
    system: String,
    test_command: String,
    ret: Option<String>,
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
    back(f.ret.as_deref())
}
