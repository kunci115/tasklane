//! People who may approve, and what each role can approve.

use std::sync::Arc;

use axum::extract::{Form, Path, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::json;
use sqlx::Row;

use super::ui::{empty, head, icon, render};
use super::{csrf_ok, forbidden, guard, redirect, AppState};
use crate::esc;

pub async fn page(State(st): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let sess = match guard(&st, &headers).await {
        Ok(s) => s,
        Err(r) => return r,
    };
    let rows = sqlx::query("SELECT id, channel, ref, role FROM principals ORDER BY (channel = 'console') DESC, channel, ref").fetch_all(st.orch.pool()).await.unwrap_or_default();
    let mut b = head(
        "People",
        "Who can approve changes. Being in a chat group never gives that right.",
        "",
    );
    b.push_str(
        "<div class=\"card\" style=\"margin-bottom:1rem\"><div class=\"grid g3\">\
<div><span class=\"pill ok\">owner</span><p class=\"small muted\">Approves anything, including level 3 (secrets, payments, personal data).</p></div>\
<div><span class=\"pill warn\">team approver</span><p class=\"small muted\">Approves up to level 2 (migrations, login and permissions, deploy, CI).</p></div>\
<div><span class=\"pill\">member</span><p class=\"small muted\">Listed for reference. Cannot approve.</p></div></div></div>",
    );
    b.push_str("<div class=\"card flush\"><div class=\"scroll\"><table><thead><tr><th>Where</th><th>Identity</th><th>Role</th><th></th></tr></thead><tbody>");
    if rows.is_empty() {
        b.push_str(&format!(
            "<tr><td colspan=\"4\">{}</td></tr>",
            empty("users", "Nobody yet")
        ));
    }
    for r in rows {
        let id: i64 = r.get("id");
        let (channel, reference, role): (String, String, String) =
            (r.get("channel"), r.get("ref"), r.get("role"));
        let builtin = channel == "console" && reference == "admin";
        let pill = match role.as_str() {
            "owner" => "ok",
            "team_approver" => "warn",
            _ => "",
        };
        b.push_str(&format!(
            "<tr><td>{}</td><td>{}{}</td><td><span class=\"pill {pill}\">{}</span></td><td style=\"text-align:right\">{}</td></tr>",
            esc(&channel),
            esc(&reference),
            if builtin { " <span class=\"muted small\">(you, signed in with the admin token)</span>" } else { "" },
            esc(&role.replace('_', " ")),
            if builtin {
                String::new()
            } else {
                format!(
                    "<form method=\"post\" action=\"/people/{id}/delete\" style=\"display:inline\"><input type=\"hidden\" name=\"csrf\" value=\"{}\"><button class=\"btn small danger\" data-confirm=\"Remove this person?\" title=\"Remove\">{}</button></form>",
                    sess.csrf,
                    icon("trash")
                )
            }
        ));
    }
    b.push_str("</tbody></table></div></div>");
    b.push_str(&format!(
        "<div class=\"card\" style=\"margin-top:1rem\"><h2>Add a person</h2><form method=\"post\" action=\"/people\"><input type=\"hidden\" name=\"csrf\" value=\"{}\"><div class=\"row\">\
<div><label for=\"ch\">Where they approve</label><select id=\"ch\" name=\"channel\"><option value=\"telegram\">Telegram (buttons in a chat group)</option><option value=\"github\">GitHub</option></select></div>\
<div><label for=\"rf\">Their id there</label><input id=\"rf\" type=\"text\" name=\"reference\" required maxlength=\"64\" placeholder=\"Telegram user id, for example 123456789\"></div>\
<div><label for=\"ro\">Role</label><select id=\"ro\" name=\"role\"><option value=\"team_approver\">Team approver (up to level 2)</option><option value=\"owner\">Owner (everything)</option><option value=\"member\">Member (cannot approve)</option></select></div>\
<div class=\"row tight\"><button class=\"btn primary\">{} Add</button></div></div><p class=\"hint\">A Telegram user id is a number, not a username. A person finds theirs by writing to the bot with /start or by asking a user-id bot.</p></form></div>",
        sess.csrf,
        icon("plus")
    ));
    render(&st, &sess.csrf, "people", "People", &b)
        .await
        .into_response()
}

#[derive(Deserialize)]
pub struct AddIn {
    csrf: String,
    channel: String,
    reference: String,
    role: String,
}

pub async fn add(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Form(f): Form<AddIn>,
) -> Response {
    if !csrf_ok(&st, &headers, &f.csrf).await {
        return forbidden();
    }
    let reference = f.reference.trim();
    if !matches!(f.channel.as_str(), "telegram" | "github")
        || !matches!(f.role.as_str(), "owner" | "team_approver" | "member")
        || reference.is_empty()
        || reference.len() > 64
    {
        return (
            axum::http::StatusCode::UNPROCESSABLE_ENTITY,
            "check the fields",
        )
            .into_response();
    }
    let _ = sqlx::query("INSERT INTO principals (channel, ref, role) VALUES ($1, $2, $3) ON CONFLICT (channel, ref) DO UPDATE SET role = $3")
        .bind(&f.channel)
        .bind(reference)
        .bind(&f.role)
        .execute(st.orch.pool())
        .await;
    let _ = sqlx::query("INSERT INTO audit_events (actor, action, detail) VALUES ('console:admin', 'people.added', $1)")
        .bind(json!({"channel": f.channel, "role": f.role}))
        .execute(st.orch.pool())
        .await;
    redirect("/people")
}

#[derive(Deserialize)]
pub struct CsrfIn {
    csrf: String,
}

pub async fn delete(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Form(f): Form<CsrfIn>,
) -> Response {
    if !csrf_ok(&st, &headers, &f.csrf).await {
        return forbidden();
    }
    // the console admin cannot be removed here
    let _ = sqlx::query(
        "DELETE FROM principals WHERE id = $1 AND NOT (channel = 'console' AND ref = 'admin')",
    )
    .bind(id)
    .execute(st.orch.pool())
    .await;
    let _ = sqlx::query("INSERT INTO audit_events (actor, action, detail) VALUES ('console:admin', 'people.removed', '{}')").execute(st.orch.pool()).await;
    redirect("/people")
}
