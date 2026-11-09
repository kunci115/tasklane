//! The overview: what needs you, what is moving, and a live pipeline board.

use std::sync::Arc;

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Response};
use sqlx::Row;

use super::system::{checklist, setup_status};
use super::ui::{ago, empty, head, icon, level_pill, render, state_pill};
use super::{guard, AppState};
use crate::esc;

struct T {
    id: i64,
    state: String,
    level: Option<i16>,
    title: String,
    service: String,
    secs: i64,
}

fn short(service: &str) -> &str {
    service.rsplit('/').next().unwrap_or(service)
}

fn tcard(t: &T) -> String {
    format!(
        "<a class=\"tcard st-{}\" href=\"/tickets/{}\"><div class=\"t\">{}</div><div class=\"m\"><span>{}</span>{}<span style=\"margin-left:auto\">{}</span></div></a>",
        t.state,
        t.id,
        esc(&t.title),
        esc(short(&t.service)),
        level_pill(t.level),
        ago(t.secs)
    )
}

async fn load(st: &AppState) -> Vec<T> {
    sqlx::query(
        "SELECT t.id, t.state, t.level, coalesce(t.title, '#' || t.external_ref) AS title, s.external_id,
                extract(epoch FROM now() - t.updated_at)::bigint AS secs
         FROM tickets t JOIN services s ON s.id = t.service_id
         WHERE t.state NOT LIKE 'closed_%' OR t.updated_at > now() - interval '7 days'
         ORDER BY t.updated_at DESC LIMIT 400",
    )
    .fetch_all(st.orch.pool())
    .await
    .unwrap_or_default()
    .into_iter()
    .map(|r| T { id: r.get("id"), state: r.get("state"), level: r.get("level"), title: r.get("title"), service: r.get("external_id"), secs: r.get("secs") })
    .collect()
}

/// The part of the page that refreshes by itself.
pub async fn live(st: &AppState) -> String {
    let tickets = load(st).await;
    let you: Vec<&T> = tickets
        .iter()
        .filter(|t| matches!(t.state.as_str(), "awaiting_approval" | "failed"))
        .collect();
    let work = |t: &&T| {
        matches!(
            t.state.as_str(),
            "new"
                | "needs_info"
                | "duplicate_proposed"
                | "triaged"
                | "queued"
                | "developing"
                | "rescoring"
        )
    };
    let check = |t: &&T| {
        matches!(
            t.state.as_str(),
            "pr_open" | "ci_running" | "reviewing" | "merge_ready"
        )
    };
    let done = |t: &&T| {
        matches!(
            t.state.as_str(),
            "merged" | "closed_done" | "closed_stale" | "closed_duplicate" | "closed_rejected"
        )
    };
    let doing: Vec<&T> = tickets.iter().filter(work).collect();
    let checking: Vec<&T> = tickets.iter().filter(check).collect();
    let finished: Vec<&T> = tickets.iter().filter(done).collect();
    let merged7 = tickets.iter().filter(|t| t.state == "merged").count();
    let failed = tickets.iter().filter(|t| t.state == "failed").count();
    let waiting = tickets
        .iter()
        .filter(|t| t.state == "awaiting_approval")
        .count();

    let mut h = String::new();
    h.push_str(&format!(
        "<div class=\"grid g4\" style=\"margin-bottom:1rem\">\
<a class=\"card stat{}\" href=\"/tickets?view=you\" style=\"color:inherit;text-decoration:none\"><span class=\"n\">{waiting}</span><span class=\"l\">waiting for your decision</span></a>\
<a class=\"card stat\" href=\"/tickets?view=working\" style=\"color:inherit;text-decoration:none\"><span class=\"n\">{}</span><span class=\"l\">in progress</span></a>\
<a class=\"card stat ok\" href=\"/tickets?view=done\" style=\"color:inherit;text-decoration:none\"><span class=\"n\">{merged7}</span><span class=\"l\">merged in the last 7 days</span></a>\
<a class=\"card stat{}\" href=\"/tickets?view=failed\" style=\"color:inherit;text-decoration:none\"><span class=\"n\">{failed}</span><span class=\"l\">stopped, need a person</span></a></div>",
        if waiting > 0 { " needs" } else { "" },
        doing.len() + checking.len(),
        if failed > 0 { " bad" } else { "" }
    ));
    // needs you
    h.push_str("<div class=\"card flush\" style=\"margin-bottom:1rem\"><div style=\"padding:1rem 1.2rem .4rem\"><h2 style=\"margin:0\">Needs you</h2></div>");
    if you.is_empty() {
        h.push_str(&empty(
            "check",
            "Nothing is waiting for you. tasklane is handling it.",
        ));
    } else {
        h.push_str("<div class=\"attn-list\">");
        for t in you.iter().take(8) {
            let (what, cta) = if t.state == "failed" {
                ("Stopped and needs a person", "Look")
            } else {
                ("Waiting for approval", "Review")
            };
            h.push_str(&format!(
                "<a class=\"attn\" href=\"/tickets/{}\"><div>{}</div><div><div class=\"main-t\">{}</div><div class=\"sub\">{} · {} · {}</div></div><span class=\"go btn small {}\">{}</span></a>",
                t.id,
                icon(if t.state == "failed" { "alert" } else { "shield" }),
                esc(&t.title),
                esc(&t.service),
                what,
                ago(t.secs),
                "",
                cta
            ));
        }
        h.push_str("</div>");
        if you.len() > 8 {
            h.push_str(&format!(
                "<div class=\"empty\"><a href=\"/tickets?view=you\">and {} more</a></div>",
                you.len() - 8
            ));
        }
    }
    h.push_str("</div>");
    // board
    let col =
        |title: &str, ico: &str, class: &str, items: &[&T], limit: usize, empty_text: &str| {
            let mut c = format!(
                "<div class=\"col {class}\"><h3>{} {}<span class=\"count\">{}</span></h3>",
                icon(ico),
                title,
                items.len()
            );
            if items.is_empty() {
                c.push_str(&format!("<div class=\"empty\">{}</div>", empty_text));
            }
            for t in items.iter().take(limit) {
                c.push_str(&tcard(t));
            }
            if items.len() > limit {
                c.push_str(&format!(
                    "<div class=\"empty small\">+{} more</div>",
                    items.len() - limit
                ));
            }
            c.push_str("</div>");
            c
        };
    let intake: Vec<&T> = tickets
        .iter()
        .filter(|t| {
            matches!(
                t.state.as_str(),
                "new" | "needs_info" | "duplicate_proposed"
            )
        })
        .collect();
    let planned: Vec<&T> = tickets
        .iter()
        .filter(|t| {
            matches!(
                t.state.as_str(),
                "triaged" | "queued" | "developing" | "rescoring"
            )
        })
        .collect();
    h.push_str("<h2>Pipeline</h2><div class=\"board\">");
    h.push_str(&col("New", "list", "", &intake, 6, "No new tickets"));
    h.push_str(&col("Needs you", "shield", "you", &you, 6, "All clear"));
    h.push_str(&col("Working", "zap", "", &planned, 6, "Idle"));
    h.push_str(&col(
        "Checking",
        "eye",
        "",
        &checking,
        6,
        "Nothing in review",
    ));
    h.push_str(&col(
        "Done",
        "check",
        "",
        &finished
            .iter()
            .copied()
            .filter(|t| t.state == "merged" || t.state == "closed_done")
            .collect::<Vec<_>>(),
        5,
        "Nothing merged yet",
    ));
    h.push_str("</div>");
    // keep the done-state pills out of the board but show the legend in the title only
    let _ = state_pill;
    h
}

pub async fn home(State(st): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let sess = match guard(&st, &headers).await {
        Ok(s) => s,
        Err(r) => return r,
    };
    let setup = setup_status(&st).await;
    let mut b = head(
        "Overview",
        "<span class=\"live-dot\"></span>Live. This page updates by itself.",
        "<a class=\"btn\" href=\"/tickets\">All tickets</a>",
    );
    if !setup.complete() {
        b.push_str(&format!(
            "<div class=\"card\" style=\"margin-bottom:1rem\"><h2>Getting started <span class=\"muted\" style=\"font-weight:400\">({} of {} done)</span></h2>{}</div>",
            setup.done(),
            setup.steps.len(),
            checklist(&setup)
        ));
    }
    b.push_str(&format!(
        "<div id=\"live\" data-live=\"/fragments/overview\" data-every=\"5000\">{}</div>",
        live(&st).await
    ));
    render(&st, &sess.csrf, "home", "Overview", &b)
        .await
        .into_response()
}

pub async fn overview_fragment(State(st): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if let Err(r) = guard(&st, &headers).await {
        return r;
    }
    Html(live(&st).await).into_response()
}
