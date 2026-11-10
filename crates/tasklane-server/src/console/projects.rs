//! Starting a project from nothing: a new repository, seeded, registered and given a first ticket,
//! so the path from an idea to a plan waiting for approval is one form.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Form, Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{json, Value};

use super::ui::{head, render};
use super::{csrf_ok, forbidden, guard, AppState};
use crate::connections::{self, GithubConn};
use crate::esc;
use crate::tickets::{create_ticket, Created};

const CALL: Duration = Duration::from_secs(60);

#[derive(Deserialize, Default)]
pub struct FormQuery {
    owner: Option<String>,
}

fn field(label: &str, id: &str, control: &str, hint: &str) -> String {
    format!(
        "<div class=\"field\"><label for=\"{id}\">{label}</label>{control}{}</div>",
        if hint.is_empty() {
            String::new()
        } else {
            format!("<div class=\"hint\">{hint}</div>")
        }
    )
}

async fn form_html(st: &AppState, csrf: &str, owner: &str, notice: &str) -> String {
    let conn: Option<GithubConn> = connections::get(st.orch.pool(), "github").await;
    let known = st
        .orch
        .source_call_with("list_installations", json!({}), Duration::from_secs(15))
        .await
        .ok();
    let accounts: Vec<String> = known
        .as_ref()
        .and_then(|v| v.get("installations"))
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|i| i.get("account").and_then(Value::as_str).map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let default_owner = if owner.is_empty() {
        accounts
            .iter()
            .find(|a| {
                conn.as_ref()
                    .is_some_and(|c| c.owner_type == "Organization" && &c.owner == *a)
            })
            .or_else(|| accounts.first())
            .cloned()
            .unwrap_or_default()
    } else {
        owner.to_string()
    };
    let options: String = accounts
        .iter()
        .map(|a| format!("<option value=\"{}\">", esc(a)))
        .collect();
    let mut b = head(
        "New project",
        "Start from nothing: tasklane creates the repository, adds the basics, registers it and opens a first ticket from your brief. The plan then waits for your approval.",
        "",
    );
    b.push_str(notice);
    b.push_str(&format!(
        "<div class=\"grid g2\"><div class=\"card\"><form method=\"post\" action=\"/projects/new\"><input type=\"hidden\" name=\"csrf\" value=\"{}\">{}{}{}{}{}{}\
<button class=\"btn primary\">Create the project</button> <button class=\"btn\" formaction=\"/projects/plan\" title=\"The AI asks what it needs, then proposes a list of tickets. Nothing is created until you confirm.\">Plan it with the AI first</button></form></div>\
<div class=\"card\"><h2>What happens</h2><ol class=\"small\" style=\"padding-left:1.2rem;margin:0\">\
<li>The repository is created in the organization you name, private unless you choose otherwise.</li>\
<li>It gets a README, a <span class=\"mono\">.gitignore</span> for the template, and a <span class=\"mono\">.tasklane.yml</span> that keeps CI files behind a person.</li>\
<li>It is registered here in <strong>triage</strong>: tasklane reads and plans, and changes nothing until you approve.</li>\
<li>Your brief becomes the first ticket. Find its plan under <a href=\"/tickets\">Tickets</a>.</li></ol>\
<p class=\"hint\">Needs the GitHub App to be installed on the organization <strong>with all repositories</strong> (a new repository is then included by itself) and to have the <strong>Administration</strong> permission (write). tasklane does not ask for that by default; if GitHub refuses, the message says what to change. A personal account cannot be used: GitHub Apps cannot create repositories there.</p></div></div>",
        esc(csrf),
        field("Organization", "owner", &format!("<input id=\"owner\" type=\"text\" name=\"owner\" list=\"owners\" value=\"{}\" maxlength=\"39\" required autocomplete=\"off\"><datalist id=\"owners\">{options}</datalist>", esc(&default_owner)), "The organization the App is installed on."),
        field("Repository name", "name", "<input id=\"name\" type=\"text\" name=\"name\" maxlength=\"100\" pattern=\"[A-Za-z0-9_.\\-]+\" placeholder=\"my-new-app\" required>", "Letters, numbers, dots, hyphens and underscores."),
        field("One-line description", "description", "<input id=\"description\" type=\"text\" name=\"description\" maxlength=\"350\">", ""),
        field("Template", "template", "<select id=\"template\" name=\"template\"><option value=\"blank\">Blank</option><option value=\"python\">Python (.gitignore)</option><option value=\"node\">Node (.gitignore)</option></select>", ""),
        field("Who can see it", "visibility", "<select id=\"visibility\" name=\"visibility\"><option value=\"private\">Private (recommended)</option><option value=\"public\">Public</option></select>", ""),
        field("Brief for the first ticket (optional)", "brief", "<textarea id=\"brief\" name=\"brief\" rows=\"7\" maxlength=\"4000\" placeholder=\"What should this project do? Who uses it? Any stack you want?\"></textarea>", "Leave it empty to create only the repository."),
    ));
    b
}

pub async fn page(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<FormQuery>,
) -> Response {
    let sess = match guard(&st, &headers).await {
        Ok(s) => s,
        Err(r) => return r,
    };
    let b = form_html(&st, &sess.csrf, q.owner.as_deref().unwrap_or(""), "").await;
    render(&st, &sess.csrf, "projects", "New project", &b)
        .await
        .into_response()
}

#[derive(Deserialize)]
pub struct NewIn {
    csrf: String,
    owner: String,
    name: String,
    description: Option<String>,
    template: Option<String>,
    visibility: Option<String>,
    brief: Option<String>,
    /// Questions, answers and the proposed tickets of the plan step (`q0`, `a0`, `t0_on`, `t0_title`, ...).
    #[serde(flatten)]
    extra: std::collections::HashMap<String, String>,
}

/// What is wrong with the fields of the form, if anything.
fn check_fields(owner: &str, name: &str, template: &str, brief: &str) -> Option<&'static str> {
    let owner_ok = !owner.is_empty()
        && owner.len() <= 39
        && owner
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-');
    let name_ok = !name.is_empty()
        && name.len() <= 100
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        && name != "."
        && name != ".."
        && !name.ends_with(".git");
    if !owner_ok {
        Some("The organization is only letters, numbers and hyphens.")
    } else if !name_ok {
        Some("The repository name is letters, numbers, dots, hyphens and underscores.")
    } else if !matches!(template, "blank" | "python" | "node") {
        Some("Choose one of the templates.")
    } else if brief.chars().count() > 4000 {
        Some("The brief is longer than 4000 characters.")
    } else {
        None
    }
}

/// The tickets the person left ticked on the plan page: up to eight (title, body).
fn planned_tickets(extra: &std::collections::HashMap<String, String>) -> Vec<(String, String)> {
    (0..8)
        .filter(|i| extra.get(&format!("t{i}_on")).map(String::as_str) == Some("on"))
        .filter_map(|i| {
            let title: String = extra
                .get(&format!("t{i}_title"))?
                .trim()
                .chars()
                .take(120)
                .collect();
            let body: String = extra
                .get(&format!("t{i}_body"))
                .map(|b| b.trim().chars().take(3000).collect())
                .unwrap_or_default();
            (!title.is_empty()).then_some((title, body))
        })
        .collect()
}

/// Questions the AI asked earlier and the answers given, from the hidden fields of the plan page.
fn answered(extra: &std::collections::HashMap<String, String>) -> Vec<(String, String)> {
    (0..20)
        .filter_map(|i| {
            let q = extra.get(&format!("q{i}"))?.trim().to_string();
            let a = extra.get(&format!("a{i}"))?.trim().to_string();
            (!q.is_empty() && !a.is_empty()).then(|| {
                (
                    q.chars().take(300).collect(),
                    a.chars().take(1000).collect(),
                )
            })
        })
        .collect()
}

fn hidden(name: &str, value: &str) -> String {
    format!(
        "<input type=\"hidden\" name=\"{}\" value=\"{}\">",
        esc(name),
        esc(value)
    )
}

/// Step two: the AI asks what it needs, then proposes tickets. Nothing is created here.
pub async fn plan(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Form(f): Form<NewIn>,
) -> Response {
    let sess = match guard(&st, &headers).await {
        Ok(s) => s,
        Err(r) => return r,
    };
    if !csrf_ok(&st, &headers, &f.csrf).await {
        return forbidden();
    }
    let owner = f.owner.trim().to_string();
    let name = f.name.trim().to_string();
    let brief = f.brief.as_deref().unwrap_or("").trim().to_string();
    let template = f.template.as_deref().unwrap_or("blank").to_string();
    let problem = if brief.is_empty() {
        Some("Write a brief first: the AI plans from it.")
    } else {
        check_fields(&owner, &name, &template, &brief)
    };
    if let Some(p) = problem {
        let b = form_html(&st, &sess.csrf, &owner, &bad(p)).await;
        return (
            axum::http::StatusCode::UNPROCESSABLE_ENTITY,
            render(&st, &sess.csrf, "projects", "New project", &b).await,
        )
            .into_response();
    }
    let qa = answered(&f.extra);
    let comments: Vec<Value> = qa
        .iter()
        .map(|(q, a)| json!({"author": "you", "body": format!("Question: {q}\nAnswer: {a}")}))
        .collect();
    let out = st
        .orch
        .engine_call_with(
            "run",
            json!({
                "role": "planner",
                "ticket": {"service": "", "ref": "", "title": name, "body": brief, "comments": comments},
                "profile": {},
                "references": st.orch.references_for("").await,
                "budget": {"max_secs": 150, "max_tokens": 60000},
                "read_only": true
            }),
            Duration::from_secs(180),
        )
        .await;
    let out = match out {
        Ok(v) => v,
        Err(e) => {
            let msg: String = format!("The AI could not plan: {e}")
                .chars()
                .take(400)
                .collect();
            let b = form_html(&st, &sess.csrf, &owner, &bad(&msg)).await;
            return (
                axum::http::StatusCode::BAD_GATEWAY,
                render(&st, &sess.csrf, "projects", "New project", &b).await,
            )
                .into_response();
        }
    };
    let text = |k: &str| out.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let questions: Vec<String> = out
        .get("questions")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let tickets: Vec<(String, String)> = out
        .get("tickets")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .take(8)
                .map(|t| {
                    let g = |k: &str| t.get(k).and_then(Value::as_str).unwrap_or("").to_string();
                    (g("title"), g("body"))
                })
                .collect()
        })
        .unwrap_or_default();

    let mut carry = hidden("csrf", &sess.csrf)
        + &hidden("owner", &owner)
        + &hidden("name", &name)
        + &hidden("description", f.description.as_deref().unwrap_or(""))
        + &hidden("template", &template)
        + &hidden("visibility", f.visibility.as_deref().unwrap_or("private"))
        + &hidden("brief", &brief);
    let mut b = head(
        "New project",
        "The AI proposes a plan. Change anything you like; nothing exists until you create it.",
        "",
    );
    let mut prior = String::new();
    for (i, (q, a)) in qa.iter().enumerate() {
        carry.push_str(&hidden(&format!("q{i}"), q));
        carry.push_str(&hidden(&format!("a{i}"), a));
        prior.push_str(&format!(
            "<li><strong>{}</strong><br>{}</li>",
            esc(q),
            esc(a)
        ));
    }
    let mut body = String::new();
    if !text("summary").is_empty() {
        body.push_str(&format!("<p>{}</p>", esc(&text("summary"))));
    }
    if !prior.is_empty() {
        body.push_str(&format!(
            "<h3>Already answered</h3><ul class=\"small\">{prior}</ul>"
        ));
    }
    if !questions.is_empty() {
        body.push_str("<h3>Before it can plan, it asks</h3>");
        for (j, q) in questions.iter().enumerate() {
            let i = qa.len() + j;
            carry.push_str(&hidden(&format!("q{i}"), q));
            body.push_str(&format!(
                "<div class=\"field\"><label for=\"a{i}\">{}</label><input id=\"a{i}\" type=\"text\" name=\"a{i}\" maxlength=\"1000\"></div>",
                esc(q)
            ));
        }
        body.push_str("<p class=\"hint\">Leave a question empty to let the AI assume something reasonable.</p>");
        body.push_str("<button class=\"btn primary\" formaction=\"/projects/plan\">Answer and continue</button>");
    } else {
        body.push_str("<h3>Proposed tickets</h3><p class=\"hint\">Each becomes an issue in the new repository and is planned by the AI; nothing is changed until you approve it. Untick what you do not want.</p>");
        for (i, (title, tbody)) in tickets.iter().enumerate() {
            body.push_str(&format!(
                "<div class=\"card\" style=\"margin:.6rem 0\"><label class=\"row tight\" style=\"gap:.5rem;align-items:center\"><input type=\"checkbox\" name=\"t{i}_on\" value=\"on\" checked> <input type=\"text\" name=\"t{i}_title\" value=\"{}\" maxlength=\"120\" style=\"flex:1\" aria-label=\"Title\"></label><textarea name=\"t{i}_body\" rows=\"3\" maxlength=\"3000\" aria-label=\"What to build\">{}</textarea></div>",
                esc(title),
                esc(tbody)
            ));
        }
        body.push_str(&format!(
            "<button class=\"btn primary\" formaction=\"/projects/new\">Create the repository and {} ticket(s)</button> <button class=\"btn\" formaction=\"/projects/plan\" title=\"Plan again with the same answers\">Plan again</button> ",
            tickets.len()
        ));
    }
    b.push_str(&format!(
        "<div class=\"card\"><p class=\"small muted\">{} / {} ({} template, {}).</p><form method=\"post\" action=\"/projects/plan\">{carry}{body}</form></div>",
        esc(&owner),
        esc(&name),
        esc(&template),
        if f.visibility.as_deref() == Some("public") { "public" } else { "private" }
    ));
    render(&st, &sess.csrf, "projects", "New project", &b)
        .await
        .into_response()
}

fn bad(msg: &str) -> String {
    format!(
        "<div class=\"notice warn\"><strong>Not created.</strong> {}</div>",
        esc(msg)
    )
}

pub async fn create(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Form(f): Form<NewIn>,
) -> Response {
    let sess = match guard(&st, &headers).await {
        Ok(s) => s,
        Err(r) => return r,
    };
    if !csrf_ok(&st, &headers, &f.csrf).await {
        return forbidden();
    }
    let owner = f.owner.trim().to_string();
    let name = f.name.trim().to_string();
    let description: String = f
        .description
        .as_deref()
        .unwrap_or("")
        .trim()
        .chars()
        .take(350)
        .collect();
    let brief = f.brief.as_deref().unwrap_or("").trim().to_string();
    let template = f.template.as_deref().unwrap_or("blank");
    let problem = check_fields(&owner, &name, template, &brief);
    if let Some(p) = problem {
        let b = form_html(&st, &sess.csrf, &owner, &bad(p)).await;
        return (
            axum::http::StatusCode::UNPROCESSABLE_ENTITY,
            render(&st, &sess.csrf, "projects", "New project", &b).await,
        )
            .into_response();
    }
    let private = f.visibility.as_deref() != Some("public");
    let made = st
        .orch
        .source_call_with(
            "create_repo",
            json!({"owner": owner, "name": name, "description": description, "private": private, "template": template}),
            CALL,
        )
        .await;
    let made = match made {
        Ok(v) => v,
        Err(e) => {
            let msg: String = e.to_string().chars().take(400).collect();
            let b = form_html(&st, &sess.csrf, &owner, &bad(&msg)).await;
            return (
                axum::http::StatusCode::UNPROCESSABLE_ENTITY,
                render(&st, &sess.csrf, "projects", "New project", &b).await,
            )
                .into_response();
        }
    };
    let text = |k: &str| {
        made.get(k)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    let full = text("external_id");
    let url = text("html_url");
    let seed_error = text("seed_error");
    let seeded: Vec<String> = made
        .get("seeded")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    // register it now (the sync would find it within a minute); a repository we just made starts in triage
    let _ = sqlx::query(
        "INSERT INTO services (kind, source, external_id, mode, facts) VALUES ('repo', $1, $2, 'triage', $3)
         ON CONFLICT (source, external_id) DO UPDATE SET archived = false",
    )
    .bind(&st.orch.config().source_name)
    .bind(&full)
    .bind(json!({"default_branch": text("default_branch"), "private": private}))
    .execute(st.orch.pool())
    .await;
    let planned = planned_tickets(&f.extra);
    let _ = sqlx::query("INSERT INTO audit_events (actor, action, detail) VALUES ('console:admin', 'project.created', $1)")
        .bind(json!({"service": full, "template": template, "private": private, "first_ticket": !brief.is_empty(), "planned": planned.len()}))
        .execute(st.orch.pool())
        .await;
    let mut ticket_note = String::new();
    if !planned.is_empty() {
        let mut items = String::new();
        let mut failed = 0;
        for (title, body) in &planned {
            match create_ticket(
                &st.orch,
                &full,
                "console:admin",
                title,
                body,
                "the tasklane console (New project plan)",
                1000,
            )
            .await
            {
                Ok(Created::New(r) | Created::Duplicate(r)) => {
                    items.push_str(&format!("<li>#{} {}</li>", esc(&r), esc(title)));
                }
                Err(_) => failed += 1,
            }
        }
        ticket_note = format!(
            "<p>Tickets opened from the plan (each waits for your approval under <a href=\"/tickets\">Tickets</a>):</p><ul>{items}</ul>{}",
            if failed > 0 {
                format!("<p class=\"muted\">{failed} could not be opened. The App probably cannot see the new repository yet: add it to the App's installation at <a href=\"https://github.com/organizations/{}/settings/installations\" target=\"_blank\" rel=\"noopener\">GitHub</a>, or choose <strong>All repositories</strong> there.</p>", esc(&owner))
            } else {
                String::new()
            }
        );
    } else if !brief.is_empty() {
        let title = format!("Start the project: {name}");
        match create_ticket(
            &st.orch,
            &full,
            "console:admin",
            &title,
            &brief,
            "the tasklane console (New project)",
            1000,
        )
        .await
        {
            Ok(Created::New(r) | Created::Duplicate(r)) => {
                let id: Option<i64> = sqlx::query_scalar("SELECT t.id FROM tickets t JOIN services s ON s.id = t.service_id WHERE s.external_id = $1 AND t.external_ref = $2")
                    .bind(&full)
                    .bind(&r)
                    .fetch_optional(st.orch.pool())
                    .await
                    .ok()
                    .flatten();
                ticket_note = match id {
                    Some(id) => format!("<p>First ticket: <a href=\"/tickets/{id}\">#{} {}</a>. tasklane reads the brief, writes a plan and waits for your approval.</p>", esc(&r), esc(&title)),
                    None => format!("<p>First ticket #{} was opened on GitHub.</p>", esc(&r)),
                };
            }
            Err(_) => {
                ticket_note = format!("<p class=\"muted\">The repository exists, but the first ticket could not be opened. The App probably cannot see the new repository yet: add it to the App's installation at <a href=\"https://github.com/organizations/{}/settings/installations\" target=\"_blank\" rel=\"noopener\">GitHub</a>, or choose <strong>All repositories</strong> there.</p>", esc(&owner));
            }
        }
    }
    let mut b = head(
        "New project",
        "",
        "<a class=\"btn\" href=\"/projects/new\">Start another</a>",
    );
    b.push_str(&format!(
        "<div class=\"card\"><p><span class=\"pill ok\">created</span> <a href=\"{}\" target=\"_blank\" rel=\"noopener\"><strong>{}</strong></a> ({}). Registered here in triage mode.</p><p class=\"small muted\">Added: {}.{}</p>{ticket_note}<p><a href=\"/services\">Repositories</a> · <a href=\"/tickets\">Tickets</a></p></div>",
        esc(&url),
        esc(&full),
        if private { "private" } else { "public" },
        if seeded.is_empty() { "nothing yet".to_string() } else { esc(&seeded.join(", ")) },
        if seed_error.is_empty() { String::new() } else { format!(" <span class=\"pill warn\">some files missing</span> {}", esc(&seed_error)) }
    ));
    render(&st, &sess.csrf, "projects", "New project", &b)
        .await
        .into_response()
}
