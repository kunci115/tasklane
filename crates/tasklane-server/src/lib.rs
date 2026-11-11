//! The tasklane daemon: workers, the sync loop, the gateway API and the console.

pub mod api;
pub mod auth;
pub mod chat;
pub mod config;
pub mod connections;
pub mod console;
pub mod doctor;
pub mod runtime;
pub mod siem;
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
    /// (failed sign-ins, when the count started)
    pub login_throttle: std::sync::Mutex<console::Throttle>,
    /// Secrets saved from the console (files under the data directory).
    pub secrets: connections::Secrets,
    /// The plug-in specs the operator's environment describes; the console adds to them.
    pub base_source: tasklane_orchestrator::host::PluginSpec,
    pub base_engine: tasklane_orchestrator::host::PluginSpec,
    /// Public address of the console as the operator set it (the browser's own address is used when unset).
    pub public_url: Option<String>,
    /// `/metrics` is served only when this is set, and only to a caller that sends it as a bearer token.
    pub metrics_token: Option<String>,
    /// Alerts are accepted only when this is set, signed with it (HMAC-SHA256 of the body).
    pub alert_secret: Option<String>,
    /// The audit log can be read by a SIEM only when this is set, as a bearer token.
    pub siem_token: Option<String>,
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
