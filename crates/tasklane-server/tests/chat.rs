//! The chat bridge against a real Postgres, a real git remote and the fake plug-in.

use std::path::Path;
use std::process::Command;
use std::str::FromStr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::{ConnectOptions, PgPool};
use tasklane_orchestrator::host::{PluginClient, PluginSpec};
use tasklane_orchestrator::{Config, Orchestrator};
use tasklane_queue::Queue;
use tasklane_server::chat::ChatBridge;

static COUNTER: AtomicU32 = AtomicU32::new(0);
const FAKE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../tasklane-orchestrator/tests/fixtures/fake_plugin.py"
);

struct Env {
    pool: PgPool,
    bridge: ChatBridge,
    dir: tempfile::TempDir,
}

fn sh(dir: &Path, cmd: &str) {
    let o = Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "{cmd}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
}

async fn env() -> Option<Env> {
    let Ok(url) = std::env::var("TASKLANE_TEST_DATABASE_URL") else {
        if std::env::var("TASKLANE_REQUIRE_DB").as_deref() == Ok("1") {
            panic!("TASKLANE_REQUIRE_DB=1 but TASKLANE_TEST_DATABASE_URL is not set");
        }
        eprintln!("skipped: TASKLANE_TEST_DATABASE_URL not set");
        return None;
    };
    let name = format!(
        "tl_chat_{}_{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    );
    let opts = PgConnectOptions::from_str(&url)
        .unwrap()
        .log_statements(log::LevelFilter::Off);
    let mut admin = opts.connect().await.unwrap();
    drop_dead_test_databases(&mut admin).await;
    sqlx::query(&format!("CREATE DATABASE \"{name}\""))
        .execute(&mut admin)
        .await
        .unwrap();
    let pool = PgPoolOptions::new()
        .max_connections(8)
        .connect_with(opts.database(&name))
        .await
        .unwrap();
    tasklane_orchestrator::migrate(&pool).await.unwrap();

    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("state")).unwrap();
    std::fs::create_dir_all(dir.path().join("remote/org")).unwrap();
    let remote = dir.path().join("remote/org/repo.git");
    sh(
        dir.path(),
        &format!("git init -q --bare -b main {}", remote.display()),
    );
    sh(dir.path(), &format!("git init -q -b main seed && cd seed && git config user.email a@b.c && git config user.name t && echo hi > README && git add . && git commit -q -m init && git push -q {} main", remote.display()));

    let spec = || PluginSpec {
        program: "python3".into(),
        args: vec!["-I".into(), FAKE.into()],
        env: vec![
            ("PATH".into(), std::env::var("PATH").unwrap()),
            (
                "FAKE_STATE_DIR".into(),
                dir.path().join("state").display().to_string(),
            ),
            (
                "FAKE_REMOTE_ROOT".into(),
                dir.path().join("remote").display().to_string(),
            ),
        ],
    };
    let mut cfg = Config::new("chat-test", dir.path().join("data"));
    cfg.source_name = "test".into();
    cfg.call_timeout = Duration::from_secs(10);
    let orch = Arc::new(Orchestrator::new(
        pool.clone(),
        Queue::new(pool.clone(), 2),
        PluginClient::spawn(&spec()).unwrap(),
        PluginClient::spawn(&spec()).unwrap(),
        cfg,
    ));
    sqlx::query("INSERT INTO services (kind, source, external_id, mode, facts) VALUES ('repo','test','org/repo','full','{\"default_branch\":\"main\"}')")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO chat_groups (channel, group_ref, service_id, daily_cap) SELECT 'telegram', '-100', id, 2 FROM services").execute(&pool).await.unwrap();
    let bridge = ChatBridge {
        orch,
        chat: PluginClient::spawn(&spec()).unwrap(),
        channel: "telegram".into(),
        user_daily_limit: 3,
    };
    Some(Env { pool, bridge, dir })
}

impl Env {
    fn sent(&self) -> Vec<Value> {
        std::fs::read_to_string(self.dir.path().join("state/sent.json"))
            .map(|s| serde_json::from_str(&s).unwrap())
            .unwrap_or_default()
    }
    fn last_text(&self) -> String {
        self.sent()
            .last()
            .map(|m| m["text"].as_str().unwrap().to_string())
            .unwrap_or_default()
    }
    async fn count(&self, sql: &str) -> i64 {
        sqlx::query_scalar(sql).fetch_one(&self.pool).await.unwrap()
    }
}

fn cmd(group: &str, user: &str, command: &str, args: &str) -> Value {
    json!({"kind": "command", "group": group, "user": user, "user_name": "sam", "command": command, "args": args})
}

#[tokio::test]
async fn anyone_in_a_registered_group_can_ask_and_gets_an_answer_from_a_read_only_checkout() {
    let Some(e) = env().await else { return };
    e.bridge
        .handle_event(&cmd("-100", "555", "ask", "where is the config?"))
        .await;
    assert_eq!(e.last_text(), "The answer is 42.");
    let calls = std::fs::read_to_string(e.dir.path().join("state/ask_calls.log")).unwrap();
    assert!(
        calls.contains("where is the config?") && calls.contains("/ask/"),
        "{calls}"
    );
    assert_eq!(e.count("SELECT count(*) FROM audit_events WHERE action = 'chat.ask' AND actor = 'telegram:-100:555'").await, 1);
    // the checkout was removed again
    assert_eq!(
        std::fs::read_dir(e.dir.path().join("data/ask"))
            .unwrap()
            .count(),
        0
    );
}

#[tokio::test]
async fn the_daily_question_cap_holds_per_group() {
    let Some(e) = env().await else { return };
    for _ in 0..2 {
        e.bridge.handle_event(&cmd("-100", "1", "ask", "q")).await;
    }
    e.bridge
        .handle_event(&cmd("-100", "2", "ask", "one too many"))
        .await;
    assert!(
        e.last_text().contains("used its questions for today"),
        "{}",
        e.last_text()
    );
    let calls = std::fs::read_to_string(e.dir.path().join("state/ask_calls.log")).unwrap();
    assert_eq!(
        calls.lines().count(),
        2,
        "the engine was not called past the cap"
    );
}

#[tokio::test]
async fn failed_questions_count_against_the_daily_cap_too() {
    let Some(e) = env().await else { return };
    std::fs::write(e.dir.path().join("state/ask_fails"), "").unwrap();
    for _ in 0..2 {
        e.bridge.handle_event(&cmd("-100", "1", "ask", "q")).await;
    }
    std::fs::remove_file(e.dir.path().join("state/ask_fails")).unwrap();
    e.bridge
        .handle_event(&cmd("-100", "1", "ask", "now it would work"))
        .await;
    assert!(
        e.last_text().contains("used its questions for today"),
        "{}",
        e.last_text()
    );
}

#[tokio::test]
async fn a_failed_answer_says_so_without_leaking_the_error() {
    let Some(e) = env().await else { return };
    std::fs::write(e.dir.path().join("state/ask_fails"), "").unwrap();
    e.bridge.handle_event(&cmd("-100", "1", "ask", "q")).await;
    assert_eq!(e.last_text(), "I could not answer that right now.");
}

#[tokio::test]
async fn task_creates_a_ticket_once_and_respects_the_limit() {
    let Some(e) = env().await else { return };
    e.bridge
        .handle_event(&cmd(
            "-100",
            "555",
            "task",
            "Fix the login page\nIt shows a blank screen",
        ))
        .await;
    assert!(
        e.last_text().starts_with("Created ticket #100"),
        "{}",
        e.last_text()
    );
    assert_eq!(
        e.count("SELECT count(*) FROM jobs WHERE kind = 'triage' AND status = 'queued'")
            .await,
        1
    );
    let (reporter, title): (String, String) =
        sqlx::query_as("SELECT reporter_ref, title FROM tickets")
            .fetch_one(&e.pool)
            .await
            .unwrap();
    assert_eq!(
        (reporter.as_str(), title.as_str()),
        ("telegram:555", "Fix the login page")
    );

    e.bridge
        .handle_event(&cmd("-100", "555", "task", "fix the LOGIN page"))
        .await;
    assert!(
        e.last_text().starts_with("You already reported this"),
        "{}",
        e.last_text()
    );
    e.bridge
        .handle_event(&cmd("-100", "555", "task", "second"))
        .await;
    e.bridge
        .handle_event(&cmd("-100", "555", "task", "third"))
        .await;
    e.bridge
        .handle_event(&cmd("-100", "555", "task", "fourth"))
        .await;
    assert!(e.last_text().contains("daily limit"), "{}", e.last_text());
    e.bridge.handle_event(&cmd("-100", "555", "task", "")).await;
    assert!(e.last_text().starts_with("Usage"));
}

#[tokio::test]
async fn an_unregistered_group_gets_one_hint_with_its_id_and_nothing_else() {
    let Some(e) = env().await else { return };
    e.bridge
        .handle_event(&cmd("-999", "1", "ask", "show me the secrets"))
        .await;
    e.bridge
        .handle_event(&cmd("-999", "1", "task", "do something"))
        .await;
    e.bridge.handle_event(&json!({"kind": "button", "group": "-999", "user": "1", "data": "a:1:aaaaaaaaaaaaaaaa"})).await;
    let sent = e.sent();
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert!(sent[0]["text"].as_str().unwrap().contains("Group id: -999"));
    assert!(
        !e.dir.path().join("state/ask_calls.log").exists(),
        "the engine was never called"
    );
    assert_eq!(e.count("SELECT count(*) FROM tickets").await, 0);
}

#[tokio::test]
async fn plain_chatter_is_ignored() {
    let Some(e) = env().await else { return };
    e.bridge
        .handle_event(&json!({"kind": "message", "group": "-100", "user": "1", "text": "lunch?"}))
        .await;
    assert!(e.sent().is_empty());
}

async fn awaiting(e: &Env) -> (i64, String) {
    let (id, hash): (i64, String) = sqlx::query_as(
        "INSERT INTO tickets (service_id, external_ref, state, level, plan_hash, title) SELECT id, '8', 'awaiting_approval', 2, repeat('ab', 32), 'Rotate keys' FROM services RETURNING id, plan_hash",
    )
    .fetch_one(&e.pool)
    .await
    .unwrap();
    (id, hash)
}

#[tokio::test]
async fn approval_requests_are_announced_once_with_buttons_and_only_approvers_can_press_them() {
    let Some(e) = env().await else { return };
    let (id, hash) = awaiting(&e).await;
    e.bridge.announce_approvals().await;
    e.bridge.announce_approvals().await;
    let sent = e.sent();
    assert_eq!(sent.len(), 1, "announced once");
    assert_eq!(
        sent[0]["buttons"][0]["data"],
        format!("a:{id}:{}", &hash[..16])
    );
    assert!(sent[0]["text"].as_str().unwrap().contains("Rotate keys"));

    let press = |user: &str, verb: &str| json!({"kind": "button", "group": "-100", "user": user, "user_name": "pat", "data": format!("{verb}:{id}:{}", &hash[..16])});
    e.bridge.handle_event(&press("777", "a")).await;
    assert!(
        e.last_text()
            .starts_with("Not done: you are not registered as an approver"),
        "{}",
        e.last_text()
    );
    assert_eq!(
        e.count("SELECT count(*) FROM tickets WHERE state = 'awaiting_approval'")
            .await,
        1
    );

    sqlx::query(
        "INSERT INTO principals (channel, ref, role) VALUES ('telegram', '888', 'team_approver')",
    )
    .execute(&e.pool)
    .await
    .unwrap();
    e.bridge.handle_event(&press("888", "a")).await;
    assert!(
        e.last_text().contains("approved by pat"),
        "{}",
        e.last_text()
    );
    assert_eq!(
        e.count("SELECT count(*) FROM tickets WHERE state = 'queued'")
            .await,
        1
    );
    assert_eq!(
        e.count("SELECT count(*) FROM approvals WHERE approver = 'telegram:888' AND via = 'chat'")
            .await,
        1
    );
    e.bridge.handle_event(&press("888", "a")).await;
    assert!(
        e.last_text().starts_with("Not done"),
        "no double approval: {}",
        e.last_text()
    );
}

#[tokio::test]
async fn buttons_with_a_wrong_hash_a_wrong_ticket_or_from_another_groups_service_do_nothing() {
    let Some(e) = env().await else { return };
    let (id, hash) = awaiting(&e).await;
    sqlx::query("INSERT INTO principals (channel, ref, role) VALUES ('telegram', '888', 'owner')")
        .execute(&e.pool)
        .await
        .unwrap();
    let ev = |data: String| json!({"kind": "button", "group": "-100", "user": "888", "data": data});
    e.bridge
        .handle_event(&ev(format!("a:{id}:0000000000000000")))
        .await;
    assert!(e.last_text().contains("out of date"), "{}", e.last_text());
    e.bridge
        .handle_event(&ev(format!("a:{}:{}", id + 50, &hash[..16])))
        .await;
    assert!(e.last_text().contains("out of date"));
    e.bridge.handle_event(&ev("garbage".into())).await;
    e.bridge.handle_event(&ev(format!("a:{id}:ab"))).await; // prefix too short to bind to a plan
    assert_eq!(
        e.count("SELECT count(*) FROM tickets WHERE state = 'awaiting_approval'")
            .await,
        1
    );
}

#[tokio::test]
async fn the_group_that_reported_a_ticket_is_told_how_it_ended_once_per_outcome() {
    let Some(e) = env().await else { return };
    e.bridge
        .handle_event(&cmd("-100", "555", "task", "Fix the login page"))
        .await;
    let sent_before = e.sent().len();
    // nothing to announce while it is being worked on
    e.bridge.announce_outcomes().await;
    assert_eq!(e.sent().len(), sent_before);
    sqlx::query("UPDATE tickets SET state = 'merged'")
        .execute(&e.pool)
        .await
        .unwrap();
    e.bridge.announce_outcomes().await;
    e.bridge.announce_outcomes().await;
    assert_eq!(e.sent().len(), sent_before + 1, "announced once");
    assert!(
        e.last_text().contains("#100")
            && e.last_text().contains("merged into the development branch"),
        "{}",
        e.last_text()
    );
    sqlx::query("UPDATE tickets SET state = 'failed', last_error = 'CI failed twice'")
        .execute(&e.pool)
        .await
        .unwrap();
    e.bridge.announce_outcomes().await;
    assert!(
        e.last_text().contains("needs a person") && !e.last_text().contains("CI failed twice"),
        "{}",
        e.last_text()
    );
    // a ticket made some other way (no chat group) is never announced here
    sqlx::query("UPDATE tickets SET notify_ref = NULL, state = 'closed_done'")
        .execute(&e.pool)
        .await
        .unwrap();
    let n = e.sent().len();
    e.bridge.announce_outcomes().await;
    assert_eq!(e.sent().len(), n);
}

/// Drop the databases earlier test runs left behind (`tl_<kind>_<pid>_<n>`) whose test process is
/// gone, so a long-lived test Postgres does not fill up with thousands of them.
async fn drop_dead_test_databases(admin: &mut sqlx::PgConnection) {
    let names: Vec<String> =
        sqlx::query_scalar("SELECT datname FROM pg_database WHERE datname LIKE 'tl\\_%'")
            .fetch_all(&mut *admin)
            .await
            .unwrap_or_default();
    for n in names {
        let pid = n.split('_').nth(2).and_then(|p| p.parse::<u32>().ok());
        if let Some(pid) = pid {
            if !std::path::Path::new(&format!("/proc/{pid}")).exists() {
                let _ = sqlx::query(&format!("DROP DATABASE IF EXISTS \"{n}\" WITH (FORCE)"))
                    .execute(&mut *admin)
                    .await;
            }
        }
    }
}
