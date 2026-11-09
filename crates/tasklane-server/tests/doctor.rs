//! `tasklane doctor` against a real database and the fake plug-in.

use std::collections::HashMap;

use sqlx::postgres::PgPoolOptions;
use tasklane_server::config::Settings;
use tasklane_server::doctor::{run, Level};

fn settings(db: &str, source_cmd: &str, data: &std::path::Path) -> Settings {
    let fake = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../tasklane-orchestrator/tests/fixtures/fake_plugin.py"
    );
    let vars: HashMap<String, String> = [
        ("TASKLANE_DATABASE_URL", db.to_string()),
        (
            "TASKLANE_ADMIN_TOKEN",
            "a-long-enough-admin-token-123".into(),
        ),
        ("TASKLANE_SOURCE_CMD", source_cmd.replace("FAKE", fake)),
        ("TASKLANE_ENGINE_CMD", format!("python3 -I {fake}")),
        ("TASKLANE_DATA_DIR", data.display().to_string()),
        ("PATH", std::env::var("PATH").unwrap()),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect();
    Settings::from_map(&vars).unwrap()
}

#[tokio::test]
async fn doctor_reports_what_works_and_what_does_not() {
    let Ok(url) = std::env::var("TASKLANE_TEST_DATABASE_URL") else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&url)
        .await
        .unwrap();
    let s = settings(&url, "python3 -I FAKE", dir.path());
    let checks = run(&s, Some(&pool), None).await;
    let level = |what: &str| checks.iter().find(|c| c.what == what).map(|c| &c.level);
    assert_eq!(level("database"), Some(&Level::Ok));
    assert_eq!(level("git"), Some(&Level::Ok));
    assert_eq!(level("data directory"), Some(&Level::Ok));
    assert_eq!(level("ticket source plug-in"), Some(&Level::Ok));
    assert_eq!(level("agent engine plug-in"), Some(&Level::Ok));
    assert_eq!(
        level("agent sandbox"),
        Some(&Level::Warn),
        "no sandbox in the environment is a warning, not a failure"
    );
    assert_eq!(level("public address"), Some(&Level::Warn));

    // a plug-in command that does not exist, and no database, are failures that say what to do
    let bad = settings(&url, "/no/such/program", dir.path());
    let checks = run(&bad, None, Some("connection refused".into())).await;
    let fail = |what: &str| checks.iter().find(|c| c.what == what).unwrap();
    assert_eq!(fail("database").level, Level::Fail);
    assert!(fail("database").detail.contains("TASKLANE_DATABASE_URL"));
    assert_eq!(fail("ticket source plug-in").level, Level::Fail);
    assert!(fail("ticket source plug-in")
        .detail
        .contains("could not start"));
}
