//! Pull requests that are open in the repositories tasklane watches, whoever opened them (read only).
//! The list comes from the source plug-in; nothing here changes a pull request.

use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Response};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::Row;

use super::ui::{empty, head, render, repo_link};
use super::{guard, AppState};
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
    if let Err(r) = guard(&st, &headers).await {
        return r;
    }
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
    Html(list_html(&repos, skipped)).into_response()
}

fn list_html(repos: &[RepoPulls], skipped: usize) -> String {
    let total: usize = repos.iter().map(|r| r.pulls.len()).sum();
    let mut b = String::new();
    b.push_str("<div class=\"card flush\"><div class=\"scroll\"><table><thead><tr><th>Repository</th><th>Pull request</th><th>By</th><th>Branch</th><th>Updated</th></tr></thead><tbody>");
    if total == 0 {
        b.push_str(&format!(
            "<tr><td colspan=\"5\">{}</td></tr>",
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
            b.push_str(&format!(
                "<tr><td class=\"mono\">{}</td><td>{}</td><td>{}</td><td class=\"mono\">{} → {}</td><td class=\"muted\">{}</td></tr>",
                esc(&r.repo),
                link,
                who,
                esc(text(p, "branch")),
                esc(text(p, "base")),
                esc(updated.trim_end_matches('Z'))
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
