#![allow(dead_code)]
//! Shared helpers: a real Postgres database per test, a real bare git remote,
//! and the fake plug-in program.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::str::FromStr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::{ConnectOptions, PgPool};
use tasklane_orchestrator::host::{PluginClient, PluginSpec};
use tasklane_orchestrator::{Config, Orchestrator, RunResult};
use tasklane_queue::Queue;

static COUNTER: AtomicU32 = AtomicU32::new(0);
pub const FAKE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/fake_plugin.py");

pub fn spec(env: &[(&str, String)]) -> PluginSpec {
    let mut e: Vec<(String, String)> = vec![("PATH".into(), std::env::var("PATH").unwrap())];
    e.extend(env.iter().map(|(k, v)| (k.to_string(), v.clone())));
    PluginSpec {
        program: "python3".into(),
        args: vec!["-I".into(), FAKE.into()],
        env: e,
    }
}

pub async fn test_pool() -> Option<PgPool> {
    let Ok(url) = std::env::var("TASKLANE_TEST_DATABASE_URL") else {
        if std::env::var("TASKLANE_REQUIRE_DB").as_deref() == Ok("1") {
            panic!("TASKLANE_REQUIRE_DB=1 but TASKLANE_TEST_DATABASE_URL is not set");
        }
        eprintln!("skipped: TASKLANE_TEST_DATABASE_URL not set");
        return None;
    };
    let name = format!(
        "tl_orch_{}_{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    );
    let opts = PgConnectOptions::from_str(&url)
        .unwrap()
        .log_statements(log::LevelFilter::Off);
    let mut admin = opts.connect().await.unwrap();
    sqlx::query(&format!("CREATE DATABASE \"{name}\""))
        .execute(&mut admin)
        .await
        .unwrap();
    let pool = PgPoolOptions::new()
        .max_connections(16)
        .connect_with(opts.database(&name))
        .await
        .unwrap();
    tasklane_orchestrator::migrate(&pool).await.unwrap();
    Some(pool)
}

pub fn sh(dir: &Path, cmd: &str) -> String {
    let out = Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{cmd}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

pub struct World {
    pub pool: PgPool,
    pub queue: Queue,
    pub orch: Orchestrator,
    pub dir: tempfile::TempDir,
}

impl World {
    pub fn state_dir(&self) -> PathBuf {
        self.dir.path().join("state")
    }
    pub fn remote(&self) -> PathBuf {
        self.dir.path().join("remote/org/repo.git")
    }
    /// Arm a one-shot behaviour in the fake plug-in (see `once` in the fixture).
    pub fn arm(&self, what: &str) {
        std::fs::write(self.state_dir().join(format!("arm_{what}")), "").unwrap();
    }
    pub fn set_flag(&self, what: &str) {
        std::fs::write(self.state_dir().join(what), "").unwrap();
    }
    pub fn read_state(&self, name: &str) -> String {
        std::fs::read_to_string(self.state_dir().join(name)).unwrap_or_default()
    }
}

/// A world with a bare remote that has one commit on `main`, and the service row registered.
pub async fn world(pool: PgPool, tweak: impl FnOnce(&mut Config)) -> World {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("state")).unwrap();
    std::fs::create_dir_all(dir.path().join("remote/org")).unwrap();
    let remote = dir.path().join("remote/org/repo.git");
    sh(
        dir.path(),
        &format!("git init -q --bare -b main {}", remote.display()),
    );
    let seed = dir.path().join("seed");
    sh(dir.path(), &format!("git init -q -b main {0} && cd {0} && git config user.email a@b.c && git config user.name t && echo hi > README && git add . && git commit -q -m init && git push -q {1} main", seed.display(), remote.display()));

    let env = [
        (
            "FAKE_STATE_DIR",
            dir.path().join("state").display().to_string(),
        ),
        (
            "FAKE_REMOTE_ROOT",
            dir.path().join("remote").display().to_string(),
        ),
    ];
    let queue = Queue::new(pool.clone(), 2);
    let mut cfg = Config::new("orch-1", dir.path().join("data"));
    cfg.lease_secs = 30.0;
    cfg.call_timeout = Duration::from_secs(10);
    cfg.dev_timeout = Duration::from_secs(20);
    cfg.ci_poll_secs = 0.0;
    tweak(&mut cfg);
    let orch = Orchestrator::new(
        pool.clone(),
        queue.clone(),
        PluginClient::spawn(&spec(&env)).unwrap(),
        PluginClient::spawn(&spec(&env)).unwrap(),
        cfg,
    );
    sqlx::query(
        "INSERT INTO services (kind, source, external_id, facts) VALUES ('repo','test','org/repo', $1)",
    )
    .bind(serde_json::json!({"default_branch": "main", "protected_paths": [".github/workflows/"]}))
    .execute(&pool)
    .await
    .unwrap();
    World {
        pool,
        queue,
        orch,
        dir,
    }
}

/// New ticket with a pending triage job. Returns the ticket id.
pub async fn ticket(w: &World, external_ref: &str) -> i64 {
    let (t,): (i64,) = sqlx::query_as("INSERT INTO tickets (service_id, external_ref) SELECT id, $1 FROM services LIMIT 1 RETURNING id")
        .bind(external_ref)
        .fetch_one(&w.pool)
        .await
        .unwrap();
    w.queue.enqueue(t, "triage", 0, 0.0).await.unwrap();
    t
}

/// Run jobs until there is nothing runnable; returns every result.
pub async fn drive(w: &World) -> Vec<RunResult> {
    let mut out = vec![];
    for _ in 0..60 {
        let r = w.orch.run_one().await.unwrap();
        if r == RunResult::Idle {
            return out;
        }
        out.push(r);
    }
    panic!("did not settle: {out:?}");
}

pub async fn state(pool: &PgPool, t: i64) -> String {
    sqlx::query_scalar("SELECT state FROM tickets WHERE id = $1")
        .bind(t)
        .fetch_one(pool)
        .await
        .unwrap()
}
pub async fn level(pool: &PgPool, t: i64) -> Option<i16> {
    sqlx::query_scalar("SELECT level FROM tickets WHERE id = $1")
        .bind(t)
        .fetch_one(pool)
        .await
        .unwrap()
}
pub async fn count(pool: &PgPool, sql: &str) -> i64 {
    sqlx::query_scalar(sql).fetch_one(pool).await.unwrap()
}
pub async fn audit_actions(pool: &PgPool, t: i64) -> Vec<String> {
    sqlx::query_scalar("SELECT action FROM audit_events WHERE ticket_id = $1 ORDER BY id")
        .bind(t)
        .fetch_all(pool)
        .await
        .unwrap()
}
pub async fn add_principal(pool: &PgPool, reference: &str, role: &str) {
    sqlx::query("INSERT INTO principals (channel, ref, role) VALUES ('console', $1, $2)")
        .bind(reference)
        .bind(role)
        .execute(pool)
        .await
        .unwrap();
}
