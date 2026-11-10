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

use super::ui::{empty, head, render, repo_link, url_encode};
use super::{csrf_ok, forbidden, guard, redirect, AppState};
use crate::esc;

const CALL: Duration = Duration::from_secs(30);
const FRESH: Duration = Duration::from_secs(60);
/// More repositories than this are not asked in one page load.
const MAX_REPOS: usize = 40;

#[derive(Deserialize, Default)]
pub struct PullsQuery {
    refresh: Option<String>,
    done: Option<String>,
    err: Option<String>,
}

#[derive(Clone)]
struct RepoPulls {
    repo: String,
    source: String,
    pulls: Vec<Value>,
    truncated: bool,
    error: Option<String>,
}

/// One list per running server (the address of its state tells them apart).
type Cache = Mutex<HashMap<usize, (Instant, Vec<RepoPulls>)>>;

fn cache() -> &'static Cache {
    static C: OnceLock<Cache> = OnceLock::new();
    C.get_or_init(|| Mutex::new(HashMap::new()))
}

fn cache_key(st: &Arc<AppState>) -> usize {
    Arc::as_ptr(st) as usize
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
            c.remove(&cache_key(&st));
        }
    }
    let mut b = head(
        "Pull requests",
        "Open pull requests in your repositories, whoever opened them. tasklane only reads them here.",
        "<a class=\"btn\" href=\"/pulls?refresh=1\">Refresh</a>",
    );
    if let Some(d) = q.done.as_deref().filter(|d| d.starts_with("merged-")) {
        b.push_str(&format!(
            "<div class=\"notice ok\">Pull request #{} was merged.</div>",
            esc(&d["merged-".len()..])
        ));
    }
    if let Some(e) = q.err.as_deref() {
        b.push_str(&format!(
            "<div class=\"notice warn\"><strong>Not merged.</strong> {}</div>",
            esc(&e.chars().take(300).collect::<String>())
        ));
    }
    b.push_str(&format!(
        "<div id=\"live\" data-live=\"/fragments/pulls\" data-every=\"30000\" data-now>{}</div>",
        empty(
            "repo",
            "Asking GitHub for open pull requests. This can take a few seconds."
        )
    ));
    b.push_str(&switches_html(&st, &sess.csrf).await);
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
        c.get(&cache_key(&st))
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
                    c.insert(cache_key(&st), (Instant::now(), v.clone()));
                }
            }
            (v, skipped)
        }
    };
    let verdicts = last_reviews(&st).await;
    let rules = merge_rules(&st).await;
    let busy = busy_for(&st);
    Html(list_html(
        &repos, skipped, &sess.csrf, &verdicts, &rules, &busy,
    ))
    .into_response()
}

type Key = (String, String);
/// What the AI said about a pull request: verdict (or `failed`), its notes, the commit it looked at.
type Review = (String, String, String);
/// Per repository: is it in full mode, is merging from here switched on, and which branch is the default one.
type MergeRules = HashMap<String, (bool, bool, String)>;

/// Reviews that are running right now, so a second press does not start a second one. Each entry
/// belongs to one running server (the address of its state), like the list cache.
fn reviewing() -> &'static Mutex<HashSet<(usize, String, String)>> {
    static R: OnceLock<Mutex<HashSet<(usize, String, String)>>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(HashSet::new()))
}

fn busy_for(st: &Arc<AppState>) -> HashSet<Key> {
    let me = cache_key(st);
    reviewing()
        .lock()
        .map(|s| {
            s.iter()
                .filter(|(k, _, _)| *k == me)
                .map(|(_, a, b)| (a.clone(), b.clone()))
                .collect()
        })
        .unwrap_or_default()
}

/// The reviews running right now, as the live map lists work in progress (they have no ticket).
pub(super) fn busy_reviews(st: &Arc<AppState>) -> Vec<Value> {
    busy_json(busy_for(st).into_iter().collect())
}

fn busy_json(mut v: Vec<Key>) -> Vec<Value> {
    v.sort();
    v.into_iter()
        .map(|(repo, pr)| json!({"agent": "review", "kind": "pull_review", "ticket": null, "pull": format!("{repo}#{pr}"), "title": format!("PR #{pr} in {repo}")}))
        .collect()
}

/// The open pull requests for the live map, from the page's own cache. The map polls every few
/// seconds and must never ask GitHub itself: when the list is missing or old, one background load
/// is started and the next poll gets it.
pub(super) fn map_pulls(st: &Arc<AppState>) -> Option<Vec<Value>> {
    // one background load at a time per running server
    static LOADING: OnceLock<Mutex<HashSet<usize>>> = OnceLock::new();
    let loading = LOADING.get_or_init(|| Mutex::new(HashSet::new()));
    let cached = cache().lock().ok().and_then(|c| {
        c.get(&cache_key(st))
            .map(|(t, v)| (t.elapsed() < FRESH, v.clone()))
    });
    if !matches!(cached, Some((true, _)))
        && loading
            .lock()
            .map(|mut l| l.insert(cache_key(st)))
            .unwrap_or(false)
    {
        let st = st.clone();
        tokio::spawn(async move {
            let (v, _) = load(&st).await;
            if v.iter().all(|r| r.error.is_none()) {
                if let Ok(mut c) = cache().lock() {
                    c.insert(cache_key(&st), (Instant::now(), v));
                }
            }
            if let Ok(mut l) = LOADING.get_or_init(|| Mutex::new(HashSet::new())).lock() {
                l.remove(&cache_key(&st));
            }
        });
    }
    cached.map(|(_, repos)| {
        repos
            .into_iter()
            .flat_map(|r| {
                let repo = r.repo.clone();
                r.pulls.into_iter().map(move |mut p| {
                    p["repo_name"] = json!(repo);
                    p
                })
            })
            .collect()
    })
}

/// What the map shows for each open pull request: where it is in its review, and which ticket
/// (if tasklane opened it) it belongs to. `repo_ids` maps a repository name to its id on the map.
pub(super) async fn map_state(st: &Arc<AppState>, repo_ids: &HashMap<String, i64>) -> Vec<Value> {
    let Some(list) = map_pulls(st) else {
        return vec![];
    };
    let verdicts = last_reviews(st).await;
    let busy = busy_for(st);
    list.into_iter()
        .take(120)
        .filter_map(|p| {
            let repo = text(&p, "repo_name").to_string();
            let id = *repo_ids.get(&repo)?;
            let num = text(&p, "number").to_string();
            let key = (repo.clone(), num.clone());
            let state = if busy.contains(&key) {
                "reviewing".to_string()
            } else {
                verdicts.get(&key).map(|v| v.0.clone()).unwrap_or_else(|| "open".into())
            };
            let ticket = text(&p, "branch")
                .strip_prefix("tasklane/")
                .and_then(|t| t.parse::<i64>().ok());
            Some(json!({
                "key": format!("{repo}#{num}"), "repo": id, "num": num, "title": text(&p, "title"),
                "author": text(&p, "author"), "ours": p.get("ours").and_then(Value::as_bool).unwrap_or(false),
                "ticket": ticket, "state": state, "url": text(&p, "url"),
            }))
        })
        .collect()
}

/// The latest outcome per pull request: `(verdict or "failed", notes)`.
async fn last_reviews(st: &AppState) -> HashMap<Key, Review> {
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
                let cx = text(&d, "complexity");
                (
                    text(&d, "verdict").to_string(),
                    if cx.is_empty() {
                        text(&d, "notes").to_string()
                    } else {
                        format!("{}\n\nComplexity (Big-O): {cx}", text(&d, "notes"))
                    },
                )
            } else {
                ("failed".to_string(), text(&d, "error").to_string())
            };
            let head = text(&d, "head_sha").to_string();
            (
                (text(&d, "service").to_string(), text(&d, "pr").to_string()),
                (what, notes, head),
            )
        })
        .collect()
}

fn list_html(
    repos: &[RepoPulls],
    skipped: usize,
    csrf: &str,
    verdicts: &HashMap<Key, Review>,
    rules: &MergeRules,
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
                    Some((v, notes, _)) => {
                        let (class, label, next) = match v.as_str() {
                            "approve" => ("ok", "no changes needed", "Nothing to fix. A person can merge it (the Merge button appears when this repository allows it)."),
                            "changes" => ("warn", "changes asked", "Ask the author to fix what is listed, then press Review again."),
                            "escalate" => ("warn", "needs a person", "A person should look at it first; the details are in the comment on the pull request."),
                            _ => ("bad", "failed", "The review did not finish. Press Review again."),
                        };
                        format!(
                            "<details><summary><span class=\"pill {class}\">{label}</span> <span class=\"muted small\">read the review</span></summary><p class=\"small\"><strong>Next:</strong> {next}</p><div class=\"small\" style=\"white-space:pre-wrap;max-width:46rem\">{}</div></details>",
                            esc(notes)
                        )
                    }
                    None => String::new(),
                };
                let (full, on, default) = rules
                    .get(&r.repo)
                    .map(|(f, o, d)| (*f, *o, d.as_str()))
                    .unwrap_or((false, false, "main"));
                let allowed = full && on;
                let merge = match verdicts.get(&key) {
                    Some((v, _, head))
                        if v == "approve"
                            && allowed
                            && !head.is_empty()
                            && head == text(p, "head_sha")
                            && text(p, "base") != default =>
                    {
                        format!(
                            "<form method=\"post\" action=\"/pulls/merge\" style=\"display:inline\"><input type=\"hidden\" name=\"csrf\" value=\"{}\"><input type=\"hidden\" name=\"service\" value=\"{}\"><input type=\"hidden\" name=\"pr\" value=\"{}\"><button class=\"btn small primary\" data-confirm=\"Merge #{} into {}? The AI approved commit {}. GitHub merges only if the pull request still ends at that commit and its checks pass.\">Merge</button></form> ",
                            esc(csrf),
                            esc(&r.repo),
                            esc(num),
                            esc(num),
                            esc(text(p, "base")),
                            esc(&head.chars().take(7).collect::<String>())
                        )
                    }
                    Some((v, _, head)) if v == "approve" && head != text(p, "head_sha") => {
                        "<span class=\"pill warn\">changed since review</span> ".to_string()
                    }
                    // approved, but the Merge button is held back: say why and where to change it
                    Some((v, _, _)) if v == "approve" => {
                        let why = if text(p, "base") == default {
                            format!(
                                "it goes into {}, the default branch: merge it on GitHub",
                                esc(default)
                            )
                        } else if !full {
                            "this repository is not in full mode: set it on <a href=\"/services\">Repositories</a>, then switch merging on below".to_string()
                        } else {
                            "merging from here is off for this repository: <a href=\"#merging\">switch it on below</a>".to_string()
                        };
                        format!("<div class=\"muted small\" style=\"margin:.3rem 0\"><strong>No Merge button:</strong> {why}.</div>")
                    }
                    _ => String::new(),
                };
                format!(
                    "{last}{merge}<form method=\"post\" action=\"/pulls/review\" style=\"display:inline\"><input type=\"hidden\" name=\"csrf\" value=\"{}\"><input type=\"hidden\" name=\"service\" value=\"{}\"><input type=\"hidden\" name=\"pr\" value=\"{}\"><button class=\"btn small\" data-confirm=\"Let the AI review this pull request? It reads the changes and posts its opinion as a comment on GitHub. It does not approve or merge.\">{}</button></form>",
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
    let key = (cache_key(&st), f.service.clone(), f.pr.clone());
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

/// For each watched repository: is merging from the console switched on (and the repository in full
/// mode), and which branch is its default one.
async fn merge_rules(st: &AppState) -> MergeRules {
    sqlx::query(
        "SELECT external_id, mode, agent_profile->>'console_merge' AS on_, facts->>'default_branch' AS def
         FROM services WHERE NOT archived AND kind = 'repo'",
    )
    .fetch_all(st.orch.pool())
    .await
    .unwrap_or_default()
    .into_iter()
    .map(|r| {
        let mode: String = r.get("mode");
        let on_: Option<String> = r.get("on_");
        let def: Option<String> = r.get("def");
        (
            r.get::<String, _>("external_id"),
            (mode == "full", on_.as_deref() == Some("true"), def.unwrap_or_else(|| "main".into())),
        )
    })
    .collect()
}

#[derive(Deserialize)]
pub struct MergeIn {
    csrf: String,
    service: String,
    pr: String,
}

/// Merge what the AI approved. The checks (full mode, the switch, the target branch, this exact
/// commit) are made again in the orchestrator, whatever the page showed.
pub async fn merge(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Form(f): Form<MergeIn>,
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
    match st.orch.merge_pull(&f.service, &f.pr, "console:admin").await {
        Ok(()) => redirect(&format!("/pulls?refresh=1&done=merged-{}", f.pr)),
        Err(e) => {
            let msg: String = e.to_string().chars().take(200).collect();
            redirect(&format!("/pulls?refresh=1&err={}", url_encode(&msg)))
        }
    }
}

#[derive(Deserialize)]
pub struct SwitchIn {
    csrf: String,
    service: String,
    on: String,
}

/// Switch merging from the console on or off for one repository.
pub async fn switch(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Form(f): Form<SwitchIn>,
) -> Response {
    if !csrf_ok(&st, &headers, &f.csrf).await {
        return forbidden();
    }
    let on = f.on == "1";
    let changed = sqlx::query(
        "UPDATE services SET agent_profile = agent_profile || $2 WHERE external_id = $1 AND NOT archived AND ($3 = false OR mode = 'full')",
    )
    .bind(&f.service)
    .bind(json!({"console_merge": on}))
    .bind(on)
    .execute(st.orch.pool())
    .await
    .map(|r| r.rows_affected())
    .unwrap_or(0);
    if changed == 0 {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            "Only a repository in full mode can be allowed to merge from here.",
        )
            .into_response();
    }
    let _ = sqlx::query("INSERT INTO audit_events (actor, action, detail) VALUES ('console:admin', 'service.console_merge', $1)")
        .bind(json!({"service": f.service, "on": on}))
        .execute(st.orch.pool())
        .await;
    redirect("/pulls")
}

/// The page's section that lists which repositories may merge from here.
async fn switches_html(st: &AppState, csrf: &str) -> String {
    let rows = sqlx::query(
        "SELECT external_id, mode, agent_profile->>'console_merge' AS on_ FROM services WHERE NOT archived AND kind = 'repo' ORDER BY external_id",
    )
    .fetch_all(st.orch.pool())
    .await
    .unwrap_or_default();
    let mut h = String::from("<details id=\"merging\" class=\"card\" style=\"margin-top:1rem\" open><summary><strong>Merging from here</strong> <span class=\"muted small\">off for every repository until you switch it on</span></summary><p class=\"small muted\">A Merge button appears only for a pull request the AI approved at its latest commit, in a repository that is in full mode and switched on here, and only when the pull request does not go into the repository's default branch. Merging into the default branch stays a person's job on GitHub.</p><div class=\"scroll\"><table><tbody>");
    for r in rows {
        let name: String = r.get("external_id");
        let mode: String = r.get("mode");
        let on = r.get::<Option<String>, _>("on_").as_deref() == Some("true");
        let action = if mode != "full" {
            format!(
                "<span class=\"muted small\">{} mode: set it to full on Repositories first</span>",
                esc(&mode)
            )
        } else {
            format!(
                "<form method=\"post\" action=\"/pulls/switch\" style=\"display:inline\"><input type=\"hidden\" name=\"csrf\" value=\"{}\"><input type=\"hidden\" name=\"service\" value=\"{}\"><input type=\"hidden\" name=\"on\" value=\"{}\"><button class=\"btn small{}\">{}</button></form>",
                esc(csrf),
                esc(&name),
                if on { "0" } else { "1" },
                if on { "" } else { " primary" },
                if on { "Switch off" } else { "Switch on" }
            )
        };
        h.push_str(&format!(
            "<tr><td class=\"mono\">{}</td><td>{}</td><td>{}</td></tr>",
            esc(&name),
            if on {
                "<span class=\"pill ok\">on</span>"
            } else {
                "<span class=\"pill\">off</span>"
            },
            action
        ));
    }
    h.push_str("</tbody></table></div></details>");
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_running_pull_review_shows_up_as_work_for_the_review_agent() {
        let busy = busy_json(vec![("acme/map-test".to_string(), "42".to_string())]);
        assert_eq!(busy.len(), 1);
        assert_eq!(busy[0]["agent"], "review");
        assert_eq!(busy[0]["title"], "PR #42 in acme/map-test");
        assert_eq!(busy[0]["pull"], "acme/map-test#42");
        assert!(busy[0]["ticket"].is_null());
    }
}
