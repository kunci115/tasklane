//! Page shell, icons and small building blocks shared by every console page.
//! Everything that comes from a ticket or a repository goes through `esc`.

use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE};
use axum::response::{Html, IntoResponse, Response};

use crate::{esc, AppState};

const CSS: &str = include_str!("../../assets/app.css");
const JS: &str = include_str!("../../assets/app.js");
const MAP_JS: &str = include_str!("../../assets/map.js");
const VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn asset_css() -> Response {
    (
        [
            (CONTENT_TYPE, "text/css; charset=utf-8"),
            (CACHE_CONTROL, "public, max-age=3600"),
        ],
        CSS,
    )
        .into_response()
}
pub fn asset_map_js() -> Response {
    (
        [
            (CONTENT_TYPE, "application/javascript; charset=utf-8"),
            (CACHE_CONTROL, "public, max-age=3600"),
        ],
        MAP_JS,
    )
        .into_response()
}
pub fn asset_js() -> Response {
    (
        [
            (CONTENT_TYPE, "application/javascript; charset=utf-8"),
            (CACHE_CONTROL, "public, max-age=3600"),
        ],
        JS,
    )
        .into_response()
}

const ICON_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64"><defs><linearGradient id="g" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="#6366f1"/><stop offset="1" stop-color="#22d3ee"/></linearGradient></defs><rect width="64" height="64" rx="16" fill="url(#g)"/><path d="M14 22h22M14 32h36M14 42h16" stroke="#fff" stroke-width="5" stroke-linecap="round" fill="none"/><circle cx="46" cy="22" r="4" fill="#fff"/></svg>"##;

pub fn asset_icon() -> Response {
    (
        [
            (CONTENT_TYPE, "image/svg+xml"),
            (CACHE_CONTROL, "public, max-age=86400"),
        ],
        ICON_SVG,
    )
        .into_response()
}

pub fn asset_manifest() -> Response {
    let m = r##"{"name":"tasklane","short_name":"tasklane","description":"Ticket-driven AI agent pipeline","start_url":"/","scope":"/","display":"standalone","background_color":"#0e1120","theme_color":"#14172b","icons":[{"src":"/static/icon.svg","sizes":"any","type":"image/svg+xml","purpose":"any"}]}"##;
    (
        [
            (CONTENT_TYPE, "application/manifest+json"),
            (CACHE_CONTROL, "public, max-age=3600"),
        ],
        m,
    )
        .into_response()
}

/// A small stroke icon by name.
pub fn icon(name: &str) -> String {
    let d = match name {
        "home" => "M3 11l9-8 9 8v10a1 1 0 0 1-1 1h-5v-7h-6v7H4a1 1 0 0 1-1-1z",
        "list" => "M8 6h13M8 12h13M8 18h13M3 6h.01M3 12h.01M3 18h.01",
        "repo" => "M6 3v12M18 9a3 3 0 1 0 0-6 3 3 0 0 0 0 6zM6 21a3 3 0 1 0 0-6 3 3 0 0 0 0 6zM18 9a9 9 0 0 1-9 9",
        "users" => "M17 21v-2a4 4 0 0 0-4-4H5a4 4 0 0 0-4 4v2M9 11a4 4 0 1 0 0-8 4 4 0 0 0 0 8zM23 21v-2a4 4 0 0 0-3-3.87M16 3.13a4 4 0 0 1 0 7.75",
        "chat" => "M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2z",
        "activity" => "M22 12h-4l-3 9L9 3l-3 9H2",
        "heart" => "M20.84 4.61a5.5 5.5 0 0 0-7.78 0L12 5.67l-1.06-1.06a5.5 5.5 0 0 0-7.78 7.78l1.06 1.06L12 21.23l7.78-7.78 1.06-1.06a5.5 5.5 0 0 0 0-7.78z",
        "check" => "M20 6L9 17l-5-5",
        "x" => "M18 6L6 18M6 6l12 12",
        "alert" => "M10.29 3.86L1.82 18a2 2 0 0 0 1.71 3h16.94a2 2 0 0 0 1.71-3L13.71 3.86a2 2 0 0 0-3.42 0zM12 9v4M12 17h.01",
        "pause" => "M6 4h4v16H6zM14 4h4v16h-4z",
        "play" => "M5 3l14 9-14 9z",
        "moon" => "M21 12.79A9 9 0 1 1 11.21 3 7 7 0 0 0 21 12.79z",
        "logout" => "M9 21H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h4M16 17l5-5-5-5M21 12H9",
        "external" => "M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6M15 3h6v6M10 14L21 3",
        "clock" => "M12 22a10 10 0 1 0 0-20 10 10 0 0 0 0 20zM12 6v6l4 2",
        "file" => "M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8zM14 2v6h6",
        "shield" => "M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z",
        "zap" => "M13 2L3 14h9l-1 8 10-12h-9z",
        "merge" => "M18 21a3 3 0 1 0 0-6 3 3 0 0 0 0 6zM6 9a3 3 0 1 0 0-6 3 3 0 0 0 0 6zM6 9v6a3 3 0 0 0 3 3h6",
        "plug" => "M9 2v6M15 2v6M6 8h12v4a6 6 0 0 1-12 0zM12 18v4",
        "network" => "M12 8a3 3 0 1 0 0-6 3 3 0 0 0 0 6zM5 22a3 3 0 1 0 0-6 3 3 0 0 0 0 6zM19 22a3 3 0 1 0 0-6 3 3 0 0 0 0 6zM12 8v4M12 12l-6 4M12 12l6 4",
        "search" => "M11 19a8 8 0 1 0 0-16 8 8 0 0 0 0 16zM21 21l-4.35-4.35",
        "plus" => "M12 5v14M5 12h14",
        "trash" => "M3 6h18M8 6V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2M19 6l-1 14a2 2 0 0 1-2 2H8a2 2 0 0 1-2-2L5 6",
        "refresh" => "M23 4v6h-6M1 20v-6h6M3.51 9a9 9 0 0 1 14.85-3.36L23 10M1 14l4.64 4.36A9 9 0 0 0 20.49 15",
        "eye" => "M1 12s4-8 11-8 11 8 11 8-4 8-11 8-11-8-11-8zM12 15a3 3 0 1 0 0-6 3 3 0 0 0 0 6z",
        "info" => "M12 22a10 10 0 1 0 0-20 10 10 0 0 0 0 20zM12 16v-4M12 8h.01",
        _ => "M12 12h.01",
    };
    format!("<svg class=\"i\" viewBox=\"0 0 24 24\" aria-hidden=\"true\"><path d=\"{d}\"/></svg>")
}

/// Friendly name and colour class of a ticket state.
pub fn state_info(state: &str) -> (&'static str, &'static str) {
    match state {
        "new" => ("New", "info"),
        "needs_info" => ("Waiting for the reporter", "warn"),
        "duplicate_proposed" => ("Possible duplicate", "warn"),
        "triaged" => ("Planned", "info"),
        "awaiting_approval" => ("Needs you", "warn"),
        "queued" => ("Queued", "info"),
        "developing" => ("Developing", "info"),
        "rescoring" => ("Checking the change", "info"),
        "pr_open" => ("Pull request opened", "info"),
        "ci_running" => ("CI running", "info"),
        "reviewing" => ("In review", "info"),
        "merge_ready" => ("Ready to merge", "ok"),
        "merged" => ("Merged", "ok"),
        "failed" => ("Failed", "bad"),
        "stopped" => ("Stopped", "warn"),
        "closed_stale" | "closed_duplicate" | "closed_rejected" | "closed_done" => ("Closed", ""),
        _ => ("Unknown", ""),
    }
}

pub fn state_pill(state: &str) -> String {
    let (label, class) = state_info(state);
    format!("<span class=\"pill {class}\">{}</span>", esc(label))
}

pub fn level_pill(level: Option<i16>) -> String {
    match level {
        Some(1) => {
            "<span class=\"pill ok\" title=\"Level 1: runs without a person\">L1</span>".into()
        }
        Some(2) => {
            "<span class=\"pill warn\" title=\"Level 2: a team approver decides\">L2</span>".into()
        }
        Some(3) => {
            "<span class=\"pill violet\" title=\"Level 3: the owner decides\">L3</span>".into()
        }
        _ => String::new(),
    }
}

/// "just now", "5 min ago", "3 h ago", "2 d ago".
pub fn ago(secs: i64) -> String {
    match secs {
        s if s < 45 => "just now".into(),
        s if s < 3600 => format!("{} min ago", (s / 60).max(1)),
        s if s < 86_400 => format!("{} h ago", s / 3600),
        s => format!("{} d ago", s / 86_400),
    }
}

pub fn repo_link(source: &str, service: &str) -> String {
    if source == "github" {
        format!("https://github.com/{service}")
    } else {
        String::new()
    }
}

/// The full page: sidebar, header and the body. `active` is the nav key.
pub async fn render(
    st: &AppState,
    csrf: &str,
    active: &str,
    title: &str,
    body: &str,
) -> Html<String> {
    let pool = st.orch.pool();
    let paused = st.orch.is_paused().await.unwrap_or(false);
    let needs: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM tickets WHERE state IN ('awaiting_approval','failed')",
    )
    .fetch_one(pool)
    .await
    .unwrap_or(0);
    let item = |key: &str, href: &str, ico: &str, label: &str, badge: Option<i64>| {
        format!(
            "<a href=\"{href}\"{}>{}<span>{label}</span>{}</a>",
            if key == active {
                " aria-current=\"page\""
            } else {
                ""
            },
            icon(ico),
            badge
                .filter(|n| *n > 0)
                .map(|n| format!("<span class=\"badge\">{n}</span>"))
                .unwrap_or_default()
        )
    };
    let nav = [
        item("home", "/", "home", "Overview", Some(needs)),
        item("map", "/map", "network", "Live map", None),
        item("tickets", "/tickets", "list", "Tickets", None),
        item("repos", "/services", "repo", "Repositories", None),
        item("connections", "/connections", "plug", "Connections", None),
        item("people", "/people", "users", "People", None),
        item("chat", "/chat", "chat", "Chat groups", None),
        item("activity", "/activity", "activity", "Activity", None),
        item("health", "/health", "heart", "Health & setup", None),
    ]
    .join("");
    Html(format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
<title>{t} - tasklane</title><meta name=\"theme-color\" content=\"#14172b\"><link rel=\"icon\" href=\"/static/icon.svg\"><link rel=\"manifest\" href=\"/manifest.webmanifest\">\
<link rel=\"stylesheet\" href=\"/static/app.css?v={v}\"></head><body><div class=\"app\">\
<aside class=\"side\"><div class=\"brand\"><span class=\"logo\">{logo}</span>tasklane</div><nav class=\"nav\" aria-label=\"Main\">{nav}</nav><div class=\"grow\"></div>\
<div class=\"foot\"><form method=\"post\" action=\"{pa}\"><input type=\"hidden\" name=\"csrf\" value=\"{c}\">\
<button class=\"btn small\" style=\"width:100%\" {confirm}>{pi} {pl}</button></form>\
<div class=\"runstate\"><span class=\"dot{pc}\"></span>{rs}</div>\
<div class=\"row tight\" style=\"gap:.4rem\"><button class=\"btn small\" data-theme-toggle type=\"button\" title=\"Light or dark\">{moon}</button>\
<form method=\"post\" action=\"/logout\"><input type=\"hidden\" name=\"csrf\" value=\"{c}\"><button class=\"btn small\" title=\"Sign out\">{lo}</button></form></div></div></aside>\
<main class=\"main\">{body}</main></div><script src=\"/static/app.js?v={v}\" defer></script></body></html>",
        t = esc(title),
        v = VERSION,
        logo = "<svg viewBox=\"0 0 24 24\" width=\"18\" height=\"18\"><path d=\"M4 7h10M4 12h16M4 17h7\" stroke=\"#fff\" stroke-width=\"2.4\" stroke-linecap=\"round\" fill=\"none\"/></svg>",
        nav = nav,
        pa = if paused { "/resume" } else { "/pause" },
        c = csrf,
        confirm = if paused { String::new() } else { "data-confirm=\"Stop all work? Running steps finish, nothing new starts.\"".to_string() },
        pi = icon(if paused { "play" } else { "pause" }),
        pl = if paused { "Resume work" } else { "Pause all work" },
        pc = if paused { " paused" } else { "" },
        rs = if paused { "Paused" } else { "Running" },
        moon = icon("moon"),
        lo = icon("logout"),
        body = body
    ))
}

/// Page header with title, optional lead text and right-hand actions.
pub fn head(title: &str, lead: &str, actions: &str) -> String {
    format!(
        "<div class=\"head\"><div><h1>{}</h1>{}</div><div class=\"actions\">{}</div></div>",
        esc(title),
        if lead.is_empty() {
            String::new()
        } else {
            format!("<p class=\"muted\" style=\"margin:0\">{}</p>", lead)
        },
        actions
    )
}

pub fn empty(icon_name: &str, text: &str) -> String {
    format!(
        "<div class=\"empty\">{}<div>{}</div></div>",
        icon(icon_name),
        text
    )
}
