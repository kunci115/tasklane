//! Ask the AI: a conversation per person about any repository, answered read-only from its code and
//! notes, with the earlier messages carried along. A conversation can be turned into an issue that a
//! person edits and creates; the issue carries what the conversation established.

use std::sync::Arc;

use axum::extract::{Form, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use sqlx::Row;

use super::ui::{empty, head, icon, render};
use super::{csrf_ok, forbidden, guard, redirect, AppState, Session};
use crate::esc;
use crate::tickets::{create_ticket, CreateError, Created};

fn who(sess: &Session) -> String {
    format!("{}:{}", sess.channel, sess.reference)
}

#[derive(Deserialize)]
pub struct Pick {
    c: Option<i64>,
}

pub async fn page(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<Pick>,
) -> Response {
    let sess = match guard(&st, &headers).await {
        Ok(s) => s,
        Err(r) => return r,
    };
    match q.c {
        Some(c) => conversation(&st, &sess, c, "", None).await,
        None => start(&st, &sess).await,
    }
}

async fn start(st: &AppState, sess: &Session) -> Response {
    let mut b = head(
        "Ask the AI",
        "Learn how a repository works, think out loud about a feature, then turn the conversation into an issue. The AI only reads; it changes nothing.",
        "",
    );
    let repos =
        sqlx::query("SELECT id, external_id FROM services WHERE NOT archived ORDER BY external_id")
            .fetch_all(st.orch.pool())
            .await
            .unwrap_or_default();
    if repos.is_empty() {
        b.push_str(&empty(
            "No repositories yet",
            "Connect GitHub and let a repository in first.",
        ));
        return render(st, &sess.csrf, "ask", "Ask the AI", &b)
            .await
            .into_response();
    }
    let mut opts = String::new();
    for r in &repos {
        let id: i64 = r.get("id");
        let name: String = r.get("external_id");
        opts.push_str(&format!("<option value=\"{id}\">{}</option>", esc(&name)));
    }
    b.push_str(&format!(
        "<div class=\"card\"><form method=\"post\" action=\"/ask/new\"><input type=\"hidden\" name=\"csrf\" value=\"{}\"><div class=\"row\"><div><label for=\"rp\">Repository</label><select id=\"rp\" name=\"repo\">{opts}</select></div><div class=\"row tight\"><button class=\"btn primary\">{} New conversation</button></div></div></form></div>",
        sess.csrf,
        icon("chat")
    ));
    let mine = sqlx::query(
        "SELECT c.id, c.title, c.updated_at, s.external_id
         FROM conversations c JOIN services s ON s.id = c.service_id
         WHERE c.owner = $1 AND NOT s.archived ORDER BY c.updated_at DESC LIMIT 30",
    )
    .bind(who(sess))
    .fetch_all(st.orch.pool())
    .await
    .unwrap_or_default();
    if !mine.is_empty() {
        b.push_str("<div class=\"card flush\" style=\"margin-top:1rem\"><div class=\"scroll\"><table><thead><tr><th>Your conversations</th><th>Repository</th></tr></thead><tbody>");
        for r in mine {
            let id: i64 = r.get("id");
            let title: String = r.get("title");
            let repo: String = r.get("external_id");
            b.push_str(&format!(
                "<tr><td><a href=\"/ask?c={id}\">{}</a></td><td>{}</td></tr>",
                esc(if title.is_empty() {
                    "(no question yet)"
                } else {
                    &title
                }),
                esc(&repo)
            ));
        }
        b.push_str("</tbody></table></div></div>");
    }
    render(st, &sess.csrf, "ask", "Ask the AI", &b)
        .await
        .into_response()
}

/// The conversation page; `notice` is shown above it, `draft` (title, body) opens the issue form.
async fn conversation(
    st: &AppState,
    sess: &Session,
    conv: i64,
    notice: &str,
    draft: Option<(String, String)>,
) -> Response {
    let row = sqlx::query(
        "SELECT s.external_id FROM conversations c JOIN services s ON s.id = c.service_id
         WHERE c.id = $1 AND c.owner = $2 AND NOT s.archived",
    )
    .bind(conv)
    .bind(who(sess))
    .fetch_optional(st.orch.pool())
    .await
    .ok()
    .flatten();
    let Some(row) = row else {
        return redirect("/ask");
    };
    let repo: String = row.get("external_id");
    let msgs = sqlx::query(
        "SELECT role, body FROM conversation_messages WHERE conversation_id = $1 ORDER BY id",
    )
    .bind(conv)
    .fetch_all(st.orch.pool())
    .await
    .unwrap_or_default();
    let mut b = head(
        &format!("Ask the AI about {repo}"),
        "It reads the repository's code, the team's notes and documents. Earlier messages stay in view of the next answer.",
        "<a class=\"btn small\" href=\"/ask\">All conversations</a>",
    );
    if !notice.is_empty() {
        b.push_str(&format!("<div class=\"card\"><p>{}</p></div>", esc(notice)));
    }
    b.push_str("<div class=\"card\">");
    if msgs.is_empty() {
        b.push_str("<p class=\"muted\">Ask anything: how a part works, where something lives, what would have to change for a feature.</p>");
    }
    for m in &msgs {
        let role: String = m.get("role");
        let body: String = m.get("body");
        b.push_str(&format!(
            "<div style=\"margin:.7rem 0\"><div class=\"muted small\">{}</div><div style=\"white-space:pre-wrap\">{}</div></div>",
            if role == "person" { "You" } else { "AI" },
            esc(&body)
        ));
    }
    b.push_str(&format!(
        "<form method=\"post\" action=\"/ask/{conv}/say\" style=\"margin-top:1rem\"><input type=\"hidden\" name=\"csrf\" value=\"{}\"><div class=\"field\"><label for=\"q\">Your question</label><textarea id=\"q\" name=\"text\" rows=\"3\" maxlength=\"2000\" required></textarea></div><button class=\"btn primary\" data-confirm=\"\">{} Ask</button> <span class=\"hint\">The answer can take up to a minute.</span></form></div>",
        sess.csrf,
        icon("chat")
    ));
    if !msgs.is_empty() && draft.is_none() {
        b.push_str(&format!(
            "<div class=\"card\" style=\"margin-top:1rem\"><h2>Turn this into an issue</h2><p class=\"hint\">The AI writes a title, what is wanted, what the conversation found and acceptance criteria. You read and edit it before anything is created.</p><form method=\"post\" action=\"/ask/{conv}/draft\"><input type=\"hidden\" name=\"csrf\" value=\"{}\"><button class=\"btn\">{} Draft the issue</button></form></div>",
            sess.csrf,
            icon("plus")
        ));
    }
    if let Some((title, body)) = draft {
        b.push_str(&format!(
            "<div class=\"card\" style=\"margin-top:1rem\"><h2>Draft issue in {}</h2><form method=\"post\" action=\"/ask/{conv}/issue\"><input type=\"hidden\" name=\"csrf\" value=\"{}\"><div class=\"field\"><label for=\"it\">Title</label><input id=\"it\" type=\"text\" name=\"title\" maxlength=\"200\" required value=\"{}\"></div><div class=\"field\"><label for=\"ib\">Description</label><textarea id=\"ib\" name=\"body\" rows=\"16\" maxlength=\"20000\">{}</textarea></div><button class=\"btn primary\" data-confirm=\"Create this issue? It is triaged as soon as it is created.\">{} Create the issue</button></form></div>",
            esc(&repo),
            sess.csrf,
            esc(&title),
            esc(&body),
            icon("plus")
        ));
    }
    render(st, &sess.csrf, "ask", "Ask the AI", &b)
        .await
        .into_response()
}

#[derive(Deserialize)]
pub struct NewIn {
    csrf: String,
    repo: i64,
}

pub async fn new(
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
    match st.orch.conv_start(f.repo, &who(&sess)).await {
        Ok(id) => redirect(&format!("/ask?c={id}")),
        Err(_) => redirect("/ask"),
    }
}

#[derive(Deserialize)]
pub struct SayIn {
    csrf: String,
    #[serde(default)]
    text: String,
}

pub async fn say(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(conv): Path<i64>,
    Form(f): Form<SayIn>,
) -> Response {
    let sess = match guard(&st, &headers).await {
        Ok(s) => s,
        Err(r) => return r,
    };
    if !csrf_ok(&st, &headers, &f.csrf).await {
        return forbidden();
    }
    match st.orch.conv_say(conv, &who(&sess), &f.text).await {
        Ok(_) => redirect(&format!("/ask?c={conv}")),
        Err(e) => {
            eprintln!("ask: {e}");
            let msg = match e {
                tasklane_orchestrator::Error::Fatal(m) => m,
                _ => "The AI could not answer right now. Try again in a moment.".to_string(),
            };
            conversation(&st, &sess, conv, &msg, None).await
        }
    }
}

#[derive(Deserialize)]
pub struct Csrf {
    csrf: String,
}

pub async fn draft(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(conv): Path<i64>,
    Form(f): Form<Csrf>,
) -> Response {
    let sess = match guard(&st, &headers).await {
        Ok(s) => s,
        Err(r) => return r,
    };
    if !csrf_ok(&st, &headers, &f.csrf).await {
        return forbidden();
    }
    match st.orch.conv_draft(conv, &who(&sess)).await {
        Ok(d) => conversation(&st, &sess, conv, "", Some(d)).await,
        Err(e) => {
            eprintln!("ask: draft: {e}");
            conversation(
                &st,
                &sess,
                conv,
                "The draft could not be written right now. Try again in a moment.",
                None,
            )
            .await
        }
    }
}

#[derive(Deserialize)]
pub struct IssueIn {
    csrf: String,
    title: String,
    #[serde(default)]
    body: String,
}

pub async fn issue(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(conv): Path<i64>,
    Form(f): Form<IssueIn>,
) -> Response {
    let sess = match guard(&st, &headers).await {
        Ok(s) => s,
        Err(r) => return r,
    };
    if !csrf_ok(&st, &headers, &f.csrf).await {
        return forbidden();
    }
    let me = who(&sess);
    let repo: Option<String> = sqlx::query_scalar(
        "SELECT s.external_id FROM conversations c JOIN services s ON s.id = c.service_id
         WHERE c.id = $1 AND c.owner = $2 AND NOT s.archived",
    )
    .bind(conv)
    .bind(&me)
    .fetch_optional(st.orch.pool())
    .await
    .ok()
    .flatten();
    let Some(repo) = repo else {
        return (StatusCode::NOT_FOUND, "unknown conversation").into_response();
    };
    let via = format!("the console, after a conversation with the AI, by {me}");
    match create_ticket(
        &st.orch,
        &repo,
        &me,
        &f.title,
        &f.body,
        &via,
        st.user_daily_limit,
    )
    .await
    {
        Ok(Created::New(r)) => {
            let n = format!("Created issue #{r} in {repo}. It is being triaged.");
            conversation(&st, &sess, conv, &n, None).await
        }
        Ok(Created::Duplicate(r)) => {
            conversation(
                &st,
                &sess,
                conv,
                &format!("You already created this: #{r}."),
                None,
            )
            .await
        }
        Err(e) => {
            let msg = match e {
                CreateError::BadTitle => "The title needs 1 to 200 characters.",
                CreateError::BodyTooLong => "The description is too long.",
                CreateError::ReadOnlyService => {
                    "This repository is only being watched, so it takes no new issues."
                }
                CreateError::LimitReached => "You have reached your daily limit for new issues.",
                _ => "The issue could not be created right now.",
            };
            conversation(&st, &sess, conv, msg, None).await
        }
    }
}
