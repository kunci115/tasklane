//! The model accounts the agent uses, in order. When one runs out of usage the next takes over and
//! carries on with the same ticket (the list itself lives in the engine plug-in; see its README).

use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Form, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{json, Value};

use super::ui::{empty, head, icon, render};
use super::{csrf_ok, forbidden, guard, redirect, AppState};
use crate::esc;

const CALL: Duration = Duration::from_secs(10);

fn waiting(secs: u64) -> String {
    match secs {
        0 => "ready".into(),
        s if s < 90 => format!("sleeping {s} s"),
        s if s < 5400 => format!("sleeping {} min", s.div_ceil(60)),
        s => format!("sleeping {} h", s.div_ceil(3600)),
    }
}

pub async fn page(State(st): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let sess = match guard(&st, &headers).await {
        Ok(s) => s,
        Err(r) => return r,
    };
    let listing = st.orch.engine_call_with("accounts", json!({}), CALL).await;
    let mut b = head(
        "Model accounts",
        "The accounts the agent works with, first to last. When one runs out of usage, the next takes over the same ticket and carries on.",
        "",
    );
    match listing {
        Err(e) => b.push_str(&format!(
            "<div class=\"card\"><p><span class=\"pill bad\">unavailable</span> The agent plug-in did not answer ({}). This page works with the Claude engine plug-in.</p></div>",
            esc(&e.to_string())
        )),
        Ok(v) if v.get("configured").and_then(Value::as_bool) != Some(true) => b.push_str(
            "<div class=\"card\"><p>The list of accounts is switched off. Set <code>TASKLANE_CLAUDE_POOL_FILE</code> for the agent plug-in, or use the default data directory of the Docker image.</p></div>",
        ),
        Ok(v) => {
            let rows = v.get("rows").and_then(Value::as_array).cloned().unwrap_or_default();
            b.push_str("<div class=\"card flush\"><div class=\"scroll\"><table><thead><tr><th>#</th><th>Account</th><th>Kind</th><th>Token</th><th>State</th><th>Last problem</th><th></th></tr></thead><tbody>");
            if rows.is_empty() {
                b.push_str(&format!(
                    "<tr><td colspan=\"7\">{}</td></tr>",
                    empty("shield", "No account yet. Connect one on the Connections page, or add one below.")
                ));
            }
            for (i, r) in rows.iter().enumerate() {
                let label = r.get("label").and_then(Value::as_str).unwrap_or("");
                let kind = r.get("kind").and_then(Value::as_str).unwrap_or("");
                let token = r.get("token").and_then(Value::as_str).unwrap_or("");
                let enabled = r.get("enabled").and_then(Value::as_bool).unwrap_or(true);
                let sleep = r.get("cooldown_secs").and_then(Value::as_u64).unwrap_or(0);
                let problem = r.get("last_error").and_then(Value::as_str).unwrap_or("");
                let removable = r.get("removable").and_then(Value::as_bool).unwrap_or(false);
                let pill = if !enabled {
                    "<span class=\"pill\">disabled</span>".to_string()
                } else if sleep > 0 {
                    format!("<span class=\"pill warn\">{}</span>", waiting(sleep))
                } else {
                    "<span class=\"pill ok\">ready</span>".to_string()
                };
                let btn = |action: &str, text: &str, extra: &str| {
                    format!(
                        "<form method=\"post\" action=\"/accounts/action\" style=\"display:inline\"><input type=\"hidden\" name=\"csrf\" value=\"{}\"><input type=\"hidden\" name=\"label\" value=\"{}\"><input type=\"hidden\" name=\"action\" value=\"{action}\"><button class=\"btn small {extra}\">{text}</button></form> ",
                        sess.csrf,
                        esc(label)
                    )
                };
                let mut actions = String::new();
                if removable {
                    if i > 0 && rows[i - 1].get("removable").and_then(Value::as_bool) == Some(true) {
                        actions.push_str(&btn("up", "Up", "ghost"));
                    }
                    if i + 1 < rows.len() {
                        actions.push_str(&btn("down", "Down", "ghost"));
                    }
                    actions.push_str(&if enabled { btn("disable", "Disable", "") } else { btn("enable", "Enable", "") });
                }
                if sleep > 0 {
                    actions.push_str(&btn("reset", "Use now", ""));
                }
                if removable {
                    actions.push_str(&format!(
                        "<form method=\"post\" action=\"/accounts/action\" style=\"display:inline\"><input type=\"hidden\" name=\"csrf\" value=\"{}\"><input type=\"hidden\" name=\"label\" value=\"{}\"><input type=\"hidden\" name=\"action\" value=\"remove\"><button class=\"btn small danger\" data-confirm=\"Remove this account from the list?\" title=\"Remove\">{}</button></form>",
                        sess.csrf,
                        esc(label),
                        icon("trash")
                    ));
                }
                b.push_str(&format!(
                    "<tr><td>{}</td><td>{}{}</td><td>{}</td><td><code>{}</code></td><td>{pill}</td><td class=\"muted small\">{}</td><td style=\"text-align:right;white-space:nowrap\">{actions}</td></tr>",
                    i + 1,
                    esc(label),
                    if label == "environment" {
                        " <span class=\"muted small\">(set on the Connections page or in the environment)</span>".to_string()
                    } else {
                        match r.get("owner").and_then(Value::as_str).filter(|o| !o.is_empty()) {
                            Some(o) => format!(" <span class=\"pill\">only for {}</span>", esc(o)),
                            None => " <span class=\"muted small\">shared</span>".to_string(),
                        }
                    },
                    esc(&kind.replace('_', " ")),
                    esc(token),
                    esc(problem)
                ));
            }
            b.push_str("</tbody></table></div></div>");
        }
    }
    b.push_str(&format!(
        "<div class=\"card\" style=\"margin-top:1rem\"><h2>Add an account</h2><form method=\"post\" action=\"/accounts\" autocomplete=\"off\"><input type=\"hidden\" name=\"csrf\" value=\"{}\"><div class=\"row\">\
<div><label for=\"al\">Name</label><input id=\"al\" type=\"text\" name=\"label\" required maxlength=\"40\" placeholder=\"backup\"></div>\
<div><label for=\"ak\">Kind</label><select id=\"ak\" name=\"kind\"><option value=\"oauth\">Claude subscription token</option><option value=\"api_key\">Anthropic API key</option></select></div>\
<div><label for=\"at\">Token</label><input id=\"at\" type=\"password\" name=\"token\" required maxlength=\"400\" autocomplete=\"off\" style=\"width:22rem\"></div>\
<div class=\"row tight\"><button class=\"btn primary\">{} Add</button></div></div>\
<p class=\"hint\">Get a subscription token by running <code>claude setup-token</code> on a computer where that account is signed in, then paste it here. It is stored in the data directory, readable only by tasklane, and is never shown again (only its last four characters). A subscription token is for your own use; for a team, use API keys.</p></form></div>\
<div class=\"card\" style=\"margin-top:1rem\"><h2>How the switch works</h2><p class=\"small muted\">Accounts marked shared serve everyone. A person's own account (added by them under My account) is used only for work that person approved or asked for. The first account that is ready does the work. If it reports that its usage limit is reached it sleeps for 30 minutes (a refused token sleeps for a day), and the same run starts again with the next account. The ticket, the feedback and the working copy carry over; the developer is told that part of the work may already be there. A conversation cannot move between accounts, so the new run reads the code again instead of remembering. When every account sleeps, the work waits for the first to wake up and no attempt is used up.</p></div>",
        sess.csrf,
        icon("plus")
    ));
    render(&st, &sess.csrf, "accounts", "Model accounts", &b)
        .await
        .into_response()
}

#[derive(Deserialize)]
pub struct AddIn {
    csrf: String,
    label: String,
    kind: String,
    token: String,
}

pub async fn add(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Form(f): Form<AddIn>,
) -> Response {
    if !csrf_ok(&st, &headers, &f.csrf).await {
        return forbidden();
    }
    let r = st
        .orch
        .engine_call_with(
            "account",
            json!({"action": "add", "label": f.label.trim(), "kind": f.kind, "token": f.token.trim()}),
            CALL,
        )
        .await;
    done(&st, "accounts.added", f.label.trim(), r).await
}

#[derive(Deserialize)]
pub struct ActionIn {
    csrf: String,
    label: String,
    action: String,
}

pub async fn action(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Form(f): Form<ActionIn>,
) -> Response {
    if !csrf_ok(&st, &headers, &f.csrf).await {
        return forbidden();
    }
    if !matches!(
        f.action.as_str(),
        "up" | "down" | "enable" | "disable" | "reset" | "remove"
    ) {
        return (StatusCode::UNPROCESSABLE_ENTITY, "unknown action").into_response();
    }
    let r = st
        .orch
        .engine_call_with(
            "account",
            json!({"action": f.action, "label": f.label}),
            CALL,
        )
        .await;
    done(&st, &format!("accounts.{}", f.action), &f.label, r).await
}

/// Record the change (never the token) and go back to the list, or say what was wrong.
async fn done(
    st: &AppState,
    what: &str,
    label: &str,
    r: Result<Value, tasklane_orchestrator::Error>,
) -> Response {
    match r {
        Ok(_) => {
            let _ = sqlx::query(
                "INSERT INTO audit_events (actor, action, detail) VALUES ('console:admin', $1, $2)",
            )
            .bind(what)
            .bind(json!({"account": label}))
            .execute(st.orch.pool())
            .await;
            redirect("/accounts")
        }
        Err(e) => {
            // a plug-in message is plain text about the input ("an account called x already exists")
            let text = e.to_string();
            let text = text.split_once("): ").map(|(_, m)| m).unwrap_or(&text);
            (StatusCode::UNPROCESSABLE_ENTITY, text.to_string()).into_response()
        }
    }
}
