//! My account: who I am signed in as, and my own model account. Each person can have an account of
//! their own; work done for them uses it, so accounts do not get mixed.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Form, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{json, Value};

use super::ui::{head, icon, render};
use super::{csrf_ok, forbidden, guard, redirect, AppState};
use crate::esc;

const CALL: Duration = Duration::from_secs(10);

pub async fn page(State(st): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let sess = match guard(&st, &headers).await {
        Ok(s) => s,
        Err(r) => return r,
    };
    let me = format!("{}:{}", sess.channel, sess.reference);
    let mut b = head(
        "My account",
        "Who you are signed in as, and the model account that does your work.",
        "",
    );
    b.push_str(&format!(
        "<div class=\"card\"><p>Signed in as <strong>{}</strong> <span class=\"pill\">{}</span></p>{}</div>",
        esc(&me),
        esc(&sess.role.replace('_', " ")),
        if sess.channel == "console" {
            "<p class=\"hint\">You used the admin token, which acts for the whole installation and has no model account of its own: its work uses the shared accounts under Model accounts. People get their own sign-in under People.</p>"
        } else {
            ""
        }
    ));
    if sess.channel == "console" {
        b.push_str(&format!(
            "<div class=\"card\" style=\"margin-top:1rem\"><h2>Admin token</h2><form method=\"post\" action=\"/me/admin-token\" autocomplete=\"off\"><input type=\"hidden\" name=\"csrf\" value=\"{}\"><div class=\"row\">\
<div><label for=\"nt\">New token</label><input id=\"nt\" type=\"password\" name=\"token\" required minlength=\"24\" maxlength=\"200\" autocomplete=\"new-password\" style=\"width:20rem\"></div>\
<div><label for=\"nt2\">Again</label><input id=\"nt2\" type=\"password\" name=\"again\" required minlength=\"24\" maxlength=\"200\" autocomplete=\"new-password\" style=\"width:20rem\"></div>\
<div class=\"row tight\"><button class=\"btn primary\" data-confirm=\"Change the admin token? Other admin sessions end.\">Change</button></div></div>\
<p class=\"hint\">At least 24 characters. After the change only the new token signs in as admin (the one in the server's environment stops working). Only a hash is kept. If you lose it, delete the row <code>admin_token_hash</code> from <code>sync_state</code> in the database and the environment token works again.</p></form></div>",
            sess.csrf
        ));
    }
    if sess.channel != "console" {
        let rows = st
            .orch
            .engine_call_with("accounts", json!({"owner": me}), CALL)
            .await
            .ok()
            .and_then(|v| v.get("rows").and_then(Value::as_array).cloned())
            .unwrap_or_default();
        b.push_str("<div class=\"card\" style=\"margin-top:1rem\"><h2>Your model account</h2>");
        if rows.is_empty() {
            b.push_str("<p class=\"muted\">You have none yet. Work for you uses the shared accounts until you add one.</p>");
        }
        for r in &rows {
            let label = r.get("label").and_then(Value::as_str).unwrap_or("");
            let sleep = r.get("cooldown_secs").and_then(Value::as_u64).unwrap_or(0);
            b.push_str(&format!(
                "<form method=\"post\" action=\"/me/account/remove\" class=\"row tight\" style=\"align-items:center;gap:.6rem;margin:.3rem 0\"><input type=\"hidden\" name=\"csrf\" value=\"{}\"><input type=\"hidden\" name=\"label\" value=\"{}\"><span>{} <code>{}</code></span>{}<button class=\"btn small danger\" data-confirm=\"Remove your account?\">{} Remove</button></form>",
                sess.csrf,
                esc(label),
                esc(label),
                esc(r.get("token").and_then(Value::as_str).unwrap_or("")),
                if sleep > 0 { "<span class=\"pill warn\">used up, waking later</span>" } else { "<span class=\"pill ok\">ready</span>" },
                icon("trash")
            ));
        }
        b.push_str(&format!(
            "<form method=\"post\" action=\"/me/account\" autocomplete=\"off\" style=\"margin-top:.8rem\"><input type=\"hidden\" name=\"csrf\" value=\"{}\"><div class=\"row\">\
<div><label for=\"ak\">Kind</label><select id=\"ak\" name=\"kind\"><option value=\"oauth\">Claude subscription token</option><option value=\"api_key\">Anthropic API key</option></select></div>\
<div><label for=\"at\">Token</label><input id=\"at\" type=\"password\" name=\"token\" required maxlength=\"400\" autocomplete=\"off\" style=\"width:22rem\"></div>\
<div class=\"row tight\"><button class=\"btn primary\">{} Add</button></div></div>\
<p class=\"hint\">Run <code>claude setup-token</code> on a computer where your own account is signed in and paste the token. It is stored where only tasklane can read it and never shown again. Work that you approve (or that is opened for you) runs on your account first, then on the shared ones if yours is used up. Nobody else's work ever uses yours.</p></form></div>",
            sess.csrf,
            icon("plus")
        ));
    }
    render(&st, &sess.csrf, "me", "My account", &b)
        .await
        .into_response()
}

#[derive(Deserialize)]
pub struct AddIn {
    csrf: String,
    kind: String,
    token: String,
}

fn label_for(reference: &str, existing: &[String]) -> String {
    let base: String = reference
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .take(30)
        .collect();
    let mut label = base.clone();
    let mut n = 2;
    while existing.contains(&label) {
        label = format!("{base}-{n}");
        n += 1;
    }
    label
}

pub async fn add_account(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Form(f): Form<AddIn>,
) -> Response {
    let sess = match guard(&st, &headers).await {
        Ok(s) => s,
        Err(r) => return r,
    };
    if !csrf_ok(&st, &headers, &f.csrf).await {
        return forbidden();
    }
    if sess.channel == "console" {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            "The admin token has no account of its own; add shared accounts under Model accounts.",
        )
            .into_response();
    }
    let me = format!("{}:{}", sess.channel, sess.reference);
    let all: Vec<String> = st
        .orch
        .engine_call_with("accounts", json!({}), CALL)
        .await
        .ok()
        .and_then(|v| v.get("rows").and_then(Value::as_array).cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|r| r.get("label").and_then(Value::as_str).map(str::to_string))
        .collect();
    let label = label_for(&sess.reference, &all);
    let r = st
        .orch
        .engine_call_with(
            "account",
            json!({"action": "add", "label": label, "kind": f.kind, "token": f.token.trim(), "owner": me, "scope": me}),
            CALL,
        )
        .await;
    match r {
        Ok(_) => {
            let _ = sqlx::query("INSERT INTO audit_events (actor, action, detail) VALUES ($1, 'accounts.added', $2)")
                .bind(&me)
                .bind(json!({"account": label, "owner": me}))
                .execute(st.orch.pool())
                .await;
            redirect("/me")
        }
        Err(e) => {
            let text = e.to_string();
            let text = text.split_once("): ").map(|(_, m)| m).unwrap_or(&text);
            (StatusCode::UNPROCESSABLE_ENTITY, text.to_string()).into_response()
        }
    }
}

#[derive(Deserialize)]
pub struct RemoveIn {
    csrf: String,
    label: String,
}

pub async fn remove_account(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Form(f): Form<RemoveIn>,
) -> Response {
    let sess = match guard(&st, &headers).await {
        Ok(s) => s,
        Err(r) => return r,
    };
    if !csrf_ok(&st, &headers, &f.csrf).await {
        return forbidden();
    }
    let me = format!("{}:{}", sess.channel, sess.reference);
    let r = st
        .orch
        .engine_call_with(
            "account",
            json!({"action": "remove", "label": f.label, "scope": me}),
            CALL,
        )
        .await;
    match r {
        Ok(_) => redirect("/me"),
        Err(_) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            "That is not one of your accounts.",
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
pub struct TokenIn {
    csrf: String,
    token: String,
    again: String,
}

/// Change the admin token. Only the admin session may; every other admin session ends.
pub async fn change_admin_token(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Form(f): Form<TokenIn>,
) -> Response {
    let sess = match guard(&st, &headers).await {
        Ok(s) => s,
        Err(r) => return r,
    };
    if !csrf_ok(&st, &headers, &f.csrf).await {
        return forbidden();
    }
    if sess.channel != "console" {
        return forbidden();
    }
    if f.token != f.again {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            "The two tokens are not the same.",
        )
            .into_response();
    }
    if f.token.chars().count() < 24 || f.token.chars().count() > 200 {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            "The token needs at least 24 characters (and at most 200).",
        )
            .into_response();
    }
    let hash = tasklane_orchestrator::sha256_hex(&f.token);
    let _ = sqlx::query("INSERT INTO sync_state (key, value) VALUES ('admin_token_hash', $1) ON CONFLICT (key) DO UPDATE SET value = $1, updated_at = now()")
        .bind(&hash)
        .execute(st.orch.pool())
        .await;
    let _ = sqlx::query("DELETE FROM console_sessions WHERE principal_id IS NULL AND csrf <> $1")
        .bind(&sess.csrf)
        .execute(st.orch.pool())
        .await;
    let _ = sqlx::query("INSERT INTO audit_events (actor, action, detail) VALUES ('console:admin', 'admin.token_changed', '{}')")
        .execute(st.orch.pool())
        .await;
    redirect("/me")
}
