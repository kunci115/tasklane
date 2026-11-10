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
