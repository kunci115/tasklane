//! Pull requests that are open in the repositories tasklane watches, whoever opened them (read only).
//! The list comes from the source plug-in; nothing here changes a pull request.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use axum::extract::{Form, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::Row;

use super::ui::{empty, head, render, repo_link};
use super::{csrf_ok, forbidden, guard, redirect, AppState};
use crate::esc;

const CALL: Duration = Duration::from_secs(30);
const FRESH: Duration = Duration::from_secs(60);
/// More repositories than this are not asked in one page load.
const MAX_REPOS: usize = 40;

#[derive(Deserialize, Default)]
pub struct PullsQuery {
    refresh: Option<String>,
}

#[derive(Clone)]
struct RepoPulls {
    repo: String,
    source: String,
    pulls: Vec<Value>,
    truncated: bool,
    error: Option<String>,
}

type Cache = Mutex<Option<(Instant, Vec<RepoPulls>)>>;

fn cache() -> &'static Cache {
    static C: OnceLock<Cache> = OnceLock::new();
    C.get_or_init(|| Mutex::new(None))
}

async fn load(st: &Arc<AppState>) -> (Vec<RepoPulls>, usize) {
    let rows = sqlx::query(
        "SELECT external_id, source FROM services WHERE NOT archived AND kind = 'repo' ORDER BY external_id",
    )
    .fetch_all(st.orch.pool())
    .await
    .unwrap_or_default();
    let skipped = rows.len().saturating_sub(MAX_REPOS);
    let mut set = tokio::task::JoinSet::new();
    for r in rows.into_iter().take(MAX_REPOS) {
        let (repo, source): (String, String) = (r.get("external_id"), r.get("source"));
        let st = st.clone();
        set.spawn(async move {
            let got = st
                .orch
                .source_call_with("list_pulls", json!({"service": repo}), CALL)
                .await;
            match got {
                Ok(v) => RepoPulls {
                    pulls: v
                        .get("pulls")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default(),
                    truncated: v.get("truncated").and_then(Value::as_bool).unwrap_or(false),
                    repo,
                    source,
                    error: None,
                },
                Err(e) => RepoPulls {
                    repo,
                    source,
                    pulls: vec![],
                    truncated: false,
                    error: Some(e.to_string()),
                },
            }
        });
    }
    let mut out = Vec::new();
    while let Some(Ok(r)) = set.join_next().await {
        out.push(r);
    }
    out.sort_by(|a, b| a.repo.cmp(&b.repo));
    (out, skipped)
}

fn text<'a>(p: &'a Value, k: &str) -> &'a str {
    p.get(k).and_then(Value::as_str).unwrap_or("")
}

/// The page opens at once; the list (which asks GitHub, repository by repository) fills in after.
pub async fn page(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<PullsQuery>,
) -> Response {
    let sess = match guard(&st, &headers).await {
        Ok(s) => s,
        Err(r) => return r,
    };
    if q.refresh.is_some() {
        if let Ok(mut c) = cache().lock() {
            *c = None;
        }
    }
    let mut b = head(
        "Pull requests",
        "Open pull requests in your repositories, whoever opened them. tasklane only reads them here.",
        "<a class=\"btn\" href=\"/pulls?refresh=1\">Refresh</a>",
    );
    b.push_str(&format!(
        "<div id=\"live\" data-live=\"/fragments/pulls\" data-every=\"30000\" data-now>{}</div>",
        empty(
            "repo",
            "Asking GitHub for open pull requests. This can take a few seconds."
        )
    ));
    render(&st, &sess.csrf, "pulls", "Pull requests", &b)
        .await
        .into_response()
}

pub async fn fragment(State(st): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let sess = match guard(&st, &headers).await {
        Ok(s) => s,
        Err(r) => return r,
    };
    let cached = cache().lock().ok().and_then(|c| {
        c.as_ref()
            .filter(|(t, _)| t.elapsed() < FRESH)
            .map(|(_, v)| v.clone())
    });
    let (repos, skipped) = match cached {
        Some(v) => (v, 0),
        None => {
            let (v, skipped) = load(&st).await;
            // a list with a failed repository is not kept: the next look asks again
            if v.iter().all(|r| r.error.is_none()) {
                if let Ok(mut c) = cache().lock() {
                    *c = Some((Instant::now(), v.clone()));
                }
            }
            (v, skipped)
        }
    };
    let verdicts = last_reviews(&st).await;
    let busy = reviewing().lock().map(|s| s.clone()).unwrap_or_default();
    Html(list_html(&repos, skipped, &sess.csrf, &verdicts, &busy)).into_response()
}

type Key = (String, String);

/// Reviews that are running right now, so a second press does not start a second one.
fn reviewing() -> &'static Mutex<HashSet<Key>> {
    static R: OnceLock<Mutex<HashSet<Key>>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(HashSet::new()))
}

/// The latest outcome per pull request: `(verdict or "failed", notes)`.
async fn last_reviews(st: &AppState) -> HashMap<Key, (String, String)> {
    let rows = sqlx::query(
        "SELECT DISTINCT ON (detail->>'service', detail->>'pr') action, detail
         FROM audit_events WHERE action IN ('pull.reviewed', 'pull.review_failed')
         ORDER BY detail->>'service', detail->>'pr', id DESC",
    )
    .fetch_all(st.orch.pool())
    .await
    .unwrap_or_default();
    rows.into_iter()
        .map(|r| {
            let d: Value = r.get("detail");
            let action: String = r.get("action");
            let (what, notes) = if action == "pull.reviewed" {
                (
                    text(&d, "verdict").to_string(),
                    text(&d, "notes").to_string(),
                )
            } else {
                ("failed".to_string(), text(&d, "error").to_string())
            };
            (
                (text(&d, "service").to_string(), text(&d, "pr").to_string()),
                (what, notes),
            )
        })
        .collect()
}

fn list_html(
    repos: &[RepoPulls],
    skipped: usize,
    csrf: &str,
    verdicts: &HashMap<Key, (String, String)>,
    busy: &HashSet<Key>,
) -> String {
    let total: usize = repos.iter().map(|r| r.pulls.len()).sum();
    let mut b = String::new();
    b.push_str("<div class=\"card flush\"><div class=\"scroll\"><table><thead><tr><th>Repository</th><th>Pull request</th><th>By</th><th>Branch</th><th>Updated</th><th>AI</th></tr></thead><tbody>");
    if total == 0 {
        b.push_str(&format!(
            "<tr><td colspan=\"6\">{}</td></tr>",
            empty(
                "repo",
                "No open pull requests in the repositories tasklane can see."
            )
        ));
    }
    for r in repos {
        let base = repo_link(&r.source, &r.repo);
        for p in &r.pulls {
            let num = text(p, "number");
            let url = {
                let u = text(p, "url");
                if u.starts_with("https://") {
                    u.to_string()
                } else if base.is_empty() {
                    String::new()
                } else {
                    format!("{base}/pull/{num}")
                }
            };
            let who = if p.get("ours").and_then(Value::as_bool) == Some(true) {
                "<span class=\"pill info\">tasklane</span>".to_string()
            } else if p.get("bot").and_then(Value::as_bool) == Some(true) {
                format!("<span class=\"pill\">bot</span> {}", esc(text(p, "author")))
            } else {
                esc(text(p, "author"))
            };
            let title = format!(
                "{}{}",
                if p.get("draft").and_then(Value::as_bool) == Some(true) {
                    "<span class=\"pill\">draft</span> "
                } else {
                    ""
                },
                esc(text(p, "title"))
            );
            let link = if url.is_empty() {
                format!("#{} {}", esc(num), title)
            } else {
                format!(
                    "<a href=\"{}\" target=\"_blank\" rel=\"noopener\">#{}</a> {}",
                    esc(&url),
                    esc(num),
                    title
                )
            };
            let updated = text(p, "updated_at").replace('T', " ");
            let key = (r.repo.clone(), num.to_string());
            let ai = if busy.contains(&key) {
                "<span class=\"pill info\">reviewing…</span>".to_string()
            } else {
                let last = match verdicts.get(&key) {
                    Some((v, notes)) => {
                        let (class, label) = match v.as_str() {
                            "approve" => ("ok", "no changes needed"),
                            "changes" => ("warn", "changes asked"),
                            "escalate" => ("warn", "needs a person"),
                            _ => ("bad", "failed"),
                        };
                        format!(
                            "<span class=\"pill {class}\" title=\"{}\">{label}</span> ",
                            esc(notes)
                        )
                    }
                    None => String::new(),
                };
                format!(
                    "{last}<form method=\"post\" action=\"/pulls/review\" style=\"display:inline\"><input type=\"hidden\" name=\"csrf\" value=\"{}\"><input type=\"hidden\" name=\"service\" value=\"{}\"><input type=\"hidden\" name=\"pr\" value=\"{}\"><button class=\"btn small\" data-confirm=\"Let the AI review this pull request? It reads the changes and posts its opinion as a comment on GitHub. It does not approve or merge.\">{}</button></form>",
                    esc(csrf),
                    esc(&r.repo),
                    esc(num),
                    if verdicts.contains_key(&key) { "Review again" } else { "Review with AI" }
                )
            };
            b.push_str(&format!(
                "<tr><td class=\"mono\">{}</td><td>{}</td><td>{}</td><td class=\"mono\">{} → {}</td><td class=\"muted\">{}</td><td>{}</td></tr>",
                esc(&r.repo),
                link,
                who,
                esc(text(p, "branch")),
                esc(text(p, "base")),
                esc(updated.trim_end_matches('Z')),
                ai
            ));
        }
    }
    b.push_str("</tbody></table></div></div>");
    for r in repos {
        if let Some(e) = &r.error {
            b.push_str(&format!(
                "<p class=\"muted small\"><span class=\"pill warn\">unavailable</span> {}: {}</p>",
                esc(&r.repo),
                esc(e)
            ));
        } else if r.truncated {
            b.push_str(&format!(
                "<p class=\"muted small\">{} has more open pull requests than are listed here.</p>",
                esc(&r.repo)
            ));
        }
    }
    if skipped > 0 {
        b.push_str(&format!(
            "<p class=\"muted small\">{skipped} more repositories were not asked; archive the ones you do not need.</p>"
        ));
    }
    b
}

#[derive(Deserialize)]
pub struct ReviewIn {
    csrf: String,
    service: String,
    pr: String,
}

/// Start a read-only AI review in the background; the list shows it as running, then its verdict.
pub async fn review(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Form(f): Form<ReviewIn>,
) -> Response {
    if !csrf_ok(&st, &headers, &f.csrf).await {
        return forbidden();
    }
    if f.pr.is_empty() || f.pr.len() > 9 || !f.pr.bytes().all(|b| b.is_ascii_digit()) {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            "A pull request number is digits only.",
        )
            .into_response();
    }
    let known: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM services WHERE external_id = $1 AND NOT archived AND mode <> 'watch'",
    )
    .bind(&f.service)
    .fetch_one(st.orch.pool())
    .await
    .unwrap_or(0);
    if known == 0 {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            "That repository is unknown or in watch mode. Set it to triage or full on the Repositories page first.",
        )
            .into_response();
    }
    let key = (f.service.clone(), f.pr.clone());
    let fresh = reviewing()
        .lock()
        .map(|mut s| s.insert(key.clone()))
        .unwrap_or(false);
    if fresh {
        let (st2, service, pr) = (st.clone(), f.service, f.pr);
        tokio::spawn(async move {
            let done = st2.orch.review_pull(&service, &pr, "console:admin").await;
            if let Err(e) = done {
                let _ = sqlx::query("INSERT INTO audit_events (actor, action, detail) VALUES ('console:admin', 'pull.review_failed', $1)")
                    .bind(json!({"service": service, "pr": pr, "error": e.to_string().chars().take(300).collect::<String>()}))
                    .execute(st2.orch.pool())
                    .await;
            }
            if let Ok(mut s) = reviewing().lock() {
                s.remove(&key);
            }
        });
    }
    redirect("/pulls")
}
