//! Connections: GitHub (created from a manifest in one click) and the agent (key, isolation, test).
//! Nothing secret is ever shown again after it is saved; the pages only say whether it exists.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Form, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use serde::Deserialize;
use serde_json::{json, Value};

use super::ui::{empty, head, icon, render};
use super::{cookie_value, csrf_ok, forbidden, guard, random_hex, redirect, AppState};
use crate::connections::{self, AgentSettings, GithubConn};
use crate::esc;

/// Where this console is, as the browser sees it. The operator's setting wins; otherwise the
/// address the user is using right now (so `localhost` works for a first try).
fn base_url(st: &AppState, headers: &HeaderMap) -> String {
    if let Some(u) = &st.public_url {
        return u.clone();
    }
    let host = headers
        .get("host")
        .and_then(|v| v.to_str().ok())
        .filter(|h| {
            !h.is_empty()
                && h.len() < 120
                && h.bytes().all(|b| {
                    b.is_ascii_alphanumeric()
                        || b == b'.'
                        || b == b':'
                        || b == b'-'
                        || b == b'['
                        || b == b']'
                })
        })
        .unwrap_or("localhost:8080");
    let https = st.secure_cookies
        || headers
            .get("x-forwarded-proto")
            .and_then(|v| v.to_str().ok())
            == Some("https");
    format!("{}://{host}", if https { "https" } else { "http" })
}

/// The manifest GitHub turns into an App. Webhooks are requested only when the console has a public
/// https address (GitHub cannot reach `localhost`); without them tasklane polls every minute.
pub fn manifest(name: &str, base: &str) -> Value {
    let mut m = json!({
        "name": name,
        "url": "https://github.com/kunci115/tasklane",
        "redirect_url": format!("{base}/connect/github/callback"),
        "setup_url": format!("{base}/connect/github/installed"),
        "setup_on_update": false,
        "public": false,
        "default_permissions": {
            "issues": "write", "pull_requests": "write", "contents": "write",
            "metadata": "read", "checks": "read", "statuses": "read"
        },
        "default_events": []
    });
    if base.starts_with("https://") {
        m["hook_attributes"] =
            json!({"url": format!("{base}/api/v1/webhook/source"), "active": true});
        m["default_events"] = json!(["issues", "issue_comment", "pull_request", "check_suite"]);
    }
    m
}

fn clean_name(raw: &str) -> String {
    let s: String = raw
        .trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    s.trim_matches('-').chars().take(34).collect()
}

fn flash(code: Option<&str>) -> String {
    let (class, text) = match code {
        Some("github_connected") => ("ok", "GitHub is connected."),
        Some("github_removed") => (
            "",
            "GitHub was disconnected here. Delete the App on GitHub too if you no longer need it.",
        ),
        Some("agent_saved") => ("ok", "Saved."),
        Some("agent_removed") => ("", "The key was removed."),
        _ => return String::new(),
    };
    format!("<div class=\"card\" style=\"margin-bottom:1rem\"><span class=\"pill {class}\">{}</span></div>", esc(text))
}

#[derive(Deserialize)]
pub struct FlashQuery {
    msg: Option<String>,
}

pub async fn page(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<FlashQuery>,
) -> Response {
    let sess = match guard(&st, &headers).await {
        Ok(s) => s,
        Err(r) => return r,
    };
    let pool = st.orch.pool();
    let (src, eng) = st.orch.describe_plugins().await;
    let mut gh: Option<GithubConn> = connections::get(pool, "github").await;
    // the App was installed but GitHub did not send the user back here: find the installation ourselves
    if let Some(c) = &gh {
        if c.installation_id.is_none() {
            if let Ok(v) = st
                .orch
                .source_call_with("list_installations", json!({}), Duration::from_secs(20))
                .await
            {
                let list = v
                    .get("installations")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                let pick = list
                    .iter()
                    .find(|i| i.get("account").and_then(Value::as_str) == Some(c.owner.as_str()))
                    .or_else(|| list.first());
                if let Some(id) = pick.and_then(|i| i.get("id")).and_then(Value::as_str) {
                    let mut c2 = c.clone();
                    c2.installation_id = Some(id.to_string());
                    let _ = connections::put(pool, "github", &c2).await;
                    let _ = connections::apply(&st).await;
                    gh = Some(c2);
                }
            }
        }
    }
    let repos: i64 = sqlx::query_scalar("SELECT count(*) FROM services WHERE NOT archived")
        .fetch_one(pool)
        .await
        .unwrap_or(0);
    let src_ok = src
        .as_ref()
        .and_then(|s| s.get("configured"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let agent: AgentSettings = connections::get(pool, "agent").await.unwrap_or_default();
    let has_key = eng
        .as_ref()
        .and_then(|e| e.get("credential"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let sandbox = eng
        .as_ref()
        .and_then(|e| e.get("sandbox"))
        .and_then(Value::as_str)
        .unwrap_or("none");
    let allowed = eng
        .as_ref()
        .and_then(|e| e.get("unsandboxed_allowed"))
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let mut b = head("Connections", "Where tasklane gets its tickets and how its agent works. Secrets are saved on the server and never shown again.", "");
    b.push_str(&flash(q.msg.as_deref()));
    b.push_str("<div class=\"grid g2\">");
    // GitHub card
    b.push_str("<div class=\"card\"><div class=\"row tight\" style=\"align-items:center\"><h2 style=\"margin:0\">GitHub</h2>");
    match (&gh, src_ok) {
        (Some(c), _) if c.installation_id.is_some() => b.push_str(&format!("<span class=\"pill ok\">connected</span></div><p class=\"muted\">App <strong>{}</strong> owned by {}. {} repositor{} found.</p><div class=\"row tight\"><a class=\"btn small\" href=\"{}\" target=\"_blank\" rel=\"noopener\">{} App on GitHub</a><a class=\"btn small\" href=\"/services\">Choose repositories</a></div>",
            esc(&c.slug), esc(&c.owner), repos, if repos == 1 { "y" } else { "ies" }, esc(&c.html_url), icon("external"))),
        (Some(c), _) => b.push_str(&format!("<span class=\"pill warn\">one step left</span></div><p class=\"muted\">The App <strong>{}</strong> exists. Install it on the repositories tasklane should work on.</p><a class=\"btn primary\" href=\"/connect/github/install\">Install the App</a>", esc(&c.slug))),
        (None, true) => b.push_str(&format!("<span class=\"pill ok\">connected</span></div><p class=\"muted\">Connected through the server's own settings (environment variables). {repos} repositor{} found.</p>", if repos == 1 { "y" } else { "ies" })),
        (None, false) => b.push_str("<span class=\"pill\">not connected</span></div><p class=\"muted\">tasklane needs a GitHub App to read issues and open pull requests. It takes one click: GitHub opens with everything filled in, you press Create, then Install.</p><a class=\"btn primary\" href=\"/connect/github\">Connect GitHub</a>"),
    }
    if gh.is_some() {
        b.push_str(&format!("<form method=\"post\" action=\"/connect/github/disconnect\" style=\"margin-top:.8rem\"><input type=\"hidden\" name=\"csrf\" value=\"{}\"><button class=\"btn small danger\" data-confirm=\"Disconnect GitHub? tasklane stops reading tickets until you connect again.\">Disconnect</button></form>", sess.csrf));
    }
    b.push_str("</div>");
    // Agent card
    let ready = (has_key || agent.sandbox == "none" && has_key) && (sandbox == "docker" || allowed);
    b.push_str("<div class=\"card\"><div class=\"row tight\" style=\"align-items:center\"><h2 style=\"margin:0\">Agent (Claude)</h2>");
    b.push_str(if ready {
        "<span class=\"pill ok\">ready</span>"
    } else if has_key {
        "<span class=\"pill warn\">choose isolation</span>"
    } else {
        "<span class=\"pill\">not connected</span>"
    });
    b.push_str("</div>");
    b.push_str(&format!(
        "<p class=\"muted\">{}</p><div class=\"row tight\"><a class=\"btn {}\" href=\"/connect/agent\">{}</a></div>",
        match (has_key, sandbox, allowed) {
            (false, _, _) => "Add the credential the agent uses and choose how it is isolated from your server.".to_string(),
            (true, "docker", _) => "Key saved. The agent runs in a Docker sandbox.".to_string(),
            (true, _, true) => "Key saved. The agent runs on this server without a sandbox, because you accepted the risk.".to_string(),
            (true, _, false) => "Key saved, but no isolation is chosen. The agent will not run until you choose one.".to_string(),
        },
        if ready { "" } else { "primary" },
        if has_key { "Change settings" } else { "Connect the agent" }
    ));
    b.push_str("</div></div>");
    b.push_str(&format!("<div class=\"card\" style=\"margin-top:1rem\"><h2>Chat</h2><p class=\"muted\">Ask questions and approve from Telegram groups. Set up the bot token on the server, then register groups here.</p><a class=\"btn small\" href=\"/chat\">{} Chat groups</a></div>", icon("chat")));
    render(&st, &sess.csrf, "connections", "Connections", &b)
        .await
        .into_response()
}

pub async fn github_form(State(st): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let sess = match guard(&st, &headers).await {
        Ok(s) => s,
        Err(r) => return r,
    };
    let base = base_url(&st, &headers);
    let hooks = base.starts_with("https://");
    let mut b = head(
        "Connect GitHub",
        "tasklane works through a GitHub App that only you own.",
        "<a class=\"btn\" href=\"/connections\">Back</a>",
    );
    b.push_str(&format!(
        "<div class=\"grid g2\"><div class=\"card\"><form method=\"post\" action=\"/connect/github/start\"><input type=\"hidden\" name=\"csrf\" value=\"{}\">\
<div class=\"field\"><label for=\"nm\">Name of the App</label><input id=\"nm\" type=\"text\" name=\"name\" maxlength=\"34\" value=\"tasklane-{}\" required><div class=\"hint\">GitHub names are unique. Letters, numbers and hyphens.</div></div>\
<div class=\"field\"><label>Create it under</label><label style=\"font-weight:400\"><input type=\"radio\" name=\"owner_type\" value=\"user\" checked> my personal account</label>\
<label style=\"font-weight:400\"><input type=\"radio\" name=\"owner_type\" value=\"org\"> an organization</label></div>\
<div class=\"field\"><label for=\"org\">Organization name (only if you chose organization)</label><input id=\"org\" type=\"text\" name=\"org\" maxlength=\"60\" placeholder=\"my-company\"></div>\
<button class=\"btn primary\">{} Continue to GitHub</button></form></div>\
<div class=\"card\"><h2>What happens</h2><ol class=\"small\" style=\"padding-left:1.2rem;margin:0\"><li>GitHub opens with the App already filled in. You press <strong>Create GitHub App</strong>.</li><li>You come back here. tasklane stores the App's credentials on the server.</li><li>GitHub asks where to install it. Choose <strong>only the repositories</strong> tasklane should work on.</li></ol>\
<p class=\"hint\">The App gets: issues, pull requests and code (read and write), checks and statuses (read). It does <strong>not</strong> get permission to change CI workflow files. {}</p>\
<p class=\"hint\">GitHub sends you back to <span class=\"mono\">{}</span>. If that address is not reachable from your browser, set TASKLANE_PUBLIC_URL on the server first.</p></div></div>",
        sess.csrf,
        &random_hex()[..4],
        icon("external"),
        if hooks { "Webhooks are on, so tickets are picked up at once." } else { "Webhooks are off because this address is not public https; tasklane checks for new issues every minute instead." },
        esc(&base)
    ));
    render(&st, &sess.csrf, "connections", "Connect GitHub", &b)
        .await
        .into_response()
}

#[derive(Deserialize)]
pub struct StartIn {
    csrf: String,
    name: String,
    owner_type: String,
    org: Option<String>,
}

/// Remember a one-time state for this session, then hand the browser to GitHub with the manifest.
pub async fn github_start(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Form(f): Form<StartIn>,
) -> Response {
    if !csrf_ok(&st, &headers, &f.csrf).await {
        return forbidden();
    }
    let name = clean_name(&f.name);
    let org = f.org.as_deref().map(str::trim).unwrap_or("");
    if name.len() < 3
        || (f.owner_type == "org"
            && (org.is_empty() || !org.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')))
    {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            "Check the name (3 or more letters, numbers or hyphens) and the organization.",
        )
            .into_response();
    }
    let Some(session) = cookie_value(&headers) else {
        return forbidden();
    };
    let state = random_hex();
    if sqlx::query("INSERT INTO setup_states (state, session_id, purpose, expires_at) VALUES ($1, $2, 'github_manifest', now() + interval '20 minutes')")
        .bind(&state)
        .bind(&session)
        .execute(st.orch.pool())
        .await
        .is_err()
    {
        return (StatusCode::INTERNAL_SERVER_ERROR, "Could not start.").into_response();
    }
    let base = base_url(&st, &headers);
    let manifest = manifest(&name, &base).to_string();
    let target = if f.owner_type == "org" {
        format!("https://github.com/organizations/{org}/settings/apps/new?state={state}")
    } else {
        format!("https://github.com/settings/apps/new?state={state}")
    };
    Html(format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><title>Opening GitHub - tasklane</title><link rel=\"stylesheet\" href=\"/static/app.css\"></head><body>\
<div class=\"login\"><div class=\"card\"><h1>Opening GitHub</h1><p class=\"muted\">If nothing happens, press the button.</p>\
<form method=\"post\" action=\"{}\" data-autosubmit><input type=\"hidden\" name=\"manifest\" value=\"{}\"><button class=\"btn primary\" style=\"width:100%\">Continue to GitHub</button></form></div></div><script src=\"/static/app.js\"></script></body></html>",
        esc(&target),
        esc(&manifest)
    ))
    .into_response()
}

#[derive(Deserialize)]
pub struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
}

async fn page_message(
    st: &AppState,
    csrf: &str,
    status: StatusCode,
    title: &str,
    text: &str,
    back: &str,
) -> Response {
    let b = format!(
        "{}<div class=\"card\"><p>{}</p><a class=\"btn\" href=\"{}\">Back</a></div>",
        head(title, "", ""),
        esc(text),
        esc(back)
    );
    (status, render(st, csrf, "connections", title, &b).await).into_response()
}

/// GitHub sends the user back here with a one-time code. The state must be one this session started.
pub async fn github_callback(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<CallbackQuery>,
) -> Response {
    let sess = match guard(&st, &headers).await {
        Ok(s) => s,
        Err(r) => return r,
    };
    let (Some(code), Some(state), Some(session)) = (
        q.code.as_deref(),
        q.state.as_deref(),
        cookie_value(&headers),
    ) else {
        return page_message(
            &st,
            &sess.csrf,
            StatusCode::BAD_REQUEST,
            "Not completed",
            "This page is only for GitHub to send you back to after creating the App.",
            "/connect/github",
        )
        .await;
    };
    // one use only: the state is deleted as it is checked
    let valid: Option<String> = sqlx::query_scalar("DELETE FROM setup_states WHERE state = $1 AND session_id = $2 AND purpose = 'github_manifest' AND expires_at > now() RETURNING state")
        .bind(state)
        .bind(&session)
        .fetch_optional(st.orch.pool())
        .await
        .ok()
        .flatten();
    if valid.is_none() {
        return page_message(
            &st,
            &sess.csrf,
            StatusCode::BAD_REQUEST,
            "Not completed",
            "This link has expired or was not started from this console. Start again.",
            "/connect/github",
        )
        .await;
    }
    let converted = match st
        .orch
        .source_call_with(
            "app_manifest_convert",
            json!({"code": code}),
            Duration::from_secs(30),
        )
        .await
    {
        Ok(v) => v,
        Err(e) => {
            eprintln!("github manifest: {e}");
            return page_message(
                &st,
                &sess.csrf,
                StatusCode::BAD_GATEWAY,
                "GitHub did not hand over the App",
                "The code was refused or has been used already. Start again from Connect GitHub.",
                "/connect/github",
            )
            .await;
        }
    };
    let field = |k: &str| {
        converted
            .get(k)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let (pem, webhook) = (field("pem"), field("webhook_secret"));
    if pem.is_empty() || field("app_id").is_empty() {
        return page_message(
            &st,
            &sess.csrf,
            StatusCode::BAD_GATEWAY,
            "GitHub did not hand over the App",
            "The answer had no credentials. Start again.",
            "/connect/github",
        )
        .await;
    }
    let conn = GithubConn {
        app_id: field("app_id"),
        slug: field("slug"),
        owner: field("owner"),
        owner_type: field("owner_type"),
        html_url: field("html_url"),
        installation_id: None,
    };
    let saved = st
        .secrets
        .write(connections::GITHUB_KEY, &pem)
        .and_then(|_| {
            if webhook.is_empty() {
                Ok(())
            } else {
                st.secrets.write(connections::GITHUB_WEBHOOK, &webhook)
            }
        });
    if saved.is_err()
        || connections::put(st.orch.pool(), "github", &conn)
            .await
            .is_err()
    {
        return page_message(&st, &sess.csrf, StatusCode::INTERNAL_SERVER_ERROR, "Could not save", "The credentials could not be stored on the server. Check that the data directory is writable.", "/connections").await;
    }
    let _ = sqlx::query("INSERT INTO audit_events (actor, action, detail) VALUES ('console:admin', 'connection.github_created', $1)").bind(json!({"app": conn.slug, "owner": conn.owner})).execute(st.orch.pool()).await;
    if let Err(e) = connections::apply(&st).await {
        eprintln!("restarting the source plug-in: {e}");
    }
    redirect("/connect/github/install")
}

/// The App is private, so GitHub's public `/apps/<slug>/installations/new` page answers 404.
/// Its owner installs it from the settings page that lists the App's installations.
fn install_url(gh: &GithubConn) -> String {
    if gh.owner_type == "Organization" {
        format!("https://github.com/organizations/{}/settings/apps/{}/installations", gh.owner, gh.slug)
    } else {
        format!("https://github.com/settings/apps/{}/installations", gh.slug)
    }
}

pub async fn github_install(State(st): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let sess = match guard(&st, &headers).await {
        Ok(s) => s,
        Err(r) => return r,
    };
    let Some(mut gh) = connections::get::<GithubConn>(st.orch.pool(), "github").await else {
        return redirect("/connect/github");
    };
    // an App made before the owner type was stored: ask GitHub, remember the answer, and
    // skip the install step if the App is already installed
    if gh.owner_type.is_empty() || gh.installation_id.is_none() {
        if let Ok(v) = st
            .orch
            .source_call_with("list_installations", json!({}), Duration::from_secs(20))
            .await
        {
            if let Some(t) = v.get("owner_type").and_then(Value::as_str).filter(|t| !t.is_empty()) {
                gh.owner_type = t.to_string();
            }
            let found = v
                .get("installations")
                .and_then(Value::as_array)
                .and_then(|a| a.first())
                .and_then(|i| i.get("id"))
                .and_then(Value::as_str)
                .map(str::to_string);
            if gh.installation_id.is_none() && found.is_some() {
                gh.installation_id = found;
                let _ = connections::put(st.orch.pool(), "github", &gh).await;
                let _ = connections::apply(&st).await;
                return redirect("/connections");
            }
            let _ = connections::put(st.orch.pool(), "github", &gh).await;
        }
    }
    let b = format!(
        "{}<div class=\"card\"><h2>Install the App</h2><p>The App <strong>{}</strong> exists. Now choose where it works. <strong>Select only the repositories</strong> tasklane should handle, not all repositories.</p>\
<a class=\"btn primary\" href=\"{}\">{} Install on GitHub</a>\
<p class=\"hint\">After installing, GitHub brings you back here. If it does not, open <a href=\"/connections\">Connections</a>; tasklane finds the installation by itself.</p></div>",
        head("Almost there", "One more step on GitHub.", ""),
        esc(&gh.slug),
        esc(&install_url(&gh)),
        icon("external")
    );
    render(&st, &sess.csrf, "connections", "Install the App", &b)
        .await
        .into_response()
}

#[derive(Deserialize)]
pub struct InstalledQuery {
    installation_id: Option<String>,
}

/// GitHub's "setup URL": the user comes back after installing and the installation id is in the address.
pub async fn github_installed(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<InstalledQuery>,
) -> Response {
    if let Err(r) = guard(&st, &headers).await {
        return r;
    }
    let Some(mut gh) = connections::get::<GithubConn>(st.orch.pool(), "github").await else {
        return redirect("/connections");
    };
    if let Some(id) = q
        .installation_id
        .as_deref()
        .filter(|i| !i.is_empty() && i.len() < 20 && i.bytes().all(|b| b.is_ascii_digit()))
    {
        gh.installation_id = Some(id.to_string());
        let _ = connections::put(st.orch.pool(), "github", &gh).await;
        let _ = connections::apply(&st).await;
        let orch = st.orch.clone();
        tokio::spawn(async move {
            if let Err(e) = orch.sync_once().await {
                eprintln!("first sync: {e}");
            }
        });
        let _ = sqlx::query("INSERT INTO audit_events (actor, action, detail) VALUES ('console:admin', 'connection.github_installed', '{}')").execute(st.orch.pool()).await;
    }
    redirect("/connections?msg=github_connected")
}

#[derive(Deserialize)]
pub struct CsrfOnly {
    csrf: String,
}

pub async fn github_disconnect(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Form(f): Form<CsrfOnly>,
) -> Response {
    if !csrf_ok(&st, &headers, &f.csrf).await {
        return forbidden();
    }
    st.secrets.delete(connections::GITHUB_KEY);
    st.secrets.delete(connections::GITHUB_WEBHOOK);
    let _ = connections::remove(st.orch.pool(), "github").await;
    let _ = sqlx::query("INSERT INTO audit_events (actor, action, detail) VALUES ('console:admin', 'connection.github_removed', '{}')").execute(st.orch.pool()).await;
    let _ = connections::apply(&st).await;
    redirect("/connections?msg=github_removed")
}

pub async fn agent_form(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<FlashQuery>,
) -> Response {
    let sess = match guard(&st, &headers).await {
        Ok(s) => s,
        Err(r) => return r,
    };
    let (_, eng) = st.orch.describe_plugins().await;
    let has_key = eng
        .as_ref()
        .and_then(|e| e.get("credential"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let agent: AgentSettings = connections::get(st.orch.pool(), "agent")
        .await
        .unwrap_or_default();
    let sel = |v: &str| if agent.sandbox == v { " checked" } else { "" };
    let mut b = head(
        "Connect the agent",
        "The credential tasklane's agent uses, and how it is kept away from your server.",
        "<a class=\"btn\" href=\"/connections\">Back</a>",
    );
    b.push_str(&flash(q.msg.as_deref()));
    b.push_str(&format!(
        "<div class=\"grid g2\"><div class=\"card\"><form method=\"post\" action=\"/connect/agent\"><input type=\"hidden\" name=\"csrf\" value=\"{}\">\
<div class=\"field\"><label>How the agent signs in</label>\
<label style=\"font-weight:400\"><input type=\"radio\" name=\"auth\" value=\"api_key\"{}> <strong>API key</strong>. Create a key of its own in the Anthropic console, with a spending limit.</label>\
<label style=\"font-weight:400\"><input type=\"radio\" name=\"auth\" value=\"subscription\"{}> <strong>Claude subscription</strong> (no API key). On a computer where you are signed in to Claude Code, run <span class=\"mono\">claude setup-token</span> and paste the token it prints. The work counts against your plan's usage limits.</label></div>\
<div class=\"field\"><label for=\"key\">Key or token {}</label><input id=\"key\" type=\"password\" name=\"key\" autocomplete=\"off\" placeholder=\"{}\" maxlength=\"400\"><div class=\"hint\">It is stored on the server and never shown again.</div></div>\
<div class=\"field\"><label>Isolation</label>\
<label style=\"font-weight:400\"><input type=\"radio\" name=\"sandbox\" value=\"docker\"{}> <strong>Docker sandbox</strong> (recommended). The agent runs in a locked-down container that holds none of your secrets. Needs the Docker socket available to tasklane (see the guide: docker-compose.sandbox.yml).</label>\
<label style=\"font-weight:400\"><input type=\"radio\" name=\"sandbox\" value=\"none\"{}> <strong>No sandbox</strong>. The agent runs as the same user as tasklane and can read its files and secrets. Only for a machine that holds nothing else, with repositories and reporters you fully trust.</label></div>\
<details {}><summary class=\"btn small\">Docker settings</summary><div style=\"margin-top:.8rem\"><div class=\"field\"><label for=\"im\">Sandbox image</label><input id=\"im\" type=\"text\" name=\"image\" value=\"{}\" placeholder=\"tasklane-sandbox\"><div class=\"hint\">Built from Dockerfile.sandbox. It holds only the agent program.</div></div>\
<div class=\"field\"><label for=\"nw\">Docker network</label><input id=\"nw\" type=\"text\" name=\"network\" value=\"{}\" placeholder=\"bridge\"><div class=\"hint\">A network that can reach only the model API, if you built one. The built-in <span class=\"mono\">bridge</span> reaches everything.</div></div></div></details>\
<div class=\"row tight\" style=\"margin-top:1rem\"><button class=\"btn primary\">{} Save</button></div></form></div>",
        sess.csrf,
        if agent.auth != "subscription" { " checked" } else { "" },
        if agent.auth == "subscription" { " checked" } else { "" },
        if has_key { "<span class=\"pill ok\">saved</span>" } else { "" },
        if has_key { "Leave empty to keep the saved one" } else { "sk-ant-..." },
        sel("docker"),
        sel("none"),
        if agent.sandbox == "docker" { "open" } else { "" },
        esc(&agent.image),
        esc(&agent.network),
        icon("check")
    ));
    b.push_str(&format!(
        "<div class=\"card\"><h2>Test it</h2><p class=\"muted\">Sends one tiny question through everything above: the key, the isolation and the agent program. It costs a fraction of a cent.</p>\
<form method=\"post\" action=\"/connect/agent/test\"><input type=\"hidden\" name=\"csrf\" value=\"{}\"><button class=\"btn\">{} Run the test</button></form>{}</div></div>",
        sess.csrf,
        icon("play"),
        if has_key { format!("<form method=\"post\" action=\"/connect/agent/disconnect\" style=\"margin-top:1rem\"><input type=\"hidden\" name=\"csrf\" value=\"{}\"><button class=\"btn small danger\" data-confirm=\"Remove the saved key?\">Remove the key</button></form>", sess.csrf) } else { String::new() }
    ));
    render(&st, &sess.csrf, "connections", "Connect the agent", &b)
        .await
        .into_response()
}

#[derive(Deserialize)]
pub struct AgentIn {
    csrf: String,
    key: Option<String>,
    sandbox: Option<String>,
    image: Option<String>,
    network: Option<String>,
    auth: Option<String>,
}

fn safe_ref(s: &str) -> bool {
    s.is_empty()
        || (s.len() <= 128
            && s.bytes().next().is_some_and(|b| b.is_ascii_alphanumeric())
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_.:/@-".contains(&b)))
}

pub async fn agent_save(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Form(f): Form<AgentIn>,
) -> Response {
    if !csrf_ok(&st, &headers, &f.csrf).await {
        return forbidden();
    }
    let key = f.key.as_deref().map(str::trim).unwrap_or("");
    let sandbox = f.sandbox.as_deref().unwrap_or("");
    let (image, network) = (
        f.image.as_deref().map(str::trim).unwrap_or(""),
        f.network.as_deref().map(str::trim).unwrap_or(""),
    );
    if !key.is_empty()
        && (key.len() > 400
            || key
                .bytes()
                .any(|b| b.is_ascii_whitespace() || b.is_ascii_control()))
    {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            "That does not look like an API key (no spaces or line breaks).",
        )
            .into_response();
    }
    let previous: AgentSettings = connections::get(st.orch.pool(), "agent")
        .await
        .unwrap_or_default();
    let auth = match f.auth.as_deref().unwrap_or("") {
        "" => previous.auth.as_str(),
        a @ ("api_key" | "subscription") => a,
        _ => {
            return (StatusCode::UNPROCESSABLE_ENTITY, "Choose API key or Claude subscription.")
                .into_response()
        }
    };
    let was_sub = previous.auth == "subscription";
    if key.is_empty() && (auth == "subscription") != was_sub && st.secrets.exists(connections::AGENT_KEY) {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            "You changed how the agent signs in, so paste the matching key or token too.",
        )
            .into_response();
    }
    if sandbox.is_empty() && previous.sandbox.is_empty() {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            "Choose how the agent is isolated: the Docker sandbox (recommended) or no sandbox.",
        )
            .into_response();
    }
    if !matches!(sandbox, "docker" | "none" | "")
        || !safe_ref(image)
        || !safe_ref(network)
        || (sandbox == "docker" && (image.is_empty() || network.is_empty()))
    {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            "Check the isolation settings. Docker needs an image and a network name.",
        )
            .into_response();
    }
    if !key.is_empty() && st.secrets.write(connections::AGENT_KEY, key).is_err() {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            "The key could not be stored on the server.",
        )
            .into_response();
    }
    let settings = AgentSettings {
        sandbox: sandbox.to_string(),
        image: image.to_string(),
        network: network.to_string(),
        auth: auth.to_string(),
    };
    let _ = connections::put(st.orch.pool(), "agent", &settings).await;
    let _ = sqlx::query("INSERT INTO audit_events (actor, action, detail) VALUES ('console:admin', 'connection.agent_saved', $1)").bind(json!({"sandbox": sandbox, "auth": auth, "key_changed": !key.is_empty()})).execute(st.orch.pool()).await;
    if let Err(e) = connections::apply(&st).await {
        eprintln!("restarting the engine plug-in: {e}");
    }
    redirect("/connect/agent?msg=agent_saved")
}

pub async fn agent_disconnect(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Form(f): Form<CsrfOnly>,
) -> Response {
    if !csrf_ok(&st, &headers, &f.csrf).await {
        return forbidden();
    }
    st.secrets.delete(connections::AGENT_KEY);
    let _ = connections::apply(&st).await;
    redirect("/connect/agent?msg=agent_removed")
}

pub async fn agent_test(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Form(f): Form<CsrfOnly>,
) -> Response {
    if !csrf_ok(&st, &headers, &f.csrf).await {
        return forbidden();
    }
    let sess = match guard(&st, &headers).await {
        Ok(s) => s,
        Err(r) => return r,
    };
    let r = st
        .orch
        .engine_call_with("check", json!({}), Duration::from_secs(150))
        .await;
    let (ok, text) = match &r {
        Ok(v) if v.get("ok").and_then(Value::as_bool) == Some(true) => (
            true,
            format!(
                "It works. The agent answered in {} seconds{}.",
                v.get("secs").and_then(Value::as_f64).unwrap_or(0.0),
                if v.get("sandbox").and_then(Value::as_str) == Some("docker") {
                    ", inside the Docker sandbox"
                } else {
                    ""
                }
            ),
        ),
        Ok(v) => (
            false,
            v.get("message")
                .and_then(Value::as_str)
                .unwrap_or("The test failed.")
                .to_string(),
        ),
        Err(e) => (false, format!("The agent program did not answer: {e}")),
    };
    let b = format!(
        "{}<div class=\"card\"><span class=\"pill {}\">{}</span><p style=\"margin-top:.8rem\">{}</p><div class=\"row tight\"><a class=\"btn\" href=\"/connect/agent\">Back</a><a class=\"btn\" href=\"/connections\">Connections</a></div></div>",
        head("Agent test", "", ""),
        if ok { "ok" } else { "bad" },
        if ok { "passed" } else { "failed" },
        esc(&text)
    );
    let _ = empty;
    render(&st, &sess.csrf, "connections", "Agent test", &b)
        .await
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_install_link_is_the_owners_settings_page_not_the_404_public_one() {
        let mut gh = GithubConn { app_id: "1".into(), slug: "my-app".into(), owner: "acme".into(), owner_type: "Organization".into(), html_url: String::new(), installation_id: None };
        assert_eq!(install_url(&gh), "https://github.com/organizations/acme/settings/apps/my-app/installations");
        gh.owner_type = "User".into();
        assert_eq!(install_url(&gh), "https://github.com/settings/apps/my-app/installations");
    }

    #[test]
    fn the_manifest_asks_for_exactly_the_permissions_tasklane_needs_and_no_workflow_access() {
        let m = manifest("tasklane-ab12", "http://localhost:8080");
        let p = &m["default_permissions"];
        assert_eq!(
            (
                p["issues"].as_str(),
                p["pull_requests"].as_str(),
                p["contents"].as_str()
            ),
            (Some("write"), Some("write"), Some("write"))
        );
        assert_eq!(
            (
                p["metadata"].as_str(),
                p["checks"].as_str(),
                p["statuses"].as_str()
            ),
            (Some("read"), Some("read"), Some("read"))
        );
        assert!(
            p.get("workflows").is_none() && p.get("administration").is_none(),
            "no more than needed"
        );
        assert_eq!(m["public"], false);
        assert_eq!(
            m["redirect_url"],
            "http://localhost:8080/connect/github/callback"
        );
        assert_eq!(
            m["setup_url"],
            "http://localhost:8080/connect/github/installed"
        );
    }

    #[test]
    fn webhooks_are_requested_only_for_a_public_https_address() {
        assert!(manifest("n", "http://localhost:8080")
            .get("hook_attributes")
            .is_none());
        let m = manifest("n", "https://tasklane.example.com");
        assert_eq!(
            m["hook_attributes"]["url"],
            "https://tasklane.example.com/api/v1/webhook/source"
        );
        assert_eq!(m["hook_attributes"]["active"], true);
        assert!(m["default_events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e == "issues"));
    }

    #[test]
    fn app_names_are_cleaned_to_what_github_accepts() {
        assert_eq!(clean_name("  my app!! 2026 "), "my-app---2026");
        assert_eq!(clean_name("-x-"), "x");
        assert_eq!(clean_name(&"a".repeat(80)).len(), 34);
    }

    #[test]
    fn image_and_network_names_cannot_carry_options() {
        assert!(safe_ref("tasklane-sandbox") && safe_ref("ghcr.io/me/img:1.2") && safe_ref(""));
        for bad in ["--privileged", "a b", "a;b", "$(x)", "-v", "a\nb"] {
            assert!(!safe_ref(bad), "{bad}");
        }
    }
}
