//! The admin console: server-rendered pages, one admin token, no JavaScript.
//! Every value that comes from a ticket is escaped. Every state-changing form
//! carries a CSRF token, and the session cookie is HttpOnly and SameSite=Strict.

use std::sync::Arc;

use axum::extract::{Form, Path, Query, State};
use axum::http::header::{COOKIE, LOCATION, SET_COOKIE};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use serde::Deserialize;
use sqlx::Row;
use tasklane_orchestrator::approval::{decide, Decision, Principal};

use crate::auth::{ct_eq, hmac_hex};
use crate::{esc, AppState};

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/", get(home))
        .route("/login", get(login_form).post(login))
        .route("/tickets/{id}", get(ticket_page))
        .route("/tickets/{id}/decide", post(decide_form))
        .route("/services", get(services_page))
        .route("/services/{id}/mode", post(set_mode))
        .route("/chat", get(chat_page))
        .route("/chat/groups", post(add_group))
        .route("/chat/groups/{id}/delete", post(delete_group))
        .route("/pause", post(pause))
        .route("/resume", post(resume))
}

fn session_value(st: &AppState) -> String {
    hmac_hex(st.admin_token.as_bytes(), "session")
}
fn csrf_value(st: &AppState) -> String {
    hmac_hex(st.admin_token.as_bytes(), "csrf")
}

fn logged_in(st: &AppState, headers: &HeaderMap) -> bool {
    let want = session_value(st);
    headers
        .get(COOKIE)
        .and_then(|v| v.to_str().ok())
        .map(|c| {
            c.split(';')
                .filter_map(|p| p.trim().strip_prefix("tl="))
                .any(|v| ct_eq(v, &want))
        })
        .unwrap_or(false)
}

fn redirect(to: &str) -> Response {
    (StatusCode::SEE_OTHER, [(LOCATION, to.to_string())]).into_response()
}

fn page(title: &str, body: &str) -> Html<String> {
    Html(format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
<title>{t} - tasklane</title><style>\
body{{font:15px/1.5 system-ui,sans-serif;max-width:60rem;margin:2rem auto;padding:0 1rem;color:#1d2330}}\
table{{border-collapse:collapse;width:100%}}th,td{{text-align:left;padding:.4rem .6rem;border-bottom:1px solid #dde1ea}}\
a{{color:#0b5fff}}.pill{{border-radius:1em;padding:0 .6em;background:#eef1f7}}.warn{{background:#fff3cd}}\
button{{font:inherit;padding:.3rem .8rem;margin-right:.4rem}}code,pre{{background:#f4f6fa;padding:.1rem .3rem}}pre{{padding:.6rem;overflow:auto}}\
nav a{{margin-right:1rem}}</style></head><body><nav><a href=\"/\">Tickets</a><a href=\"/services\">Repositories</a><a href=\"/chat\">Chat groups</a></nav>{b}</body></html>",
        t = esc(title),
        b = body
    ))
}

async fn login_form() -> Html<String> {
    page("Sign in", "<h1>Sign in</h1><form method=\"post\" action=\"/login\"><label>Admin token <input type=\"password\" name=\"token\" autocomplete=\"current-password\" required></label> <button>Sign in</button></form>")
}

#[derive(Deserialize)]
struct LoginIn {
    token: String,
}

async fn login(State(st): State<Arc<AppState>>, Form(f): Form<LoginIn>) -> Response {
    if !ct_eq(&f.token, &st.admin_token) {
        return (StatusCode::UNAUTHORIZED, page("Sign in", "<h1>Sign in</h1><p>That token is not right.</p><p><a href=\"/login\">Try again</a></p>")).into_response();
    }
    let secure = if st.secure_cookies { "; Secure" } else { "" };
    (
        StatusCode::SEE_OTHER,
        [
            (
                SET_COOKIE,
                format!(
                    "tl={}; HttpOnly; SameSite=Strict; Path=/; Max-Age=28800{secure}",
                    session_value(&st)
                ),
            ),
            (LOCATION, "/".to_string()),
        ],
    )
        .into_response()
}

#[derive(Deserialize)]
struct HomeQuery {
    state: Option<String>,
}

async fn home(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<HomeQuery>,
) -> Response {
    if !logged_in(&st, &headers) {
        return redirect("/login");
    }
    let pool = st.orch.pool();
    let paused = st.orch.is_paused().await.unwrap_or(false);
    let rows = sqlx::query(
        "SELECT t.id, t.state, t.level, t.external_ref, t.title, s.external_id, t.updated_at::text AS updated
         FROM tickets t JOIN services s ON s.id = t.service_id
         WHERE ($1::text IS NULL OR t.state = $1) ORDER BY t.updated_at DESC LIMIT 100",
    )
    .bind(q.state.as_deref())
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    let mut b = String::from("<h1>Tickets</h1>");
    b.push_str(&format!(
        "<form method=\"post\" action=\"{}\"><input type=\"hidden\" name=\"csrf\" value=\"{}\"><p>Work is <strong>{}</strong>. <button>{}</button></p></form>",
        if paused { "/resume" } else { "/pause" },
        csrf_value(&st),
        if paused { "paused" } else { "running" },
        if paused { "Resume" } else { "Pause everything" }
    ));
    b.push_str("<p>Show: <a href=\"/\">all</a> | <a href=\"/?state=awaiting_approval\">waiting for approval</a> | <a href=\"/?state=failed\">failed</a></p>");
    b.push_str("<table><tr><th>#</th><th>Repository</th><th>Ticket</th><th>State</th><th>Level</th><th>Updated</th></tr>");
    for r in rows {
        let id: i64 = r.get("id");
        let state: String = r.get("state");
        let level: Option<i16> = r.get("level");
        let ext: String = r.get("external_ref");
        let title: Option<String> = r.get("title");
        let service: String = r.get("external_id");
        let updated: String = r.get("updated");
        b.push_str(&format!(
            "<tr><td><a href=\"/tickets/{id}\">{id}</a></td><td>{}</td><td>{}{}</td><td><span class=\"pill{}\">{}</span></td><td>{}</td><td>{}</td></tr>",
            esc(&service),
            esc(&ext),
            title.map(|t| format!(" {}", esc(&t))).unwrap_or_default(),
            if state == "awaiting_approval" || state == "failed" { " warn" } else { "" },
            esc(&state),
            level.map(|l| l.to_string()).unwrap_or_default(),
            esc(&updated)
        ));
    }
    b.push_str("</table>");
    page("Tickets", &b).into_response()
}

async fn ticket_page(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Response {
    if !logged_in(&st, &headers) {
        return redirect("/login");
    }
    let pool = st.orch.pool();
    let Some(r) = sqlx::query(
        "SELECT t.state, t.level, t.level_reasons::text AS reasons, t.plan_hash, t.external_ref, t.title, t.branch, t.pr_ref, t.last_error, s.external_id
         FROM tickets t JOIN services s ON s.id = t.service_id WHERE t.id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
    .unwrap_or(None) else {
        return (StatusCode::NOT_FOUND, page("Not found", "<p>No such ticket.</p>")).into_response();
    };
    let state: String = r.get("state");
    let level: Option<i16> = r.get("level");
    let plan_hash: Option<String> = r.get("plan_hash");
    let mut b = format!(
        "<h1>Ticket {id}</h1><p>{} #{} {}</p><table>\
<tr><th>State</th><td>{}</td></tr><tr><th>Level</th><td>{}</td></tr><tr><th>Why</th><td><code>{}</code></td></tr>\
<tr><th>Branch</th><td>{}</td></tr><tr><th>Pull request</th><td>{}</td></tr><tr><th>Last error</th><td>{}</td></tr></table>",
        esc(&r.get::<String, _>("external_id")),
        esc(&r.get::<String, _>("external_ref")),
        esc(&r.get::<Option<String>, _>("title").unwrap_or_default()),
        esc(&state),
        level.map(|l| l.to_string()).unwrap_or_default(),
        esc(&r.get::<Option<String>, _>("reasons").unwrap_or_default()),
        esc(&r.get::<Option<String>, _>("branch").unwrap_or_default()),
        esc(&r.get::<Option<String>, _>("pr_ref").unwrap_or_default()),
        esc(&r.get::<Option<String>, _>("last_error").unwrap_or_default()),
    );
    if state == "awaiting_approval" {
        if let Some(h) = &plan_hash {
            b.push_str(&format!(
                "<h2>Decision</h2><p>Approving applies to this exact plan or diff: <code>{}</code></p>\
<form method=\"post\" action=\"/tickets/{id}/decide\"><input type=\"hidden\" name=\"csrf\" value=\"{}\"><input type=\"hidden\" name=\"plan_hash\" value=\"{}\">\
<button name=\"decision\" value=\"approve\">Approve</button><button name=\"decision\" value=\"reject\">Reject</button></form>",
                esc(&h[..16.min(h.len())]),
                csrf_value(&st),
                esc(h)
            ));
        }
    }
    b.push_str(
        "<h2>History</h2><table><tr><th>When</th><th>Who</th><th>What</th><th>Detail</th></tr>",
    );
    let audit = sqlx::query("SELECT at::text AS at, actor, action, detail::text AS detail FROM audit_events WHERE ticket_id = $1 ORDER BY id DESC LIMIT 100")
        .bind(id)
        .fetch_all(pool)
        .await
        .unwrap_or_default();
    for a in audit {
        b.push_str(&format!(
            "<tr><td>{}</td><td>{}</td><td>{}</td><td><code>{}</code></td></tr>",
            esc(&a.get::<String, _>("at")),
            esc(&a.get::<String, _>("actor")),
            esc(&a.get::<String, _>("action")),
            esc(&a.get::<String, _>("detail"))
        ));
    }
    b.push_str("</table>");
    page(&format!("Ticket {id}"), &b).into_response()
}

#[derive(Deserialize)]
struct DecideIn {
    csrf: String,
    plan_hash: String,
    decision: String,
}

async fn decide_form(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Form(f): Form<DecideIn>,
) -> Response {
    if !logged_in(&st, &headers) || !ct_eq(&f.csrf, &csrf_value(&st)) {
        return (
            StatusCode::FORBIDDEN,
            page(
                "Refused",
                "<p>Session or form token is not valid. Sign in again.</p>",
            ),
        )
            .into_response();
    }
    let decision = if f.decision == "approve" {
        Decision::Approve
    } else {
        Decision::Reject
    };
    let who = Principal {
        channel: "console".into(),
        reference: "admin".into(),
    };
    match decide(st.orch.pool(), id, &who, "console", decision, &f.plan_hash).await {
        Ok(_) => redirect(&format!("/tickets/{id}")),
        Err(e) => (
            StatusCode::CONFLICT,
            page(
                "Not done",
                &format!(
                    "<p>{}</p><p><a href=\"/tickets/{id}\">Back to the ticket</a></p>",
                    esc(&e.to_string())
                ),
            ),
        )
            .into_response(),
    }
}

async fn services_page(State(st): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if !logged_in(&st, &headers) {
        return redirect("/login");
    }
    let rows =
        sqlx::query("SELECT id, external_id, mode, archived FROM services ORDER BY external_id")
            .fetch_all(st.orch.pool())
            .await
            .unwrap_or_default();
    let mut b = String::from("<h1>Repositories</h1><p><strong>watch</strong>: listed only. <strong>triage</strong>: tickets are read and planned; a person must approve before any change. <strong>full</strong>: the whole pipeline runs for approved levels.</p><table><tr><th>Repository</th><th>Mode</th><th></th></tr>");
    for r in rows {
        let id: i64 = r.get("id");
        let mode: String = r.get("mode");
        let archived: bool = r.get("archived");
        b.push_str(&format!(
            "<tr><td>{}{}</td><td>{}</td><td><form method=\"post\" action=\"/services/{id}/mode\"><input type=\"hidden\" name=\"csrf\" value=\"{}\">\
<select name=\"mode\"><option>watch</option><option>triage</option><option>full</option></select> <button>Set</button></form></td></tr>",
            esc(&r.get::<String, _>("external_id")),
            if archived { " (archived)" } else { "" },
            esc(&mode),
            csrf_value(&st)
        ));
    }
    b.push_str("</table>");
    page("Repositories", &b).into_response()
}

#[derive(Deserialize)]
struct ModeIn {
    csrf: String,
    mode: String,
}

async fn set_mode(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Form(f): Form<ModeIn>,
) -> Response {
    if !logged_in(&st, &headers) || !ct_eq(&f.csrf, &csrf_value(&st)) {
        return (
            StatusCode::FORBIDDEN,
            page("Refused", "<p>Session or form token is not valid.</p>"),
        )
            .into_response();
    }
    if !matches!(f.mode.as_str(), "watch" | "triage" | "full") {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            page("Refused", "<p>Unknown mode.</p>"),
        )
            .into_response();
    }
    let _ = sqlx::query("UPDATE services SET mode = $2 WHERE id = $1")
        .bind(id)
        .bind(&f.mode)
        .execute(st.orch.pool())
        .await;
    let _ = sqlx::query("INSERT INTO audit_events (actor, action, detail) VALUES ('console:admin', 'service.mode', $1)")
        .bind(serde_json::json!({"service_id": id, "mode": f.mode}))
        .execute(st.orch.pool())
        .await;
    redirect("/services")
}

#[derive(Deserialize)]
struct CsrfOnly {
    csrf: String,
}

async fn pause(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Form(f): Form<CsrfOnly>,
) -> Response {
    toggle(st, headers, f, true).await
}
async fn resume(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Form(f): Form<CsrfOnly>,
) -> Response {
    toggle(st, headers, f, false).await
}
async fn toggle(st: Arc<AppState>, headers: HeaderMap, f: CsrfOnly, paused: bool) -> Response {
    if !logged_in(&st, &headers) || !ct_eq(&f.csrf, &csrf_value(&st)) {
        return (
            StatusCode::FORBIDDEN,
            page("Refused", "<p>Session or form token is not valid.</p>"),
        )
            .into_response();
    }
    let _ = st.orch.set_paused(paused).await;
    let _ = sqlx::query(
        "INSERT INTO audit_events (actor, action, detail) VALUES ('console:admin', $1, '{}')",
    )
    .bind(if paused {
        "system.paused"
    } else {
        "system.resumed"
    })
    .execute(st.orch.pool())
    .await;
    redirect("/")
}

async fn chat_page(State(st): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if !logged_in(&st, &headers) {
        return redirect("/login");
    }
    let pool = st.orch.pool();
    let groups = sqlx::query("SELECT g.id, g.channel, g.group_ref, g.daily_cap, s.external_id FROM chat_groups g JOIN services s ON s.id = g.service_id ORDER BY g.id").fetch_all(pool).await.unwrap_or_default();
    let services =
        sqlx::query("SELECT id, external_id FROM services WHERE NOT archived ORDER BY external_id")
            .fetch_all(pool)
            .await
            .unwrap_or_default();
    let mut b = String::from("<h1>Chat groups</h1><p>Anyone in a registered group can ask questions about its repository and open tickets. Only registered approvers can approve. A group that is not listed gets one reply with its id and nothing else.</p><table><tr><th>Channel</th><th>Group id</th><th>Repository</th><th>Questions per day</th><th></th></tr>");
    for g in groups {
        let id: i64 = g.get("id");
        b.push_str(&format!(
            "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td><form method=\"post\" action=\"/chat/groups/{id}/delete\"><input type=\"hidden\" name=\"csrf\" value=\"{}\"><button>Remove</button></form></td></tr>",
            esc(&g.get::<String, _>("channel")),
            esc(&g.get::<String, _>("group_ref")),
            esc(&g.get::<String, _>("external_id")),
            g.get::<i32, _>("daily_cap"),
            csrf_value(&st)
        ));
    }
    b.push_str(&format!("</table><h2>Register a group</h2><form method=\"post\" action=\"/chat/groups\"><input type=\"hidden\" name=\"csrf\" value=\"{}\"><label>Group id <input name=\"group_ref\" required></label> <label>Repository <select name=\"service_id\">", csrf_value(&st)));
    for sv in services {
        b.push_str(&format!(
            "<option value=\"{}\">{}</option>",
            sv.get::<i64, _>("id"),
            esc(&sv.get::<String, _>("external_id"))
        ));
    }
    b.push_str("</select></label> <label>Questions per day <input name=\"daily_cap\" type=\"number\" value=\"50\" min=\"1\"></label> <button>Register</button></form>");
    page("Chat groups", &b).into_response()
}

#[derive(Deserialize)]
struct GroupIn {
    csrf: String,
    group_ref: String,
    service_id: i64,
    daily_cap: i32,
}

async fn add_group(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Form(f): Form<GroupIn>,
) -> Response {
    if !logged_in(&st, &headers) || !ct_eq(&f.csrf, &csrf_value(&st)) {
        return (
            StatusCode::FORBIDDEN,
            page("Refused", "<p>Session or form token is not valid.</p>"),
        )
            .into_response();
    }
    let group_ref = f.group_ref.trim();
    if group_ref.is_empty() || group_ref.len() > 64 || f.daily_cap < 1 {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            page("Refused", "<p>Check the group id and the daily number.</p>"),
        )
            .into_response();
    }
    let r = sqlx::query("INSERT INTO chat_groups (channel, group_ref, service_id, daily_cap) VALUES ('telegram', $1, $2, $3) ON CONFLICT (channel, group_ref) DO UPDATE SET service_id = $2, daily_cap = $3")
        .bind(group_ref)
        .bind(f.service_id)
        .bind(f.daily_cap)
        .execute(st.orch.pool())
        .await;
    if r.is_err() {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            page("Refused", "<p>Unknown repository.</p>"),
        )
            .into_response();
    }
    let _ = sqlx::query("INSERT INTO audit_events (actor, action, detail) VALUES ('console:admin', 'chat.group_registered', $1)")
        .bind(serde_json::json!({"group": group_ref, "service_id": f.service_id}))
        .execute(st.orch.pool())
        .await;
    redirect("/chat")
}

async fn delete_group(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Form(f): Form<CsrfOnly>,
) -> Response {
    if !logged_in(&st, &headers) || !ct_eq(&f.csrf, &csrf_value(&st)) {
        return (
            StatusCode::FORBIDDEN,
            page("Refused", "<p>Session or form token is not valid.</p>"),
        )
            .into_response();
    }
    let _ = sqlx::query("DELETE FROM chat_groups WHERE id = $1")
        .bind(id)
        .execute(st.orch.pool())
        .await;
    redirect("/chat")
}
