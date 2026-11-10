//! Knowledge: what the team tells every agent, in one place. Notes for every repository, an overview
//! of each repository's notes and documents to read first, and editing of any note.

use std::sync::Arc;

use axum::extract::{Form, Path, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use sqlx::Row;

use super::ui::{empty, head, icon, render};
use super::{csrf_ok, forbidden, guard, redirect, AppState};
use crate::esc;

pub async fn page(State(st): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let sess = match guard(&st, &headers).await {
        Ok(s) => s,
        Err(r) => return r,
    };
    let global = tasklane_orchestrator::memory::list_global(st.orch.pool())
        .await
        .unwrap_or_default();
    let mut b = head(
        "Knowledge",
        "What you tell every agent. Only people write these; nothing a model or a ticket says is saved here.",
        "",
    );
    b.push_str("<div class=\"card\" style=\"margin-bottom:1rem\"><h2>For every repository</h2><p class=\"hint\">Short facts that hold everywhere, for example &ldquo;answer in Indonesian&rdquo; or &ldquo;never add a new dependency without a ticket&rdquo;. Do not put secrets here: the agents can repeat what they are told.</p>");
    if global.is_empty() {
        b.push_str(&empty("file", "No notes for every repository yet"));
    }
    for (nid, fact, who) in &global {
        b.push_str(&note_row(*nid, fact, who, &sess.csrf, "/knowledge", true));
    }
    b.push_str(&format!(
        "<form method=\"post\" action=\"/knowledge/notes\" style=\"margin-top:.8rem\"><input type=\"hidden\" name=\"csrf\" value=\"{}\"><div class=\"field\"><label for=\"gn\">New note</label><input id=\"gn\" type=\"text\" name=\"fact\" maxlength=\"1000\" required></div><button class=\"btn small primary\">{} Add note</button></form></div>",
        sess.csrf,
        icon("plus")
    ));

    b.push_str(&sources_html(&st, &sess.csrf).await);
    b.push_str(&tools_html(&st, &sess.csrf).await);
    let rows = sqlx::query(
        "SELECT s.id, s.external_id, s.agent_profile,
                (SELECT count(*) FROM memory_notes m WHERE m.service_id = s.id) AS notes
         FROM services s WHERE NOT s.archived ORDER BY s.external_id",
    )
    .fetch_all(st.orch.pool())
    .await
    .unwrap_or_default();
    b.push_str("<div class=\"card flush\"><div class=\"scroll\"><table><thead><tr><th>Repository</th><th>Notes</th><th>Documents to read first</th><th>Instructions</th><th></th></tr></thead><tbody>");
    for r in rows {
        let id: i64 = r.get("id");
        let name: String = r.get("external_id");
        let profile: serde_json::Value = r.get("agent_profile");
        let notes: i64 = r.get("notes");
        let docs = tasklane_orchestrator::memory::docs_of(&profile);
        let has_sys = profile
            .get("system")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|s| !s.is_empty());
        b.push_str(&format!(
            "<tr><td>{}</td><td>{notes}</td><td>{}</td><td>{}</td><td style=\"text-align:right\"><a class=\"btn small\" href=\"/services?q={}\">Edit</a></td></tr>",
            esc(&name),
            if docs.is_empty() {
                "<span class=\"muted small\">none</span>".to_string()
            } else {
                docs.iter()
                    .map(|d| format!("<span class=\"chip\">{}</span>", esc(d)))
                    .collect()
            },
            if has_sys { "yes" } else { "<span class=\"muted small\">none</span>" },
            super::ui::url_encode(&name),
        ));
        let _ = id;
    }
    b.push_str("</tbody></table></div></div>");
    b.push_str("<p class=\"hint\">Notes, documents and instructions for one repository are edited on its card under Repositories. What is listed here is exactly what the agents are given, together with the repository's own <code>.tasklane.yml</code>.</p>");
    render(&st, &sess.csrf, "knowledge", "Knowledge", &b)
        .await
        .into_response()
}

/// One note with an edit form (collapsed) and a remove button.
pub(super) fn note_row(
    nid: i64,
    fact: &str,
    who: &str,
    csrf: &str,
    ret: &str,
    global: bool,
) -> String {
    let delete = if global {
        format!("<form method=\"post\" action=\"/knowledge/notes/{nid}/delete\" style=\"margin-top:.3rem\"><input type=\"hidden\" name=\"csrf\" value=\"{}\"><button class=\"btn small ghost\" data-confirm=\"Remove this note?\">Remove</button></form>", esc(csrf))
    } else {
        String::new()
    };
    format!(
        "<div style=\"margin:.4rem 0\"><details><summary style=\"cursor:pointer\">{}<br><span class=\"muted small\">{}</span></summary>\
<form method=\"post\" action=\"/knowledge/notes/{nid}/edit\" class=\"row tight\" style=\"gap:.5rem;margin-top:.4rem\"><input type=\"hidden\" name=\"csrf\" value=\"{}\"><input type=\"hidden\" name=\"ret\" value=\"{}\"><input type=\"text\" name=\"fact\" value=\"{}\" maxlength=\"1000\" style=\"flex:1\" aria-label=\"Note\"><button class=\"btn small\">Save</button></form>{delete}</details></div>",
        esc(fact),
        esc(who),
        esc(csrf),
        esc(ret),
        esc(fact)
    )
}

#[derive(Deserialize)]
pub struct NoteIn {
    csrf: String,
    fact: String,
    ret: Option<String>,
}

#[derive(Deserialize)]
pub struct CsrfIn {
    csrf: String,
}

pub async fn add_global(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Form(f): Form<NoteIn>,
) -> Response {
    if !csrf_ok(&st, &headers, &f.csrf).await {
        return forbidden();
    }
    match tasklane_orchestrator::memory::add_global(st.orch.pool(), &f.fact, "console:admin").await
    {
        Ok(_) => redirect("/knowledge"),
        Err(_) => bad_note(),
    }
}

pub async fn edit(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(nid): Path<i64>,
    Form(f): Form<NoteIn>,
) -> Response {
    if !csrf_ok(&st, &headers, &f.csrf).await {
        return forbidden();
    }
    match tasklane_orchestrator::memory::update(st.orch.pool(), nid, &f.fact).await {
        Ok(()) => redirect(match f.ret.as_deref() {
            Some(r) if r.starts_with("/knowledge") => "/knowledge",
            Some(r)
                if r.len() <= 300
                    && r.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"=&%+-_.~".contains(&b)) =>
            {
                return redirect(&format!("/services?{r}"))
            }
            _ => "/services",
        }),
        Err(_) => bad_note(),
    }
}

pub async fn delete_global(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(nid): Path<i64>,
    Form(f): Form<CsrfIn>,
) -> Response {
    if !csrf_ok(&st, &headers, &f.csrf).await {
        return forbidden();
    }
    let _ = tasklane_orchestrator::memory::delete_global(st.orch.pool(), nid).await;
    redirect("/knowledge")
}

fn bad_note() -> Response {
    (
        axum::http::StatusCode::UNPROCESSABLE_ENTITY,
        "a note needs 1 to 1000 characters",
    )
        .into_response()
}

async fn sources_html(st: &AppState, csrf: &str) -> String {
    let rows = sqlx::query(
        "SELECT r.id, r.name, r.url, r.header_name, r.has_secret, r.enabled, r.last_status,
                extract(epoch FROM now() - r.last_fetched)::bigint AS age, length(r.last_text) AS size, s.external_id
         FROM reference_sources r LEFT JOIN services s ON s.id = r.service_id ORDER BY r.id",
    )
    .fetch_all(st.orch.pool())
    .await
    .unwrap_or_default();
    let repos: Vec<(i64, String)> = sqlx::query_as(
        "SELECT id, external_id FROM services WHERE NOT archived ORDER BY external_id",
    )
    .fetch_all(st.orch.pool())
    .await
    .unwrap_or_default();
    let mut h = String::from("<div class=\"card\" style=\"margin:1rem 0\"><h2>Documents the agents can read</h2><p class=\"hint\">Point the agents at a page that describes your systems: an internal wiki page, an API description, a runbook. tasklane fetches the page itself (the agent has no network and never sees the credential) and gives the text to the agent as background data, not as instructions. Only text pages are read, up to the size limit, without following redirects. Put a secret in the header, not in the address.</p>");
    if rows.is_empty() {
        h.push_str("<p class=\"muted\">None yet.</p>");
    } else {
        h.push_str("<div class=\"scroll\"><table><thead><tr><th>Name</th><th>Address</th><th>For</th><th>State</th><th></th></tr></thead><tbody>");
    }
    for r in &rows {
        let id: i64 = r.get("id");
        let enabled: bool = r.get("enabled");
        let status: String = r.get("last_status");
        let age: Option<i64> = r.get("age");
        let size: Option<i32> = r.get("size");
        let scope: Option<String> = r.get("external_id");
        let state = if !enabled {
            "<span class=\"pill\">off</span>".to_string()
        } else if status.starts_with("failed") {
            format!("<span class=\"pill bad\">{}</span>", esc(&status))
        } else if age.is_some() {
            format!("<span class=\"pill ok\">read</span> <span class=\"muted small\">{} characters, {} s ago</span>", size.unwrap_or(0), age.unwrap_or(0))
        } else {
            "<span class=\"muted small\">not read yet</span>".to_string()
        };
        let btn = |action: &str, text: &str, extra: &str| {
            format!(
                "<form method=\"post\" action=\"/knowledge/sources/{id}/{action}\" style=\"display:inline\"><input type=\"hidden\" name=\"csrf\" value=\"{}\"><button class=\"btn small {extra}\">{text}</button></form> ",
                esc(csrf)
            )
        };
        h.push_str(&format!(
            "<tr><td>{}</td><td class=\"mono small\" style=\"word-break:break-all\">{}{}</td><td>{}</td><td>{state}</td><td style=\"text-align:right;white-space:nowrap\">{}{}{}</td></tr>",
            esc(&r.get::<String, _>("name")),
            esc(&r.get::<String, _>("url")),
            if r.get::<bool, _>("has_secret") {
                format!(" <span class=\"muted small\">+ header {}</span>", esc(&r.get::<Option<String>, _>("header_name").unwrap_or_default()))
            } else {
                String::new()
            },
            scope.map(|s| esc(&s)).unwrap_or_else(|| "every repository".into()),
            btn("test", "Read now", ""),
            btn("toggle", if enabled { "Turn off" } else { "Turn on" }, ""),
            btn("delete", "Remove", "danger"),
        ));
    }
    if !rows.is_empty() {
        h.push_str("</tbody></table></div>");
    }
    let options: String = repos
        .iter()
        .map(|(id, n)| format!("<option value=\"{id}\">{}</option>", esc(n)))
        .collect();
    h.push_str(&format!(
        "<form method=\"post\" action=\"/knowledge/sources\" autocomplete=\"off\" style=\"margin-top:.8rem\"><input type=\"hidden\" name=\"csrf\" value=\"{}\"><div class=\"row\">\
<div><label for=\"rn\">Name</label><input id=\"rn\" type=\"text\" name=\"name\" required maxlength=\"40\" placeholder=\"Deploy guide\"></div>\
<div><label for=\"ru\">Address</label><input id=\"ru\" type=\"url\" name=\"url\" required maxlength=\"500\" placeholder=\"https://wiki.example.com/deploy.md\" style=\"width:22rem\"></div>\
<div><label for=\"rf\">For</label><select id=\"rf\" name=\"service_id\"><option value=\"\">Every repository</option>{options}</select></div></div>\
<div class=\"row\"><div><label for=\"rh\">Header name (optional)</label><input id=\"rh\" type=\"text\" name=\"header_name\" maxlength=\"40\" placeholder=\"Authorization\"></div>\
<div><label for=\"rv\">Header value (optional)</label><input id=\"rv\" type=\"password\" name=\"header_value\" maxlength=\"2000\" autocomplete=\"off\" style=\"width:18rem\" placeholder=\"Bearer ...\"></div>\
<div class=\"row tight\"><button class=\"btn primary\">{} Add</button></div></div></form></div>",
        esc(csrf),
        icon("plus")
    ));
    h
}

#[derive(Deserialize)]
pub struct SourceIn {
    csrf: String,
    name: String,
    url: String,
    service_id: Option<String>,
    header_name: Option<String>,
    header_value: Option<String>,
}

pub async fn add_source(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Form(f): Form<SourceIn>,
) -> Response {
    if !csrf_ok(&st, &headers, &f.csrf).await {
        return forbidden();
    }
    let unprocessable =
        |m: &'static str| (axum::http::StatusCode::UNPROCESSABLE_ENTITY, m).into_response();
    let name = f.name.trim();
    let url = f.url.trim();
    let name_ok = !name.is_empty()
        && name.len() <= 40
        && name.starts_with(|c: char| c.is_ascii_alphanumeric())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b" ._-".contains(&b));
    if !name_ok {
        return unprocessable("The name is letters, digits, spaces, dots, dashes and underscores.");
    }
    let url_ok = url.len() <= 500
        && (url.starts_with("http://") || url.starts_with("https://"))
        && !url.chars().any(|c| c.is_control() || c.is_whitespace())
        && !url.split('/').nth(2).is_some_and(|auth| auth.contains('@'));
    if !url_ok {
        return unprocessable(
            "The address must be an http(s) address without a user name or password.",
        );
    }
    let hname = f
        .header_name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let hvalue = f
        .header_value
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    if hname.is_some() != hvalue.is_some() {
        return unprocessable("Give the header name and its value, or neither.");
    }
    if let Some(n) = hname {
        let ok = n.len() <= 40
            && n.starts_with(|c: char| c.is_ascii_alphabetic())
            && n.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
        if !ok {
            return unprocessable("The header name is letters, digits and dashes.");
        }
    }
    if hvalue.is_some_and(|v| v.len() > 2000 || v.contains(['\r', '\n', '\0'])) {
        return unprocessable("The header value is one line of at most 2000 characters.");
    }
    let service: Option<i64> = match f.service_id.as_deref().filter(|s| !s.is_empty()) {
        Some(s) => match s.parse() {
            Ok(v) => Some(v),
            Err(_) => return unprocessable("Unknown repository."),
        },
        None => None,
    };
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM reference_sources")
        .fetch_one(st.orch.pool())
        .await
        .unwrap_or(0);
    if count >= 50 {
        return unprocessable("That is enough documents; remove one first.");
    }
    let id: Result<i64, _> = sqlx::query_scalar(
        "INSERT INTO reference_sources (name, url, header_name, has_secret, service_id) VALUES ($1, $2, $3, $4, $5) RETURNING id",
    )
    .bind(name)
    .bind(url)
    .bind(hname)
    .bind(hvalue.is_some())
    .bind(service)
    .fetch_one(st.orch.pool())
    .await;
    let Ok(id) = id else {
        return unprocessable("Could not save that (is the repository still there?).");
    };
    if let Some(v) = hvalue {
        if st.secrets.write(&format!("ref_{id}"), v).is_err() {
            let _ = sqlx::query("DELETE FROM reference_sources WHERE id = $1")
                .bind(id)
                .execute(st.orch.pool())
                .await;
            return (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "Could not store the header value.",
            )
                .into_response();
        }
    }
    let _ = sqlx::query("INSERT INTO audit_events (actor, action, detail) VALUES ('console:admin', 'reference.added', $1)")
        .bind(serde_json::json!({"source_id": id, "name": name, "url": url}))
        .execute(st.orch.pool())
        .await;
    redirect("/knowledge")
}

pub async fn source_action(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((id, action)): Path<(i64, String)>,
    Form(f): Form<CsrfIn>,
) -> Response {
    let sess = match guard(&st, &headers).await {
        Ok(s) => s,
        Err(r) => return r,
    };
    if !csrf_ok(&st, &headers, &f.csrf).await {
        return forbidden();
    }
    match action.as_str() {
        "delete" => {
            let _ = sqlx::query("DELETE FROM reference_sources WHERE id = $1")
                .bind(id)
                .execute(st.orch.pool())
                .await;
            st.secrets.delete(&format!("ref_{id}"));
            redirect("/knowledge")
        }
        "toggle" => {
            let _ = sqlx::query("UPDATE reference_sources SET enabled = NOT enabled WHERE id = $1")
                .bind(id)
                .execute(st.orch.pool())
                .await;
            redirect("/knowledge")
        }
        "test" => {
            let result = st.orch.fetch_reference(id).await;
            let mut b = head(
                "Document read",
                "",
                "<a class=\"btn\" href=\"/knowledge\">Back</a>",
            );
            match result {
                Ok(text) => {
                    let shown: String = text.chars().take(1500).collect();
                    b.push_str(&format!(
                        "<div class=\"card\"><p><span class=\"pill ok\">read</span> {} characters. This is what the agents get (the start of it):</p><pre class=\"prewrap\">{}</pre></div>",
                        text.chars().count(),
                        esc(&shown)
                    ));
                }
                Err(e) => b.push_str(&format!(
                    "<div class=\"card\"><p><span class=\"pill bad\">failed</span> {}</p></div>",
                    esc(&e)
                )),
            }
            render(&st, &sess.csrf, "knowledge", "Document read", &b)
                .await
                .into_response()
        }
        _ => (axum::http::StatusCode::NOT_FOUND, "unknown action").into_response(),
    }
}

async fn tools_html(st: &AppState, csrf: &str) -> String {
    let rows = sqlx::query(
        "SELECT t.id, t.name, t.description, t.base_url, t.header_name, t.has_secret, t.roles, t.enabled, t.max_calls, t.calls_total, s.external_id
         FROM agent_tools t LEFT JOIN services s ON s.id = t.service_id ORDER BY t.id",
    )
    .fetch_all(st.orch.pool())
    .await
    .unwrap_or_default();
    let repos: Vec<(i64, String)> = sqlx::query_as(
        "SELECT id, external_id FROM services WHERE NOT archived ORDER BY external_id",
    )
    .fetch_all(st.orch.pool())
    .await
    .unwrap_or_default();
    let mut h = String::from("<div class=\"card\" style=\"margin:1rem 0\"><h2>Tools the agents may use</h2><p class=\"hint\">A tool lets an agent look something up while it works, for example a database schema or an internal API. It can only <strong>read</strong> (a GET below the address you give), only the tools you list for that repository and role, with a limit on calls per run. tasklane does the reading with the credential; the agent has no network and never sees it. Answers reach the agent as data, not instructions. Give the development agent a tool only when it needs one: it writes code on the strength of issue text from other people.</p>");
    if rows.is_empty() {
        h.push_str("<p class=\"muted\">None yet.</p>");
    } else {
        h.push_str("<div class=\"scroll\"><table><thead><tr><th>Tool</th><th>Address</th><th>For</th><th>Roles</th><th>Used</th><th></th></tr></thead><tbody>");
    }
    for r in &rows {
        let id: i64 = r.get("id");
        let enabled: bool = r.get("enabled");
        let roles: Vec<String> = r.get("roles");
        let scope: Option<String> = r.get("external_id");
        let btn = |action: &str, text: &str, extra: &str| {
            format!(
                "<form method=\"post\" action=\"/knowledge/tools/{id}/{action}\" style=\"display:inline\"><input type=\"hidden\" name=\"csrf\" value=\"{}\"><button class=\"btn small {extra}\">{text}</button></form> ",
                esc(csrf)
            )
        };
        h.push_str(&format!(
            "<tr><td><code>{}</code>{}{}</td><td class=\"mono small\" style=\"word-break:break-all\">{}{}</td><td>{}</td><td>{}</td><td>{} calls</td><td style=\"text-align:right;white-space:nowrap\"><form method=\"post\" action=\"/knowledge/tools/{id}/test\" style=\"display:inline\"><input type=\"hidden\" name=\"csrf\" value=\"{}\"><input type=\"text\" name=\"path\" value=\"/\" maxlength=\"300\" style=\"width:9rem\" aria-label=\"Path to try\"><button class=\"btn small\">Try</button></form> {}{}</td></tr>",
            esc(&r.get::<String, _>("name")),
            if enabled { "" } else { " <span class=\"pill\">off</span>" },
            {
                let d: String = r.get("description");
                if d.is_empty() { String::new() } else { format!("<br><span class=\"muted small\">{}</span>", esc(&d)) }
            },
            esc(&r.get::<String, _>("base_url")),
            if r.get::<bool, _>("has_secret") {
                format!(" <span class=\"muted small\">+ header {}</span>", esc(&r.get::<Option<String>, _>("header_name").unwrap_or_default()))
            } else {
                String::new()
            },
            scope.map(|s| esc(&s)).unwrap_or_else(|| "every repository".into()),
            esc(&roles.join(", ")),
            r.get::<i64, _>("calls_total"),
            esc(csrf),
            btn("toggle", if enabled { "Turn off" } else { "Turn on" }, ""),
            btn("delete", "Remove", "danger"),
        ));
    }
    if !rows.is_empty() {
        h.push_str("</tbody></table></div>");
    }
    let options: String = repos
        .iter()
        .map(|(id, n)| format!("<option value=\"{id}\">{}</option>", esc(n)))
        .collect();
    h.push_str(&format!(
        "<form method=\"post\" action=\"/knowledge/tools\" autocomplete=\"off\" style=\"margin-top:.8rem\"><input type=\"hidden\" name=\"csrf\" value=\"{}\"><div class=\"row\">\
<div><label for=\"tn\">Tool name</label><input id=\"tn\" type=\"text\" name=\"name\" required maxlength=\"31\" pattern=\"[a-z][a-z0-9_]*\" placeholder=\"db_schema\"></div>\
<div><label for=\"tu\">Base address</label><input id=\"tu\" type=\"url\" name=\"url\" required maxlength=\"500\" placeholder=\"https://internal.example.com/api\" style=\"width:22rem\"></div>\
<div><label for=\"tf\">For</label><select id=\"tf\" name=\"service_id\"><option value=\"\">Every repository</option>{options}</select></div></div>\
<div class=\"row\"><div><label for=\"td\">What it is (the agent reads this)</label><input id=\"td\" type=\"text\" name=\"description\" maxlength=\"200\" placeholder=\"Tables and columns of the orders database\" style=\"width:26rem\"></div>\
<div><label for=\"th\">Header name (optional)</label><input id=\"th\" type=\"text\" name=\"header_name\" maxlength=\"40\" placeholder=\"Authorization\"></div>\
<div><label for=\"tv\">Header value (optional)</label><input id=\"tv\" type=\"password\" name=\"header_value\" maxlength=\"2000\" autocomplete=\"off\" style=\"width:16rem\"></div></div>\
<div class=\"row\"><fieldset style=\"border:0;padding:0\"><legend class=\"small muted\">Agents that may use it</legend>\
<label class=\"small\"><input type=\"checkbox\" name=\"role_triage\" value=\"on\" checked> triage</label> \
<label class=\"small\"><input type=\"checkbox\" name=\"role_reviewer\" value=\"on\" checked> reviewer</label> \
<label class=\"small\"><input type=\"checkbox\" name=\"role_dev\" value=\"on\"> developer</label></fieldset>\
<div><label for=\"tm\">Calls per run</label><input id=\"tm\" type=\"number\" name=\"max_calls\" min=\"1\" max=\"100\" value=\"20\" style=\"width:5rem\"></div>\
<div class=\"row tight\"><button class=\"btn primary\">{} Add</button></div></div></form></div>",
        esc(csrf),
        icon("plus")
    ));
    h
}

#[derive(Deserialize)]
pub struct ToolIn {
    csrf: String,
    name: String,
    url: String,
    description: Option<String>,
    service_id: Option<String>,
    header_name: Option<String>,
    header_value: Option<String>,
    role_triage: Option<String>,
    role_reviewer: Option<String>,
    role_dev: Option<String>,
    max_calls: Option<i32>,
}

pub async fn add_tool(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Form(f): Form<ToolIn>,
) -> Response {
    if !csrf_ok(&st, &headers, &f.csrf).await {
        return forbidden();
    }
    let no = |m: &'static str| (axum::http::StatusCode::UNPROCESSABLE_ENTITY, m).into_response();
    let name = f.name.trim();
    if !(!name.is_empty()
        && name.len() <= 31
        && name.starts_with(|c: char| c.is_ascii_lowercase())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'))
    {
        return no(
            "The tool name is lower-case letters, digits and underscores, starting with a letter.",
        );
    }
    let url = f.url.trim();
    let url_ok = url.len() <= 500
        && (url.starts_with("http://") || url.starts_with("https://"))
        && !url.chars().any(|c| c.is_control() || c.is_whitespace())
        && !url
            .split('/')
            .nth(2)
            .is_some_and(|a| a.is_empty() || a.contains('@'));
    if !url_ok {
        return no("The address must be an http(s) address without a user name or password.");
    }
    let description: String = f
        .description
        .as_deref()
        .unwrap_or("")
        .trim()
        .chars()
        .take(200)
        .collect();
    if description.chars().any(|c| c.is_control()) {
        return no("The description is one line.");
    }
    let hname = f
        .header_name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let hvalue = f
        .header_value
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    if hname.is_some() != hvalue.is_some() {
        return no("Give the header name and its value, or neither.");
    }
    if hname.is_some_and(|n| {
        !(n.len() <= 40
            && n.starts_with(|c: char| c.is_ascii_alphabetic())
            && n.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'))
    }) {
        return no("The header name is letters, digits and dashes.");
    }
    if hvalue.is_some_and(|v| v.len() > 2000 || v.contains(['\r', '\n', '\0'])) {
        return no("The header value is one line of at most 2000 characters.");
    }
    let mut roles: Vec<&str> = vec![];
    if f.role_triage.is_some() {
        roles.push("triage");
    }
    if f.role_reviewer.is_some() {
        roles.push("reviewer");
    }
    if f.role_dev.is_some() {
        roles.push("dev");
    }
    if roles.is_empty() {
        return no("Choose at least one agent that may use it.");
    }
    let service: Option<i64> = match f.service_id.as_deref().filter(|s| !s.is_empty()) {
        Some(s) => match s.parse() {
            Ok(v) => Some(v),
            Err(_) => return no("Unknown repository."),
        },
        None => None,
    };
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM agent_tools")
        .fetch_one(st.orch.pool())
        .await
        .unwrap_or(0);
    if count >= 50 {
        return no("That is enough tools; remove one first.");
    }
    let id: Result<i64, _> = sqlx::query_scalar(
        "INSERT INTO agent_tools (name, description, base_url, header_name, has_secret, service_id, roles, max_calls)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING id",
    )
    .bind(name)
    .bind(&description)
    .bind(url)
    .bind(hname)
    .bind(hvalue.is_some())
    .bind(service)
    .bind(&roles)
    .bind(f.max_calls.unwrap_or(20).clamp(1, 100))
    .fetch_one(st.orch.pool())
    .await;
    let Ok(id) = id else {
        return no(
            "A tool with that name already exists for that repository (or the repository is gone).",
        );
    };
    if let Some(v) = hvalue {
        if st.secrets.write(&format!("tool_{id}"), v).is_err() {
            let _ = sqlx::query("DELETE FROM agent_tools WHERE id = $1")
                .bind(id)
                .execute(st.orch.pool())
                .await;
            return (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "Could not store the header value.",
            )
                .into_response();
        }
    }
    let _ = sqlx::query("INSERT INTO audit_events (actor, action, detail) VALUES ('console:admin', 'tool.added', $1)")
        .bind(serde_json::json!({"tool_id": id, "name": name, "url": url, "roles": roles}))
        .execute(st.orch.pool())
        .await;
    redirect("/knowledge")
}

#[derive(Deserialize)]
pub struct ToolActionIn {
    csrf: String,
    path: Option<String>,
}

pub async fn tool_action(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((id, action)): Path<(i64, String)>,
    Form(f): Form<ToolActionIn>,
) -> Response {
    let sess = match guard(&st, &headers).await {
        Ok(s) => s,
        Err(r) => return r,
    };
    if !csrf_ok(&st, &headers, &f.csrf).await {
        return forbidden();
    }
    match action.as_str() {
        "delete" => {
            let _ = sqlx::query("DELETE FROM agent_tools WHERE id = $1")
                .bind(id)
                .execute(st.orch.pool())
                .await;
            st.secrets.delete(&format!("tool_{id}"));
            redirect("/knowledge")
        }
        "toggle" => {
            let _ = sqlx::query("UPDATE agent_tools SET enabled = NOT enabled WHERE id = $1")
                .bind(id)
                .execute(st.orch.pool())
                .await;
            redirect("/knowledge")
        }
        "test" => {
            let path = f.path.as_deref().unwrap_or("/").trim().to_string();
            let result = st.orch.test_tool(id, &path).await;
            let mut b = head(
                "Tool tried",
                "",
                "<a class=\"btn\" href=\"/knowledge\">Back</a>",
            );
            match result {
                Ok(text) => {
                    let shown: String = text.chars().take(1500).collect();
                    b.push_str(&format!(
                        "<div class=\"card\"><p><span class=\"pill ok\">answered</span> The agent would get this for <code>{}</code> (the start of it):</p><pre class=\"prewrap\">{}</pre></div>",
                        esc(&path),
                        esc(&shown)
                    ));
                }
                Err(e) => b.push_str(&format!(
                    "<div class=\"card\"><p><span class=\"pill bad\">failed</span> {}</p></div>",
                    esc(&e)
                )),
            }
            render(&st, &sess.csrf, "knowledge", "Tool tried", &b)
                .await
                .into_response()
        }
        _ => (axum::http::StatusCode::NOT_FOUND, "unknown action").into_response(),
    }
}
