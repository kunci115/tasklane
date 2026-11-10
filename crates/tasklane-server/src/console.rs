//! The admin console: server-rendered pages with a little progressive JavaScript.
//! Everything that comes from a ticket is escaped. Every state-changing form carries a
//! per-session CSRF token, and the session cookie is HttpOnly and SameSite=Lax (Lax, not Strict, so the browser still sends it when GitHub redirects back; every state-changing request is a POST that also needs the CSRF token).
//! The pages work without JavaScript; the script only adds live updates and comfort.

mod accounts;
mod connect;
mod dashboard;
mod knowledge;
mod map;
mod people;
mod projects;
mod pulls;
mod repos;
mod system;
mod tickets;
mod ui;

use std::sync::Arc;

use axum::extract::{Form, Path, State};
use axum::http::header::{COOKIE, LOCATION, SET_COOKIE};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use serde::Deserialize;
use sqlx::Row;

use crate::auth::ct_eq;
use crate::{esc, AppState};
use ui::{empty, head, icon, render, MARK_SVG};

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/", get(dashboard::home))
        .route("/fragments/overview", get(dashboard::overview_fragment))
        .route("/login", get(login_form).post(login))
        .route("/logout", post(logout))
        .route("/tickets", get(tickets::list))
        .route("/pulls", get(pulls::page))
        .route("/projects/new", get(projects::page).post(projects::create))
        .route("/fragments/pulls", get(pulls::fragment))
        .route("/pulls/review", post(pulls::review))
        .route("/pulls/merge", post(pulls::merge))
        .route("/pulls/switch", post(pulls::switch))
        .route("/tickets/{id}", get(tickets::detail))
        .route("/fragments/ticket/{id}", get(tickets::live_fragment))
        .route("/tickets/{id}/decide", post(tickets::decide_form))
        .route("/tickets/{id}/retry", post(tickets::retry_form))
        .route("/tickets/{id}/duplicate", post(tickets::duplicate_form))
        .route("/services", get(repos::page))
        .route("/services/{id}/mode", post(repos::set_mode))
        .route("/services/{id}/profile", post(repos::set_profile))
        .route("/services/{id}/notes", post(repos::add_note))
        .route(
            "/services/{id}/notes/{nid}/delete",
            post(repos::delete_note),
        )
        .route("/services/{id}/takeover", post(repos::take_over))
        .route("/services/{id}/import", post(repos::import_open))
        .route("/knowledge", get(knowledge::page))
        .route("/knowledge/notes", post(knowledge::add_global))
        .route("/knowledge/notes/{nid}/edit", post(knowledge::edit))
        .route(
            "/knowledge/notes/{nid}/delete",
            post(knowledge::delete_global),
        )
        .route("/people", get(people::page).post(people::add))
        .route("/people/{id}/delete", post(people::delete))
        .route("/people/{id}/credit", post(people::set_credit))
        .route("/accounts", get(accounts::page).post(accounts::add))
        .route("/accounts/action", post(accounts::action))
        .route("/chat", get(chat_page))
        .route("/chat/groups", post(add_group))
        .route("/chat/groups/{id}/delete", post(delete_group))
        .route("/connections", get(connect::page))
        .route("/connect/github", get(connect::github_form))
        .route("/connect/github/start", post(connect::github_start))
        .route("/connect/github/callback", get(connect::github_callback))
        .route("/connect/github/install", get(connect::github_install))
        .route("/connect/github/installed", get(connect::github_installed))
        .route(
            "/connect/github/disconnect",
            post(connect::github_disconnect),
        )
        .route(
            "/connect/agent",
            get(connect::agent_form).post(connect::agent_save),
        )
        .route("/connect/agent/test", post(connect::agent_test))
        .route("/connect/agent/disconnect", post(connect::agent_disconnect))
        .route("/map", get(map::page))
        .route("/map/data", get(map::snapshot))
        .route("/map/replay", get(map::replay))
        .route("/activity", get(system::activity))
        .route("/health", get(system::health))
        .route("/pause", post(pause))
        .route("/resume", post(resume))
        .route("/static/app.css", get(|| async { ui::asset_css() }))
        .route(
            "/static/fonts/{name}",
            get(|Path(name): Path<String>| async move { ui::asset_font(&name) }),
        )
        .route("/static/app.js", get(|| async { ui::asset_js() }))
        .route("/static/map.js", get(|| async { ui::asset_map_js() }))
        .route("/static/icon.svg", get(|| async { ui::asset_icon() }))
        .route(
            "/manifest.webmanifest",
            get(|| async { ui::asset_manifest() }),
        )
}

/// A signed-in console session, stored server-side with its own CSRF token.
struct Session {
    csrf: String,
}

fn cookie_value(headers: &HeaderMap) -> Option<String> {
    headers
        .get(COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|c| {
            c.split(';')
                .filter_map(|p| p.trim().strip_prefix("tl="))
                .next()
                .map(str::to_string)
        })
}

async fn session(st: &AppState, headers: &HeaderMap) -> Option<Session> {
    let id = cookie_value(headers)?;
    if id.len() != 64 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let csrf: Option<String> = sqlx::query_scalar(
        "SELECT csrf FROM console_sessions WHERE id = $1 AND expires_at > now()",
    )
    .bind(&id)
    .fetch_optional(st.orch.pool())
    .await
    .ok()
    .flatten();
    csrf.map(|csrf| Session { csrf })
}

/// The session, or a redirect to the sign-in page.
#[allow(clippy::result_large_err)]
async fn guard(st: &AppState, headers: &HeaderMap) -> Result<Session, Response> {
    session(st, headers).await.ok_or_else(|| redirect("/login"))
}

async fn csrf_ok(st: &AppState, headers: &HeaderMap, token: &str) -> bool {
    session(st, headers)
        .await
        .is_some_and(|s| ct_eq(token, &s.csrf))
}

fn forbidden() -> Response {
    (StatusCode::FORBIDDEN, Html("<!doctype html><meta charset=\"utf-8\"><link rel=\"stylesheet\" href=\"/static/app.css\"><div class=\"login\"><div class=\"card\"><h1>Not allowed</h1><p class=\"muted\">Your session or form token is not valid. Sign in again and retry.</p><a class=\"btn primary\" href=\"/login\">Sign in</a></div></div>")).into_response()
}

/// 32 random bytes as hex, from the operating system.
pub(super) fn random_hex() -> String {
    let mut buf = [0u8; 32];
    getrandom::fill(&mut buf).expect("operating system randomness");
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

fn redirect(to: &str) -> Response {
    (StatusCode::SEE_OTHER, [(LOCATION, to.to_string())]).into_response()
}

fn login_page(message: &str) -> Html<String> {
    Html(format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>Sign in - tasklane</title>\
<meta name=\"theme-color\" content=\"#03040a\"><link rel=\"icon\" href=\"/static/icon.svg\"><link rel=\"manifest\" href=\"/manifest.webmanifest\"><link rel=\"stylesheet\" href=\"/static/app.css\"></head>\
<body><div class=\"login\"><div class=\"card\"><div class=\"logo\">{MARK_SVG}</div>\
<h1>tasklane</h1><p class=\"muted\">Issues in, reviewed changes out. Sign in to manage it.</p>{message}\
<form method=\"post\" action=\"/login\"><div class=\"field\"><label for=\"token\">Admin token</label><input id=\"token\" type=\"password\" name=\"token\" autocomplete=\"current-password\" autofocus required></div>\
<button class=\"btn primary\" style=\"width:100%\">Sign in</button><p class=\"hint\">The token is the value of TASKLANE_ADMIN_TOKEN on the server.</p></form></div></div><script src=\"/static/app.js\" defer></script></body></html>"
    ))
}

async fn login_form() -> Html<String> {
    login_page("")
}

#[derive(Deserialize)]
struct LoginIn {
    token: String,
}

/// Wrong-token counters: one per client address (five wrong tokens shut that client out for five
/// minutes) and one for everybody (a ceiling against guessing from many addresses).
#[derive(Default)]
pub struct Throttle {
    per_client: std::collections::HashMap<String, (u32, std::time::Instant)>,
    all: (u32, Option<std::time::Instant>),
}

const LOGIN_WINDOW: std::time::Duration = std::time::Duration::from_secs(300);

impl Throttle {
    fn shut(&mut self, client: &str) -> bool {
        self.per_client.retain(|_, v| v.1.elapsed() < LOGIN_WINDOW);
        if self.all.1.is_some_and(|t| t.elapsed() > LOGIN_WINDOW) {
            self.all = (0, None);
        }
        self.all.0 >= 50 || self.per_client.get(client).is_some_and(|v| v.0 >= 5)
    }
    fn wrong(&mut self, client: &str) {
        let e = self
            .per_client
            .entry(client.to_string())
            .or_insert((0, std::time::Instant::now()));
        e.0 += 1;
        self.all.0 += 1;
        self.all.1.get_or_insert_with(std::time::Instant::now);
    }
    fn right(&mut self, client: &str) {
        self.per_client.remove(client);
    }
}

/// Who is asking: the connecting address; behind a proxy on the same machine or network (a private
/// address) the first address the proxy reports.
fn client_key(peer: Option<std::net::SocketAddr>, headers: &HeaderMap) -> String {
    let Some(peer) = peer else {
        return "unknown".into();
    };
    let ip = peer.ip();
    let private = match ip {
        std::net::IpAddr::V4(a) => a.is_loopback() || a.is_private() || a.is_link_local(),
        std::net::IpAddr::V6(a) => a.is_loopback() || (a.segments()[0] & 0xfe00) == 0xfc00,
    };
    if private {
        if let Some(f) = headers
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(',').next())
            .map(str::trim)
            .filter(|v| v.parse::<std::net::IpAddr>().is_ok())
        {
            return f.to_string();
        }
    }
    ip.to_string()
}

async fn login(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    peer: Option<axum::Extension<axum::extract::ConnectInfo<std::net::SocketAddr>>>,
    Form(f): Form<LoginIn>,
) -> Response {
    let client = client_key(peer.map(|c| (c.0).0), &headers);
    // After five wrong tokens from one address the door stays shut for it for five minutes.
    if st
        .login_throttle
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .shut(&client)
    {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            login_page(
                "<p class=\"pill bad\">Too many wrong tokens. Try again in a few minutes.</p>",
            ),
        )
            .into_response();
    }
    if !ct_eq(&f.token, &st.admin_token) {
        st.login_throttle
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .wrong(&client);
        return (
            StatusCode::UNAUTHORIZED,
            login_page("<p class=\"pill bad\">That token is not right.</p>"),
        )
            .into_response();
    }
    st.login_throttle
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .right(&client);
    let (id, csrf) = (random_hex(), random_hex());
    if sqlx::query("INSERT INTO console_sessions (id, csrf, expires_at) VALUES ($1, $2, now() + interval '8 hours')")
        .bind(&id)
        .bind(&csrf)
        .execute(st.orch.pool())
        .await
        .is_err()
    {
        return (StatusCode::INTERNAL_SERVER_ERROR, login_page("<p class=\"pill bad\">Could not start a session.</p>")).into_response();
    }
    let secure = if st.secure_cookies { "; Secure" } else { "" };
    (
        StatusCode::SEE_OTHER,
        [
            (
                SET_COOKIE,
                format!("tl={id}; HttpOnly; SameSite=Lax; Path=/; Max-Age=28800{secure}"),
            ),
            (LOCATION, "/".to_string()),
        ],
    )
        .into_response()
}

#[derive(Deserialize)]
struct CsrfOnly {
    csrf: String,
}

async fn logout(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Form(f): Form<CsrfOnly>,
) -> Response {
    let Some(sess) = session(&st, &headers).await else {
        return redirect("/login");
    };
    if !ct_eq(&f.csrf, &sess.csrf) {
        return forbidden();
    }
    if let Some(id) = cookie_value(&headers) {
        let _ = sqlx::query("DELETE FROM console_sessions WHERE id = $1")
            .bind(id)
            .execute(st.orch.pool())
            .await;
    }
    (
        StatusCode::SEE_OTHER,
        [
            (
                SET_COOKIE,
                "tl=; HttpOnly; SameSite=Lax; Path=/; Max-Age=0".to_string(),
            ),
            (LOCATION, "/login".to_string()),
        ],
    )
        .into_response()
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
    if !csrf_ok(&st, &headers, &f.csrf).await {
        return forbidden();
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
    let sess = match guard(&st, &headers).await {
        Ok(s) => s,
        Err(r) => return r,
    };
    let pool = st.orch.pool();
    let groups = sqlx::query("SELECT g.id, g.channel, g.group_ref, g.daily_cap, s.external_id FROM chat_groups g JOIN services s ON s.id = g.service_id ORDER BY g.id").fetch_all(pool).await.unwrap_or_default();
    let services =
        sqlx::query("SELECT id, external_id FROM services WHERE NOT archived ORDER BY external_id")
            .fetch_all(pool)
            .await
            .unwrap_or_default();
    let mut b = head(
        "Chat groups",
        "Let people ask questions and open tickets from a chat, with approvals as buttons.",
        "",
    );
    b.push_str(
        "<div class=\"card\" style=\"margin-bottom:1rem\"><div class=\"grid g3\">\
<div><strong>1. Add the bot</strong><p class=\"small muted\">Add your bot to a group. It replies once with the group id, then leaves.</p></div>\
<div><strong>2. Register the group</strong><p class=\"small muted\">Paste that id below and pick the repository it is about. Add the bot again.</p></div>\
<div><strong>3. Use it</strong><p class=\"small muted\">Anyone in the group can use /ask and /task. Only approvers on the People page can press approve.</p></div></div></div>",
    );
    b.push_str("<div class=\"card flush\"><div class=\"scroll\"><table><thead><tr><th>Channel</th><th>Group id</th><th>Repository</th><th>Questions per day</th><th></th></tr></thead><tbody>");
    if groups.is_empty() {
        b.push_str(&format!(
            "<tr><td colspan=\"5\">{}</td></tr>",
            empty("chat", "No groups registered yet")
        ));
    }
    for g in groups {
        let id: i64 = g.get("id");
        b.push_str(&format!(
            "<tr><td>{}</td><td class=\"mono\">{}</td><td>{}</td><td>{}</td><td style=\"text-align:right\"><form method=\"post\" action=\"/chat/groups/{id}/delete\" style=\"display:inline\"><input type=\"hidden\" name=\"csrf\" value=\"{}\"><button class=\"btn small danger\" data-confirm=\"Remove this group?\" title=\"Remove\">{}</button></form></td></tr>",
            esc(&g.get::<String, _>("channel")),
            esc(&g.get::<String, _>("group_ref")),
            esc(&g.get::<String, _>("external_id")),
            g.get::<i32, _>("daily_cap"),
            sess.csrf,
            icon("trash")
        ));
    }
    b.push_str("</tbody></table></div></div>");
    b.push_str(&format!("<div class=\"card\" style=\"margin-top:1rem\"><h2>Register a group</h2><form method=\"post\" action=\"/chat/groups\"><input type=\"hidden\" name=\"csrf\" value=\"{}\"><div class=\"row\"><div><label for=\"gr\">Group id</label><input id=\"gr\" name=\"group_ref\" type=\"text\" required placeholder=\"-1001234567890\"></div><div><label for=\"sv\">Repository</label><select id=\"sv\" name=\"service_id\">", sess.csrf));
    for sv in services {
        b.push_str(&format!(
            "<option value=\"{}\">{}</option>",
            sv.get::<i64, _>("id"),
            esc(&sv.get::<String, _>("external_id"))
        ));
    }
    b.push_str(&format!("</select></div><div><label for=\"dc\">Questions per day</label><input id=\"dc\" name=\"daily_cap\" type=\"number\" value=\"50\" min=\"1\"></div><div class=\"row tight\"><button class=\"btn primary\">{} Register</button></div></div><p class=\"hint\">Anyone in a registered group can read what /ask can read from that repository. Keep groups private.</p></form></div>", icon("plus")));
    render(&st, &sess.csrf, "chat", "Chat groups", &b)
        .await
        .into_response()
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
    if !csrf_ok(&st, &headers, &f.csrf).await {
        return forbidden();
    }
    let group_ref = f.group_ref.trim();
    if group_ref.is_empty() || group_ref.len() > 64 || f.daily_cap < 1 {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            "Check the group id and the daily number.",
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
        return (StatusCode::UNPROCESSABLE_ENTITY, "Unknown repository.").into_response();
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
    if !csrf_ok(&st, &headers, &f.csrf).await {
        return forbidden();
    }
    let _ = sqlx::query("DELETE FROM chat_groups WHERE id = $1")
        .bind(id)
        .execute(st.orch.pool())
        .await;
    redirect("/chat")
}
