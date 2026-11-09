//! The tasklane daemon: workers, the sync loop, the gateway API and the console.

pub mod api;
pub mod auth;
pub mod chat;
pub mod config;
pub mod console;
pub mod runtime;
pub mod tickets;

use std::collections::HashMap;
use std::sync::Arc;

use tasklane_orchestrator::Orchestrator;

/// Shared by every request handler.
pub struct AppState {
    pub orch: Arc<Orchestrator>,
    pub admin_token: String,
    pub jwt_secrets: HashMap<String, String>,
    pub user_daily_limit: i64,
    pub secure_cookies: bool,
}

pub fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Escape text for HTML. Everything that comes from a ticket goes through this.
pub fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}
