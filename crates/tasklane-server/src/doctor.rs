//! `tasklane doctor`: check an installation without starting it. Every line says what was checked and,
//! when it is not fine, what to do. Nothing here changes anything.

use std::process::Command;
use std::time::Duration;

use sqlx::PgPool;
use tasklane_orchestrator::host::{PluginClient, PluginSpec};

use crate::config::Settings;

#[derive(Debug, PartialEq, Eq)]
pub enum Level {
    Ok,
    Warn,
    Fail,
}

#[derive(Debug)]
pub struct Check {
    pub level: Level,
    pub what: String,
    pub detail: String,
}

fn check(level: Level, what: &str, detail: impl Into<String>) -> Check {
    Check {
        level,
        what: what.into(),
        detail: detail.into(),
    }
}

fn runs(program: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(program).args(args).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

async fn plugin(what: &str, spec: &PluginSpec) -> Check {
    let client = match PluginClient::spawn(spec) {
        Ok(c) => c,
        Err(e) => {
            return check(
                Level::Fail,
                what,
                format!(
                "could not start `{}`: {e}. Check the command and that the program is installed.",
                spec.program
            ),
            )
        }
    };
    match client
        .call("describe", serde_json::json!({}), Duration::from_secs(10))
        .await
    {
        Ok(v) => check(
            Level::Ok,
            what,
            format!(
                "{} {} answers (protocol {})",
                v.get("name").and_then(|x| x.as_str()).unwrap_or("plug-in"),
                v.get("version").and_then(|x| x.as_str()).unwrap_or(""),
                v.get("protocol_major")
                    .map_or("?".to_string(), |x| x.to_string())
            ),
        ),
        Err(e) => check(
            Level::Fail,
            what,
            format!("started but did not answer `describe`: {e}"),
        ),
    }
}

/// Run every check. `pool` is the database connection if one could be made.
pub async fn run(s: &Settings, pool: Option<&PgPool>, db_error: Option<String>) -> Vec<Check> {
    let mut out = vec![];
    match (pool, db_error) {
        (Some(pool), _) => {
            out.push(check(Level::Ok, "database", "connected"));
            let applied: Result<i64, _> =
                sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations")
                    .fetch_one(pool)
                    .await;
            match applied {
                Ok(n) => out.push(check(Level::Ok, "migrations", format!("{n} applied"))),
                Err(_) => out.push(check(
                    Level::Warn,
                    "migrations",
                    "none applied yet; they are applied when the daemon starts (or run `tasklane migrate`)",
                )),
            }
        }
        (None, e) => out.push(check(
            Level::Fail,
            "database",
            format!(
                "{}. Check TASKLANE_DATABASE_URL and that Postgres is running.",
                e.unwrap_or_default()
            ),
        )),
    }
    out.push(match runs("git", &["--version"]) {
        Some(v) => check(Level::Ok, "git", v),
        None => check(
            Level::Fail,
            "git",
            "not found on PATH; tasklane needs git to work on repositories",
        ),
    });
    out.push(
        match std::fs::create_dir_all(&s.data_dir).and_then(|_| {
            let probe = s.data_dir.join(".doctor");
            std::fs::write(&probe, b"x").and_then(|_| std::fs::remove_file(&probe))
        }) {
            Ok(()) => check(
                Level::Ok,
                "data directory",
                format!("{} is writable", s.data_dir.display()),
            ),
            Err(e) => check(
                Level::Fail,
                "data directory",
                format!("{} is not writable: {e}", s.data_dir.display()),
            ),
        },
    );
    out.push(plugin("ticket source plug-in", &s.source).await);
    out.push(plugin("agent engine plug-in", &s.engine).await);
    let sandbox = s
        .engine
        .env
        .iter()
        .find(|(k, _)| k == "TASKLANE_SANDBOX")
        .map(|(_, v)| v.as_str());
    if sandbox == Some("docker") {
        out.push(match runs("docker", &["version", "--format", "{{.Server.Version}}"]) {
            Some(v) => check(Level::Ok, "docker", format!("daemon {v}")),
            None => check(Level::Fail, "docker", "the Docker sandbox is chosen but `docker` does not answer; is the socket available?"),
        });
    } else {
        out.push(check(
            Level::Warn,
            "agent sandbox",
            "no Docker sandbox in the environment; choose one in the console (Connections) before real work, or the agent will refuse to run",
        ));
    }
    out.push(match &s.public_url {
        Some(u) if u.starts_with("https://") => check(Level::Ok, "public address", u.clone()),
        Some(u) => check(Level::Warn, "public address", format!("{u} is not https; GitHub webhooks and secure cookies need https")),
        None => check(Level::Warn, "public address", "TASKLANE_PUBLIC_URL is not set; fine for a local try, but GitHub cannot send webhooks (tasklane polls instead)"),
    });
    out
}

/// Print the checks; returns false if any failed.
pub fn print(checks: &[Check]) -> bool {
    let mut ok = true;
    for c in checks {
        let mark = match c.level {
            Level::Ok => "ok  ",
            Level::Warn => "warn",
            Level::Fail => {
                ok = false;
                "FAIL"
            }
        };
        println!("{mark}  {:<24} {}", c.what, c.detail);
    }
    println!(
        "\n{}",
        if ok {
            "No problems that stop tasklane from starting."
        } else {
            "Fix the lines marked FAIL, then run this again."
        }
    );
    ok
}
