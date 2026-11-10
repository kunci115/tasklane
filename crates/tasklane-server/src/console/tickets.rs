//! Tickets: the list, and the page where a person decides.

use std::sync::Arc;

use axum::extract::{Form, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use serde::Deserialize;
use serde_json::Value;
use sqlx::Row;
use tasklane_orchestrator::approval::{decide, resolve_duplicate, retry_or_close, Decision};

use super::repos::like_literal;
use super::ui::{ago, empty, head, icon, level_pill, pager, render, state_pill, url_encode};
use super::{csrf_ok, forbidden, guard, redirect, AppState};
use crate::esc;

/// Who did it, in words.
pub fn who(actor: &str) -> String {
    match actor {
        "sync" | "orchestrator" | "system" => "tasklane".into(),
        "console:admin" => "you".into(),
        a if a.starts_with("telegram:") => {
            format!("Telegram user {}", a.rsplit(':').next().unwrap_or(""))
        }
        a => a.to_string(),
    }
}

fn s<'a>(d: &'a Value, k: &str) -> &'a str {
    d.get(k).and_then(Value::as_str).unwrap_or_default()
}

fn chips_of(a: &Value) -> String {
    a.as_array()
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(|x| format!("<span class=\"chip\">{}</span>", esc(x)))
                .collect()
        })
        .unwrap_or_default()
}

fn chips(d: &Value, k: &str) -> String {
    d.get(k)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(|x| format!("<span class=\"chip\">{}</span>", esc(x)))
                .collect()
        })
        .unwrap_or_default()
}

/// One audit event as (html text, css class, icon name).
pub fn describe_event_full(
    action: &str,
    d: &Value,
) -> (String, &'static str, &'static str, String) {
    let level = d.get("level").and_then(Value::as_u64).unwrap_or(0);
    match action {
        "ticket.created" => ("Picked up the issue".into(), "info", "list", String::new()),
        "ticket.triaged" => {
            let to = s(d, "to");
            if to == "needs_info" {
                (
                    "Asked the reporter a question".into(),
                    "warn",
                    "chat",
                    d.get("questions")
                        .and_then(Value::as_array)
                        .map(|q| {
                            q.iter()
                                .filter_map(Value::as_str)
                                .collect::<Vec<_>>()
                                .join("\n")
                        })
                        .unwrap_or_default(),
                )
            } else {
                (
                    format!(
                        "Planned it as level {level}{}",
                        if to == "awaiting_approval" {
                            ", waiting for a person"
                        } else {
                            ""
                        }
                    ),
                    if to == "awaiting_approval" {
                        "warn"
                    } else {
                        "ok"
                    },
                    "check",
                    chips(d, "reasons"),
                )
            }
        }
        "ticket.resumed" => (
            "The reporter answered, planning again".into(),
            "info",
            "refresh",
            String::new(),
        ),
        "ticket.decided" => (
            format!(
                "{} by a person (level {level}, via {})",
                if s(d, "decision") == "approve" {
                    "Approved"
                } else {
                    "Rejected"
                },
                esc(s(d, "via"))
            ),
            if s(d, "decision") == "approve" {
                "ok"
            } else {
                "bad"
            },
            if s(d, "decision") == "approve" {
                "check"
            } else {
                "x"
            },
            String::new(),
        ),
        "ticket.developing" => (
            format!(
                "The developer agent started (attempt {})",
                d.get("attempt").and_then(Value::as_u64).unwrap_or(1)
            ),
            "info",
            "zap",
            String::new(),
        ),
        "ticket.text_changed" => (
            "The ticket text changed after approval, asking again".into(),
            "warn",
            "alert",
            chips(d, "reasons"),
        ),
        "ticket.developed" => {
            let files = d
                .get("files")
                .and_then(Value::as_array)
                .map(|a| a.len())
                .unwrap_or(0);
            let asks = s(d, "to") == "awaiting_approval";
            (
                format!(
                    "Change ready: {files} file{}, {} changed lines{}",
                    if files == 1 { "" } else { "s" },
                    d.get("lines").and_then(Value::as_u64).unwrap_or(0),
                    if asks { ", needs your approval" } else { "" }
                ),
                if asks { "warn" } else { "ok" },
                "file",
                d.get("reasons")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join("; ")
                    })
                    .unwrap_or_default(),
            )
        }
        "ticket.pr_opened" => (
            format!("Pull request #{} opened", esc(s(d, "pr"))),
            "info",
            "merge",
            String::new(),
        ),
        "ticket.ci_green" => ("CI passed".into(), "ok", "check", String::new()),
        "ticket.ci_red" => (
            format!(
                "CI failed (run {})",
                d.get("run").and_then(Value::as_u64).unwrap_or(1)
            ),
            "bad",
            "x",
            chips(d, "failed"),
        ),
        "ticket.reviewed" => {
            let v = s(d, "verdict");
            (
                format!(
                    "Reviewed: {}",
                    match v {
                        "approve" => "approved",
                        "changes" => "changes requested",
                        "escalate" => "needs a person",
                        _ =>
                            if d.get("notes").is_some() {
                                "looked at"
                            } else {
                                "done"
                            },
                    }
                ),
                if v == "changes" || v == "escalate" {
                    "warn"
                } else {
                    "ok"
                },
                "eye",
                s(d, "notes").to_string(),
            )
        }
        "ticket.rescored_at_review" => (
            "Before review: needs approval of the final change".into(),
            "warn",
            "shield",
            chips(d, "reasons"),
        ),
        "ticket.merge_refused" => (
            format!("Merge refused ({}), checking again", esc(s(d, "reason"))),
            "warn",
            "alert",
            String::new(),
        ),
        "ticket.merged" => ("Merged".into(), "ok", "merge", String::new()),
        "ticket.failed" => (
            "Stopped, needs a person".into(),
            "bad",
            "alert",
            s(d, "reason").to_string(),
        ),
        "ticket.retried_by_person" => (
            "A person put it back to work".into(),
            "info",
            "refresh",
            String::new(),
        ),
        "ticket.closed_by_person" => ("A person closed it".into(), "", "x", String::new()),
        "chat.ask" => (
            "A question was asked in chat".into(),
            "info",
            "chat",
            String::new(),
        ),
        "chat.group_registered" => (
            "A chat group was registered".into(),
            "info",
            "chat",
            String::new(),
        ),
        "service.mode" => (
            format!("Repository mode set to {}", esc(s(d, "mode"))),
            "info",
            "repo",
            String::new(),
        ),
        "service.profile" => (
            "Agent settings changed".into(),
            "info",
            "repo",
            String::new(),
        ),
        "people.added" => ("A person was added".into(), "info", "users", String::new()),
        "people.removed" => (
            "A person was removed".into(),
            "info",
            "users",
            String::new(),
        ),
        "system.paused" => ("All work was paused".into(), "warn", "pause", String::new()),
        "system.resumed" => ("Work was resumed".into(), "ok", "play", String::new()),
        other => (esc(other), "", "info", String::new()),
    }
}

pub fn describe_event(action: &str, d: &Value) -> (String, &'static str, &'static str) {
    let (t, c, i, _) = describe_event_full(action, d);
    (t, c, i)
}

/// Render a unified diff as collapsible files with line numbers and colours.
pub fn render_diff(diff: &str) -> String {
    struct File {
        name: String,
        lines: String,
        add: usize,
        del: usize,
    }
    let mut files: Vec<File> = vec![];
    let (mut old_no, mut new_no) = (0usize, 0usize);
    let mut shown = 0usize;
    let mut truncated = false;
    for line in diff.lines() {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            let name = rest.split(" b/").last().unwrap_or(rest).to_string();
            files.push(File {
                name,
                lines: String::new(),
                add: 0,
                del: 0,
            });
            continue;
        }
        let Some(f) = files.last_mut() else { continue };
        if line.starts_with("@@") {
            // @@ -a,b +c,d @@
            let nums: Vec<usize> = line
                .split_whitespace()
                .filter(|p| p.starts_with('-') || p.starts_with('+'))
                .take(2)
                .filter_map(|p| p[1..].split(',').next().and_then(|n| n.parse().ok()))
                .collect();
            // the numbers are counted up before a line is shown, so start one before the hunk
            old_no = nums.first().copied().unwrap_or(1).saturating_sub(1);
            new_no = nums.get(1).copied().unwrap_or(1).saturating_sub(1);
            f.lines.push_str(&format!("<div class=\"dl hunk\"><span class=\"no\"></span><span class=\"no\"></span><span class=\"tx\">{}</span></div>", esc(line)));
            continue;
        }
        if line.starts_with("--- ")
            || line.starts_with("+++ ")
            || line.starts_with("index ")
            || line.starts_with("new file")
            || line.starts_with("deleted file")
            || line.starts_with("similarity")
            || line.starts_with("rename ")
        {
            if line.starts_with("new file") {
                f.lines.push_str("<div class=\"dl hunk\"><span class=\"no\"></span><span class=\"no\"></span><span class=\"tx\">new file</span></div>");
            } else if line.starts_with("deleted file") {
                f.lines.push_str("<div class=\"dl hunk\"><span class=\"no\"></span><span class=\"no\"></span><span class=\"tx\">file deleted</span></div>");
            }
            continue;
        }
        if line.starts_with("Binary files") {
            f.lines.push_str(&format!("<div class=\"dl hunk\"><span class=\"no\"></span><span class=\"no\"></span><span class=\"tx\">{}</span></div>", esc(line)));
            continue;
        }
        shown += 1;
        if shown > 4000 {
            truncated = true;
            break;
        }
        let (class, a, b, text) = if let Some(t) = line.strip_prefix('+') {
            f.add += 1;
            new_no += 1;
            ("add", String::new(), new_no.to_string(), t)
        } else if let Some(t) = line.strip_prefix('-') {
            f.del += 1;
            old_no += 1;
            ("del", old_no.to_string(), String::new(), t)
        } else {
            old_no += 1;
            new_no += 1;
            (
                "",
                old_no.to_string(),
                new_no.to_string(),
                line.strip_prefix(' ').unwrap_or(line),
            )
        };
        f.lines.push_str(&format!("<div class=\"dl {class}\"><span class=\"no\">{a}</span><span class=\"no\">{b}</span><span class=\"tx\">{}</span></div>", esc(text)));
    }
    if files.is_empty() {
        return String::new();
    }
    let open = files.len() <= 6;
    let mut h = format!(
        "<div class=\"row tight\" style=\"margin-bottom:.6rem;align-items:center\"><span class=\"muted small\">{} file{} changed</span><button class=\"btn small\" type=\"button\" data-expand-all=\"#diff\">Expand or collapse all</button></div><div id=\"diff\">",
        files.len(),
        if files.len() == 1 { "" } else { "s" }
    );
    for f in &files {
        h.push_str(&format!(
            "<details class=\"diff-file\"{}><summary>{} <span class=\"mono\">{}</span><span class=\"stats\"><span class=\"add-n\">+{}</span> <span class=\"del-n\">-{}</span></span></summary><div class=\"diff-lines\">{}</div></details>",
            if open { " open" } else { "" },
            icon("file"),
            esc(&f.name),
            f.add,
            f.del,
            f.lines
        ));
    }
    h.push_str("</div>");
    if truncated {
        h.push_str("<p class=\"muted small\">The diff is very long; only the first part is shown here. The branch has the rest.</p>");
    }
    h
}

fn stepper(state: &str, has_branch: bool, has_pr: bool, has_head: bool) -> String {
    let steps = [
        "Plan",
        "Approval",
        "Develop",
        "Pull request",
        "CI",
        "Review",
        "Merged",
    ];
    // (index of the current step, how it stands)
    let (idx, mode) = match state {
        "new" | "needs_info" | "duplicate_proposed" => (0, "now"),
        "triaged" => (0, "now"),
        "awaiting_approval" => (1, "stop"),
        "queued" | "developing" | "rescoring" => (2, "now"),
        "pr_open" => (3, "now"),
        "ci_running" => (4, "now"),
        "reviewing" | "merge_ready" => (5, "now"),
        "merged" => (7, "done"),
        "failed" => (
            if has_pr && has_head {
                4
            } else if has_branch {
                3
            } else {
                2
            },
            "fail",
        ),
        _ => (99, "closed"),
    };
    let mut h = String::from("<div class=\"stepper\" role=\"list\">");
    for (i, name) in steps.iter().enumerate() {
        let class = if mode == "closed" {
            ""
        } else if i < idx {
            "done"
        } else if i == idx {
            mode
        } else {
            ""
        };
        let mark = if class == "done" {
            icon("check")
        } else if class == "fail" {
            icon("x")
        } else if class == "stop" {
            icon("shield")
        } else {
            (i + 1).to_string()
        };
        h.push_str(&format!("<div class=\"step {class}\" role=\"listitem\"><span class=\"b\">{mark}</span>{name}</div>"));
    }
    h.push_str("</div>");
    h
}

async fn timeline(st: &AppState, id: i64, source: &str, service: &str) -> String {
    let rows = sqlx::query("SELECT actor, action, detail, extract(epoch FROM now()-at)::bigint AS secs, at::text AS at FROM audit_events WHERE ticket_id = $1 ORDER BY id DESC")
        .bind(id)
        .fetch_all(st.orch.pool())
        .await
        .unwrap_or_default();
    if rows.is_empty() {
        return empty("clock", "Nothing has happened yet.");
    }
    let mut h = String::from("<ol class=\"timeline\">");
    for r in rows {
        let action: String = r.get("action");
        let d: Value = r.get("detail");
        let (text, class, ico, note) = describe_event_full(&action, &d);
        let mut text = text;
        if action == "ticket.pr_opened" && source == "github" {
            text = format!("Pull request <a href=\"https://github.com/{}/pull/{}\" target=\"_blank\" rel=\"noopener\">#{}</a> opened", esc(service), esc(s(&d, "pr")), esc(s(&d, "pr")));
        }
        let secs: i64 = r.get("secs");
        h.push_str(&format!(
            "<li class=\"tl {class}\"><span class=\"ic\">{}</span><div class=\"body\"><div class=\"what\">{}</div><div class=\"when\" title=\"{}\">{} · {}</div>{}</div></li>",
            icon(ico),
            text,
            esc(&r.get::<String, _>("at")),
            ago(secs),
            esc(&who(&r.get::<String, _>("actor"))),
            if note.is_empty() { String::new() } else if note.starts_with("<span") { format!("<div style=\"margin-top:.3rem\">{note}</div>") } else { format!("<div class=\"note\">{}</div>", esc(&note)) }
        ));
    }
    h.push_str("</ol>");
    h
}

struct Detail {
    state: String,
    level: Option<i16>,
    reasons: Value,
    plan_hash: Option<String>,
    ext: String,
    title: Option<String>,
    branch: Option<String>,
    pr: Option<String>,
    err: Option<String>,
    plan: Option<String>,
    diff: Option<String>,
    head: Option<String>,
    service: String,
    age: i64,
    ci_runs: i16,
    attempts: i16,
    reporter: Option<String>,
    duplicate_of: Option<String>,
    tokens: i64,
    models: Option<String>,
}

async fn load(st: &AppState, id: i64) -> Option<Detail> {
    let r = sqlx::query(
        "SELECT t.state, t.level, t.level_reasons, t.plan_hash, t.external_ref, t.title, t.branch, t.pr_ref, t.last_error, t.plan_text, t.diff_text, t.head_sha,
                s.external_id, extract(epoch FROM now()-t.created_at)::bigint AS age, t.ci_runs, t.dev_attempts, t.reporter_ref, t.duplicate_of, t.tokens_used,
                (SELECT string_agg(DISTINCT r.usage->>'model', ', ') FROM runs r JOIN jobs j ON j.id = r.job_id WHERE j.ticket_id = t.id AND r.usage ? 'model') AS models
         FROM tickets t JOIN services s ON s.id = t.service_id WHERE t.id = $1",
    )
    .bind(id)
    .fetch_optional(st.orch.pool())
    .await
    .ok()
    .flatten()?;
    Some(Detail {
        state: r.get("state"),
        level: r.get("level"),
        reasons: r.get("level_reasons"),
        plan_hash: r.get("plan_hash"),
        ext: r.get("external_ref"),
        title: r.get("title"),
        branch: r.get("branch"),
        pr: r.get("pr_ref"),
        err: r.get("last_error"),
        plan: r.get("plan_text"),
        diff: r.get("diff_text"),
        head: r.get("head_sha"),
        service: r.get("external_id"),
        age: r.get("age"),
        ci_runs: r.get("ci_runs"),
        attempts: r.get("dev_attempts"),
        reporter: r.get("reporter_ref"),
        duplicate_of: r.get("duplicate_of"),
        tokens: r.get("tokens_used"),
        models: r.get("models"),
    })
}

async fn steps_part(d: &Detail) -> String {
    format!(
        "<div class=\"card\" style=\"margin-bottom:1rem\">{}</div>",
        stepper(
            &d.state,
            d.branch.is_some(),
            d.pr.is_some(),
            d.head.is_some()
        )
    )
}

async fn timeline_part(st: &AppState, id: i64, d: &Detail) -> String {
    let source = st.orch.config().source_name.clone();
    format!(
        "<div class=\"card\"><h2>What happened</h2>{}</div>",
        timeline(st, id, &source, &d.service).await
    )
}

#[derive(Deserialize)]
pub struct PartQuery {
    part: Option<String>,
}

pub async fn live_fragment(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Query(q): Query<PartQuery>,
) -> Response {
    if let Err(r) = guard(&st, &headers).await {
        return r;
    }
    match load(&st, id).await {
        Some(d) if q.part.as_deref() == Some("steps") => Html(steps_part(&d).await).into_response(),
        Some(d) => Html(timeline_part(&st, id, &d).await).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

pub async fn detail(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Response {
    let sess = match guard(&st, &headers).await {
        Ok(s) => s,
        Err(r) => return r,
    };
    let Some(d) = load(&st, id).await else {
        let b = format!(
            "{}<div class=\"card\">{}</div>",
            head(
                "Not found",
                "",
                "<a class=\"btn\" href=\"/tickets\">All tickets</a>"
            ),
            empty("search", "There is no such ticket.")
        );
        return (
            StatusCode::NOT_FOUND,
            render(&st, &sess.csrf, "tickets", "Not found", &b).await,
        )
            .into_response();
    };
    let source = st.orch.config().source_name.clone();
    let issue_link = if source == "github" {
        format!("https://github.com/{}/issues/{}", d.service, d.ext)
    } else {
        String::new()
    };
    let pr_link = match (&d.pr, source.as_str()) {
        (Some(p), "github") => format!("https://github.com/{}/pull/{}", d.service, p),
        _ => String::new(),
    };
    let title = d
        .title
        .clone()
        .unwrap_or_else(|| format!("Issue #{}", d.ext));
    let mut actions = String::new();
    if !issue_link.is_empty() {
        actions.push_str(&format!(
            "<a class=\"btn\" href=\"{}\" target=\"_blank\" rel=\"noopener\">{} Issue</a>",
            esc(&issue_link),
            icon("external")
        ));
    }
    if !pr_link.is_empty() {
        actions.push_str(&format!(
            "<a class=\"btn\" href=\"{}\" target=\"_blank\" rel=\"noopener\">{} Pull request</a>",
            esc(&pr_link),
            icon("merge")
        ));
    }
    let mut b = format!(
        "<div class=\"crumbs\"><a href=\"/tickets\">Tickets</a> / {} #{}</div>",
        esc(&d.service),
        esc(&d.ext)
    );
    b.push_str(&head(
        &title,
        &format!("{} {}", state_pill(&d.state), level_pill(d.level)),
        &actions,
    ));

    b.push_str(&format!(
        "<div data-live=\"/fragments/ticket/{id}?part=steps\" data-every=\"4000\">{}</div>",
        steps_part(&d).await
    ));
    // decision
    if d.state == "awaiting_approval" {
        if let Some(h) = &d.plan_hash {
            let is_diff = d.diff.is_some();
            let what = if is_diff {
                "the exact change shown below"
            } else {
                "this plan"
            };
            b.push_str(&format!(
                "<div class=\"decide\"><h2 style=\"margin:0 0 .3rem\">{} Your decision</h2>\
<p style=\"margin:.2rem 0\">Approving applies to <strong>{what}</strong>{}. If it changes, you will be asked again.</p>{}\
<form method=\"post\" action=\"/tickets/{id}/decide\"><input type=\"hidden\" name=\"csrf\" value=\"{}\"><input type=\"hidden\" name=\"plan_hash\" value=\"{}\">\
<div class=\"btns\"><button class=\"btn ok\" name=\"decision\" value=\"approve\">{} Approve</button><button class=\"btn danger\" name=\"decision\" value=\"reject\" data-confirm=\"Reject this ticket? It will be closed.\">{} Reject</button></div></form>\
<p class=\"hint\" style=\"margin-bottom:0\">Reference <span class=\"mono\">{}</span></p></div>",
                icon("shield"),
                if d.level == Some(3) { " (level 3: owner only)" } else { "" },
                if d.reasons.as_array().map(|a| a.is_empty()).unwrap_or(true) { String::new() } else { format!("<p style=\"margin:.3rem 0\">Why a person is needed: {}</p>", chips_of(&d.reasons)) },
                sess.csrf,
                esc(h),
                icon("check"),
                icon("x"),
                esc(&h[..12.min(h.len())])
            ));
        }
    }
    if d.state == "duplicate_proposed" {
        b.push_str(&format!(
            "<div class=\"decide\"><h2 style=\"margin:0 0 .3rem\">{} Is this a duplicate?</h2><p style=\"margin:.2rem 0\">It looks like ticket <strong>#{}</strong> in the same repository. Nothing happens until a person answers.</p>\
<form method=\"post\" action=\"/tickets/{id}/duplicate\"><input type=\"hidden\" name=\"csrf\" value=\"{}\"><div class=\"btns\"><button class=\"btn ok\" name=\"action\" value=\"confirm\">{} Yes, close it as a duplicate</button><button class=\"btn\" name=\"action\" value=\"reject\">No, handle it as a new ticket</button></div></form></div>",
            icon("shield"),
            esc(d.duplicate_of.as_deref().unwrap_or("?")),
            sess.csrf,
            icon("check")
        ));
    }
    if d.state == "failed"
        && d.err
            .as_deref()
            .is_some_and(|e| e.starts_with(tasklane_orchestrator::NOTHING_TO_CHANGE))
    {
        b.push_str(&format!(
            "<div class=\"decide\"><h2 style=\"margin:0 0 .3rem\">{} Nothing needed changing</h2><p style=\"margin:.2rem 0\">{}</p>\
<form method=\"post\" action=\"/tickets/{id}/retry\"><input type=\"hidden\" name=\"csrf\" value=\"{}\"><div class=\"btns\"><button class=\"btn primary\" name=\"action\" value=\"done\" data-confirm=\"Close the issue as done?\">{} It is done: close the issue</button><button class=\"btn\" name=\"action\" value=\"retry\">Try again</button><button class=\"btn danger\" name=\"action\" value=\"close\" data-confirm=\"Close this ticket for good?\">Close the ticket only</button></div></form></div>",
            icon("check"),
            esc(d.err.as_deref().unwrap_or("")),
            sess.csrf,
            icon("check")
        ));
    } else if d.state == "failed" {
        b.push_str(&format!(
            "<div class=\"decide\" style=\"border-color:var(--bad);background:linear-gradient(0deg,var(--panel),var(--bad-bg))\"><h2 style=\"margin:0 0 .3rem\">{} This ticket stopped</h2><p style=\"margin:.2rem 0\">{}</p>\
<form method=\"post\" action=\"/tickets/{id}/retry\"><input type=\"hidden\" name=\"csrf\" value=\"{}\"><div class=\"btns\"><button class=\"btn primary\" name=\"action\" value=\"retry\">{} Try again</button><button class=\"btn danger\" name=\"action\" value=\"close\" data-confirm=\"Close this ticket for good?\">Close it</button></div></form></div>",
            icon("alert"),
            esc(d.err.as_deref().unwrap_or("No reason was recorded.")),
            sess.csrf,
            icon("refresh")
        ));
    }
    if !matches!(
        d.state.as_str(),
        "merged" | "closed_done" | "closed_rejected" | "closed_stale" | "closed_duplicate"
    ) {
        b.push_str(&message_card(&st, id, &d.state, &sess.csrf).await);
    }
    // layout
    b.push_str("<div class=\"grid\" style=\"grid-template-columns:minmax(0,2fr) minmax(0,1fr);align-items:start\"><div>");
    if let Some(plan) = &d.plan {
        b.push_str(&format!("<div class=\"card\" style=\"margin-bottom:1rem\"><h2>Plan</h2><div class=\"prewrap\">{}</div></div>", esc(plan)));
    }
    if let Some(diff) = &d.diff {
        let shown = render_diff(diff);
        let shown = if shown.is_empty() {
            format!("<div class=\"prewrap\">{}</div>", esc(diff))
        } else {
            shown
        };
        b.push_str(&format!(
            "<div class=\"card\" style=\"margin-bottom:1rem\"><h2>The change</h2>{shown}</div>"
        ));
    }
    if d.plan.is_none() && d.diff.is_none() {
        b.push_str(&format!(
            "<div class=\"card\" style=\"margin-bottom:1rem\">{}</div>",
            empty(
                "file",
                "No plan yet. It appears once the ticket is planned."
            )
        ));
    }
    b.push_str(&format!("</div><div><div data-live=\"/fragments/ticket/{id}?part=timeline\" data-every=\"4000\">{}</div>", timeline_part(&st, id, &d).await));
    b.push_str(&format!(
        "<div class=\"card\" style=\"margin-top:1rem\"><h2>Details</h2><dl class=\"kv\"><dt>Repository</dt><dd>{}</dd><dt>Issue</dt><dd>#{}</dd><dt>Opened</dt><dd>{}</dd><dt>Reported by</dt><dd>{}</dd><dt>Branch</dt><dd class=\"mono\">{}</dd><dt>Attempts</dt><dd>{} development, {} CI</dd><dt>Model tokens</dt><dd>{}</dd><dt>Model</dt><dd>{}</dd></dl></div>",
        esc(&d.service),
        esc(&d.ext),
        ago(d.age),
        esc(d.reporter.as_deref().unwrap_or("on the ticket source")),
        esc(d.branch.as_deref().unwrap_or("-")),
        d.attempts,
        d.ci_runs,
        d.tokens,
        esc(d.models.as_deref().unwrap_or("not reported yet"))
    ));
    b.push_str("</div></div>");
    render(&st, &sess.csrf, "tickets", &title, &b)
        .await
        .into_response()
}

#[derive(Deserialize)]
pub struct ListQuery {
    view: Option<String>,
    q: Option<String>,
    page: Option<u32>,
}

const PER_PAGE: u32 = 25;

pub async fn list(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<ListQuery>,
) -> Response {
    let sess = match guard(&st, &headers).await {
        Ok(s) => s,
        Err(r) => return r,
    };
    let view = q.view.as_deref().unwrap_or("all");
    let states: &[&str] = match view {
        "you" => &["awaiting_approval", "failed", "duplicate_proposed"],
        "working" => &[
            "new",
            "needs_info",
            "duplicate_proposed",
            "triaged",
            "queued",
            "developing",
            "rescoring",
            "pr_open",
            "ci_running",
            "reviewing",
            "merge_ready",
        ],
        "done" => &[
            "merged",
            "closed_done",
            "closed_stale",
            "closed_duplicate",
            "closed_rejected",
        ],
        "failed" => &["failed"],
        _ => &[],
    };
    let needle =
        q.q.as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| format!("%{}%", like_literal(s)));
    let states_v = states.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let total: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM tickets t JOIN services s ON s.id = t.service_id
         WHERE NOT s.archived AND (cardinality($1::text[]) = 0 OR t.state = ANY($1))
           AND ($2::text IS NULL OR t.title ILIKE $2 OR t.external_ref ILIKE $2 OR s.external_id ILIKE $2)",
    )
    .bind(&states_v)
    .bind(needle.as_deref())
    .fetch_one(st.orch.pool())
    .await
    .unwrap_or(0);
    let pages = ((total + i64::from(PER_PAGE) - 1) / i64::from(PER_PAGE)).max(1) as u32;
    let page = q.page.filter(|p| *p >= 1).unwrap_or(1).min(pages);
    let rows = sqlx::query(
        "SELECT t.id, t.state, t.level, t.external_ref, coalesce(t.title,'') AS title, s.external_id, extract(epoch FROM now()-t.updated_at)::bigint AS secs
         FROM tickets t JOIN services s ON s.id = t.service_id
         WHERE NOT s.archived AND (cardinality($1::text[]) = 0 OR t.state = ANY($1))
           AND ($2::text IS NULL OR t.title ILIKE $2 OR t.external_ref ILIKE $2 OR s.external_id ILIKE $2)
         ORDER BY t.updated_at DESC, t.id DESC LIMIT $3 OFFSET $4",
    )
    .bind(&states_v)
    .bind(needle.as_deref())
    .bind(i64::from(PER_PAGE))
    .bind(i64::from(page - 1) * i64::from(PER_PAGE))
    .fetch_all(st.orch.pool())
    .await
    .unwrap_or_default();
    let tab = |key: &str, label: &str| {
        format!(
            "<a class=\"btn small{}\" href=\"/tickets?view={key}\">{label}</a>",
            if key == view { " primary" } else { "" }
        )
    };
    let mut b = head(
        "Tickets",
        "Every issue tasklane picked up.",
        &format!(
            "{}{}{}{}{}",
            tab("all", "All"),
            tab("you", "Needs you"),
            tab("working", "In progress"),
            tab("done", "Done"),
            tab("failed", "Failed")
        ),
    );
    b.push_str(&format!(
        "<form method=\"get\" action=\"/tickets\" class=\"row\" style=\"margin-bottom:1rem\"><input type=\"hidden\" name=\"view\" value=\"{}\"><input type=\"text\" name=\"q\" value=\"{}\" placeholder=\"Search by title, number or repository\" aria-label=\"Search\"><div class=\"row tight\"><button class=\"btn\">{} Search</button></div></form>",
        esc(view),
        esc(q.q.as_deref().unwrap_or("")),
        icon("search")
    ));
    if rows.is_empty() {
        b.push_str(&format!(
            "<div class=\"card\">{}</div>",
            empty("list", "No tickets match.")
        ));
    } else {
        b.push_str("<div class=\"card flush\"><div class=\"scroll\"><table><thead><tr><th>Ticket</th><th>Repository</th><th>State</th><th>Level</th><th>Updated</th></tr></thead><tbody>");
        for r in rows {
            let id: i64 = r.get("id");
            let title: String = r.get("title");
            let ext: String = r.get("external_ref");
            b.push_str(&format!(
                "<tr><td><a href=\"/tickets/{id}\"><strong>{}</strong></a> <span class=\"muted\">#{}</span></td><td class=\"nw\">{}</td><td>{}</td><td>{}</td><td class=\"muted nw\">{}</td></tr>",
                if title.is_empty() { "(no title)".to_string() } else { esc(&title) },
                esc(&ext),
                esc(&r.get::<String, _>("external_id")),
                state_pill(&r.get::<String, _>("state")),
                level_pill(r.get("level")),
                ago(r.get("secs"))
            ));
        }
        b.push_str("</tbody></table></div></div>");
        let search = q.q.as_deref().map(str::trim).filter(|s| !s.is_empty());
        b.push_str(&pager(
            &format!(
                "/tickets?view={}{}",
                url_encode(view),
                search
                    .map(|s| format!("&q={}", url_encode(s)))
                    .unwrap_or_default()
            ),
            page,
            pages,
            total,
            PER_PAGE,
        ));
    }
    render(&st, &sess.csrf, "tickets", "Tickets", &b)
        .await
        .into_response()
}

#[derive(Deserialize)]
pub struct DecideIn {
    csrf: String,
    plan_hash: String,
    decision: String,
}

async fn message(
    st: &AppState,
    csrf: &str,
    status: StatusCode,
    title: &str,
    text: &str,
    back: &str,
) -> Response {
    let b = format!(
        "{}<div class=\"card\"><p>{}</p><a class=\"btn ghost\" href=\"{}\">Back</a></div>",
        head(title, "", ""),
        esc(text),
        esc(back)
    );
    (status, render(st, csrf, "tickets", title, &b).await).into_response()
}

pub async fn decide_form(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Form(f): Form<DecideIn>,
) -> Response {
    if !csrf_ok(&st, &headers, &f.csrf).await {
        return forbidden();
    }
    let decision = if f.decision == "approve" {
        Decision::Approve
    } else {
        Decision::Reject
    };
    let who = match guard(&st, &headers).await {
        Ok(s) => s.principal(),
        Err(r) => return r,
    };
    match decide(st.orch.pool(), id, &who, "console", decision, &f.plan_hash).await {
        Ok(_) => redirect(&format!("/tickets/{id}")),
        Err(e) => {
            message(
                &st,
                &f.csrf,
                StatusCode::CONFLICT,
                "That did not go through",
                &e.to_string(),
                &format!("/tickets/{id}"),
            )
            .await
        }
    }
}

#[derive(Deserialize)]
pub struct RetryIn {
    csrf: String,
    action: String,
}

pub async fn duplicate_form(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Form(f): Form<RetryIn>,
) -> Response {
    if !csrf_ok(&st, &headers, &f.csrf).await {
        return forbidden();
    }
    let who = match guard(&st, &headers).await {
        Ok(s) => s.principal(),
        Err(r) => return r,
    };
    let confirm = f.action == "confirm";
    match resolve_duplicate(st.orch.pool(), id, &who, confirm).await {
        Ok(_) => {
            st.orch
                .say_on(
                    id,
                    if confirm {
                        "A person confirmed that this is a duplicate; it is closed here."
                    } else {
                        "A person said this is not a duplicate; it goes on as a new ticket."
                    },
                )
                .await;
            redirect(&format!("/tickets/{id}"))
        }
        Err(e) => {
            message(
                &st,
                &f.csrf,
                StatusCode::CONFLICT,
                "That did not go through",
                &e.to_string(),
                &format!("/tickets/{id}"),
            )
            .await
        }
    }
}

pub async fn retry_form(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Form(f): Form<RetryIn>,
) -> Response {
    if !csrf_ok(&st, &headers, &f.csrf).await {
        return forbidden();
    }
    let sess = match guard(&st, &headers).await {
        Ok(s) => s,
        Err(r) => return r,
    };
    let who = sess.principal();
    let outcome = if f.action == "done" && !matches!(sess.role.as_str(), "owner" | "team_approver")
    {
        Err("you are not registered as an approver".to_string())
    } else if f.action == "done" {
        st.orch
            .close_issue_done(id)
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
    } else {
        retry_or_close(st.orch.pool(), id, &who, f.action == "close")
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
    };
    match outcome {
        Ok(_) => redirect(&format!("/tickets/{id}")),
        Err(e) => {
            message(
                &st,
                &f.csrf,
                StatusCode::CONFLICT,
                "That did not go through",
                &e,
                &format!("/tickets/{id}"),
            )
            .await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIFF: &str = "diff --git a/src/a.py b/src/a.py\nindex 1..2 100644\n--- a/src/a.py\n+++ b/src/a.py\n@@ -1,3 +1,4 @@\n keep\n-old <b>\n+new <b>\n+added\n keep2\ndiff --git a/n.txt b/n.txt\nnew file mode 100644\n--- /dev/null\n+++ b/n.txt\n@@ -0,0 +1 @@\n+hello\n";

    #[test]
    fn a_diff_becomes_collapsible_files_with_counts_and_escaped_text() {
        let h = render_diff(DIFF);
        assert!(h.contains("2 files changed"));
        assert!(h.contains("src/a.py") && h.contains("n.txt"));
        assert!(
            h.contains("<span class=\"add-n\">+2</span> <span class=\"del-n\">-1</span>"),
            "{h}"
        );
        assert!(
            h.contains("old &lt;b&gt;") && !h.contains("old <b>"),
            "text is escaped"
        );
        assert!(h.contains("dl add") && h.contains("dl del") && h.contains("dl hunk"));
        assert!(h.contains("new file"));
    }

    #[test]
    fn line_numbers_follow_the_hunk_header() {
        let h = render_diff(DIFF);
        assert!(h.contains("<span class=\"no\">1</span><span class=\"no\">1</span><span class=\"tx\">keep</span>"), "{h}");
        assert!(h.contains("<span class=\"no\"></span><span class=\"no\">3</span><span class=\"tx\">added</span>"), "{h}");
    }

    #[test]
    fn something_that_is_not_a_diff_renders_to_nothing_so_the_page_can_fall_back() {
        assert_eq!(render_diff("+ new line"), "");
    }

    #[test]
    fn the_stepper_marks_where_a_ticket_stands() {
        let h = stepper("awaiting_approval", false, false, false);
        assert!(h.contains("step done") && h.contains("step stop"));
        let h = stepper("failed", true, true, true);
        assert!(h.contains("step fail"));
        assert!(!stepper("closed_rejected", false, false, false).contains("step done"));
    }

    #[test]
    fn events_read_as_sentences() {
        let d = serde_json::json!({"level": 2, "to": "awaiting_approval", "reasons": ["database migration"]});
        let (text, class, _, note) = describe_event_full("ticket.triaged", &d);
        assert!(text.contains("level 2") && text.contains("waiting for a person"));
        assert_eq!(class, "warn");
        assert!(note.contains("database migration"));
        assert_eq!(who("console:admin"), "you");
        assert_eq!(who("orchestrator"), "tasklane");
    }
}

/// Send the agent a message (given to its next run), or stop the run that is going on and start it again
/// with the message. Only for people who can approve.
async fn message_card(st: &AppState, id: i64, state: &str, csrf: &str) -> String {
    let rows: Vec<(String, String, bool)> = sqlx::query_as(
        "SELECT author, body, delivered_at IS NOT NULL FROM ticket_messages WHERE ticket_id = $1 ORDER BY id DESC LIMIT 5",
    )
    .bind(id)
    .fetch_all(st.orch.pool())
    .await
    .unwrap_or_default();
    // states in which a model run may be going on right now
    let running = matches!(state, "new" | "developing" | "reviewing");
    let mut h =
        String::from("<div class=\"card\" style=\"margin:1rem 0\"><h2>Message to the agent</h2>");
    h.push_str(if running {
        "<p class=\"hint\">The agent is working or about to. <strong>Send</strong> gives it your message at its next step. <strong>Stop and restart now</strong> ends the run that is going on and starts it again at once with your message (what it had written so far is dropped; no attempt is used up).</p>"
    } else {
        "<p class=\"hint\">Your message is given to the next run on this ticket (after approval, or when you try again). It comes with your name; the agent follows it in addition to the issue, but it never overrides the safety rules.</p>"
    });
    for (body, delivered) in rows.iter().map(|r| (format!("{}: {}", r.0, r.1), r.2)) {
        h.push_str(&format!(
            "<p class=\"small\">{} <span class=\"pill {}\">{}</span></p>",
            esc(&body),
            if delivered { "ok" } else { "warn" },
            if delivered {
                "given"
            } else {
                "waiting for the next run"
            }
        ));
    }
    h.push_str(&format!(
        "<form method=\"post\" action=\"/tickets/{id}/message\"><input type=\"hidden\" name=\"csrf\" value=\"{}\"><div class=\"field\"><textarea name=\"body\" rows=\"3\" maxlength=\"2000\" required placeholder=\"For example: use the existing date helper in utils/, and do not touch the schema.\"></textarea></div><div class=\"btns\"><button class=\"btn primary\" name=\"now\" value=\"0\">Send</button>{}</div></form></div>",
        esc(csrf),
        if running {
            "<button class=\"btn danger\" name=\"now\" value=\"1\" data-confirm=\"Stop the run that is going on and start it again with this message?\">Stop and restart now</button>"
        } else {
            ""
        }
    ));
    h
}

#[derive(Deserialize)]
pub struct MessageIn {
    csrf: String,
    body: String,
    now: Option<String>,
}

pub async fn send_message(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Form(f): Form<MessageIn>,
) -> Response {
    let sess = match guard(&st, &headers).await {
        Ok(s) => s,
        Err(r) => return r,
    };
    if !csrf_ok(&st, &headers, &f.csrf).await {
        return forbidden();
    }
    if !matches!(sess.role.as_str(), "owner" | "team_approver") {
        return forbidden();
    }
    let state: Option<String> = sqlx::query_scalar("SELECT state FROM tickets WHERE id = $1")
        .bind(id)
        .fetch_optional(st.orch.pool())
        .await
        .ok()
        .flatten();
    let Some(state) = state else {
        return (StatusCode::NOT_FOUND, "no such ticket").into_response();
    };
    if state.starts_with("closed_") || state == "merged" {
        return (StatusCode::CONFLICT, "That ticket is finished.").into_response();
    }
    let author = format!("{}:{}", sess.channel, sess.reference);
    if let Err(e) = st.orch.add_message(id, &author, &f.body).await {
        return (StatusCode::UNPROCESSABLE_ENTITY, e.to_string()).into_response();
    }
    let mut stopped = false;
    if f.now.as_deref() == Some("1") && matches!(state.as_str(), "new" | "developing" | "reviewing")
    {
        stopped = st.orch.stop_running(id).await;
    }
    let _ = sqlx::query("INSERT INTO audit_events (actor, action, ticket_id, detail) VALUES ($1, 'ticket.message', $2, $3)")
        .bind(&author)
        .bind(id)
        .bind(serde_json::json!({"restart": f.now.as_deref() == Some("1"), "stopped_a_run": stopped}))
        .execute(st.orch.pool())
        .await;
    redirect(&format!("/tickets/{id}"))
}
