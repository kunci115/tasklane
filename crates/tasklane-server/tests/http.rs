//! The HTTP surface against a real Postgres and the fake plug-in.

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::{ConnectOptions, PgPool};
use tasklane_orchestrator::host::{PluginClient, PluginSpec};
use tasklane_orchestrator::{Config, Orchestrator, RunResult};
use tasklane_queue::Queue;
use tasklane_server::auth::sign_hs256;
use tasklane_server::{api, console, now_unix, AppState};
use tower::ServiceExt;

static COUNTER: AtomicU32 = AtomicU32::new(0);
const FAKE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../tasklane-orchestrator/tests/fixtures/fake_plugin.py"
);
const ADMIN: &str = "an-admin-token-that-is-long-enough";
const SECRET: &str = "platform-shared-secret-123";

struct Env {
    pool: PgPool,
    app: Router,
    orch: Arc<Orchestrator>,
    dir: tempfile::TempDir,
    cookie: String,
    csrf: String,
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
        "tl_http_{}_{}",
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
    tasklane_server::runtime::ensure_admin(&pool).await.unwrap();

    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("state")).unwrap();
    let spec = || PluginSpec {
        program: "python3".into(),
        args: vec!["-I".into(), FAKE.into()],
        env: vec![
            ("PATH".into(), std::env::var("PATH").unwrap()),
            (
                "FAKE_STATE_DIR".into(),
                dir.path().join("state").display().to_string(),
            ),
            ("FAKE_REMOTE_ROOT".into(), dir.path().display().to_string()),
        ],
    };
    // the orchestrator reads document credentials from <its data dir>/secrets, where the console writes them
    std::fs::create_dir_all(dir.path().join("data")).unwrap();
    std::fs::create_dir_all(dir.path().join("secrets")).unwrap();
    let _ = std::os::unix::fs::symlink(dir.path().join("secrets"), dir.path().join("data/secrets"));
    let mut cfg = Config::new("http-test", dir.path().join("data"));
    cfg.source_name = "test".into();
    cfg.call_timeout = Duration::from_secs(10);
    let orch = Arc::new(Orchestrator::new(
        pool.clone(),
        Queue::new(pool.clone(), 2),
        PluginClient::spawn(&spec()).unwrap(),
        PluginClient::spawn(&spec()).unwrap(),
        cfg,
    ));
    sqlx::query("INSERT INTO services (kind, source, external_id, mode, facts) VALUES ('repo','test','org/repo','triage','{}'), ('repo','test','org/watched','watch','{}')")
        .execute(&pool)
        .await
        .unwrap();
    let state = Arc::new(AppState {
        orch: orch.clone(),
        admin_token: ADMIN.into(),
        jwt_secrets: HashMap::from([("shop".to_string(), SECRET.to_string())]),
        user_daily_limit: 2,
        secure_cookies: false,
        login_throttle: Default::default(),
        secrets: tasklane_server::connections::Secrets::new(dir.path()).unwrap(),
        base_source: spec(),
        base_engine: spec(),
        public_url: None,
        metrics_token: Some("metrics-token-for-tests-1234".into()),
    });
    let app = api::routes()
        .merge(console::routes(state.clone()))
        .with_state(state);
    let (st, h, _) = send(&app, form("/login", format!("token={ADMIN}"), None)).await;
    assert_eq!(st, StatusCode::SEE_OTHER);
    let cookie = h[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    let csrf: String = sqlx::query_scalar("SELECT csrf FROM console_sessions WHERE id = $1")
        .bind(cookie.trim_start_matches("tl="))
        .fetch_one(&pool)
        .await
        .unwrap();
    Some(Env {
        pool,
        app,
        orch,
        dir,
        cookie,
        csrf,
    })
}

async fn send(app: &Router, req: Request<Body>) -> (StatusCode, axum::http::HeaderMap, String) {
    let res = app.clone().oneshot(req).await.unwrap();
    let (status, headers) = (res.status(), res.headers().clone());
    let body =
        String::from_utf8_lossy(&res.into_body().collect().await.unwrap().to_bytes()).into_owned();
    (status, headers, body)
}

fn token(claims: Value, secret: &str) -> String {
    sign_hs256(&claims, secret.as_bytes())
}
fn good(sub: &str, svc: &str) -> String {
    token(
        json!({"iss": "shop", "sub": sub, "svc": svc, "exp": now_unix() + 300}),
        SECRET,
    )
}
fn report(tok: &str, title: &str) -> Request<Body> {
    Request::post("/api/v1/report")
        .header(header::AUTHORIZATION, format!("Bearer {tok}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({"title": title, "body": "it broke"}).to_string(),
        ))
        .unwrap()
}
fn form(path: &str, body: String, cookie: Option<String>) -> Request<Body> {
    let mut r =
        Request::post(path).header(header::CONTENT_TYPE, "application/x-www-form-urlencoded");
    if let Some(c) = cookie {
        r = r.header(header::COOKIE, c);
    }
    r.body(Body::from(body)).unwrap()
}
fn get(path: &str, cookie: Option<String>) -> Request<Body> {
    let mut r = Request::get(path);
    if let Some(c) = cookie {
        r = r.header(header::COOKIE, c);
    }
    r.body(Body::empty()).unwrap()
}

#[tokio::test]
async fn health_and_readiness() {
    let Some(e) = env().await else { return };
    assert_eq!(send(&e.app, get("/healthz", None)).await.0, StatusCode::OK);
    assert_eq!(send(&e.app, get("/readyz", None)).await.0, StatusCode::OK);
}

#[tokio::test]
async fn a_signed_report_creates_one_ticket_and_a_triage_job() {
    let Some(e) = env().await else { return };
    let (st, _, body) = send(&e.app, report(&good("u1", "org/repo"), "Printing fails")).await;
    assert_eq!(st, StatusCode::CREATED, "{body}");
    let v: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        (v["ref"].as_str(), v["duplicate"].as_bool()),
        (Some("100"), Some(false))
    );
    let (reporter, title): (String, String) =
        sqlx::query_as("SELECT reporter_ref, title FROM tickets")
            .fetch_one(&e.pool)
            .await
            .unwrap();
    assert_eq!(
        (reporter.as_str(), title.as_str()),
        ("shop:u1", "Printing fails")
    );
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM jobs WHERE kind = 'triage'")
        .fetch_one(&e.pool)
        .await
        .unwrap();
    assert_eq!(n, 1);
    let created = std::fs::read_to_string(e.dir.path().join("state/created.json")).unwrap();
    assert!(created.contains("Reported through shop by u1"));
}

#[tokio::test]
async fn the_same_title_again_is_the_same_report_and_the_daily_limit_holds() {
    let Some(e) = env().await else { return };
    let tok = good("u1", "org/repo");
    assert_eq!(
        send(&e.app, report(&tok, "Printing fails")).await.0,
        StatusCode::CREATED
    );
    let (st, _, body) = send(&e.app, report(&tok, "PRINTING FAILS")).await;
    assert_eq!(st, StatusCode::OK);
    assert!(body.contains("\"duplicate\":true"));
    assert_eq!(
        send(&e.app, report(&tok, "Second problem")).await.0,
        StatusCode::CREATED
    );
    assert_eq!(
        send(&e.app, report(&tok, "Third problem")).await.0,
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(
        send(&e.app, report(&good("u2", "org/repo"), "Third problem"))
            .await
            .0,
        StatusCode::CREATED,
        "limits are per reporter"
    );
}

#[tokio::test]
async fn bad_tokens_unknown_services_and_read_only_repos_are_refused() {
    let Some(e) = env().await else { return };
    let wrong_secret = token(
        json!({"iss": "shop", "sub": "u", "svc": "org/repo", "exp": now_unix() + 300}),
        "another-secret-value-1",
    );
    let expired = token(
        json!({"iss": "shop", "sub": "u", "svc": "org/repo", "exp": now_unix() - 1000}),
        SECRET,
    );
    let unknown_platform = token(
        json!({"iss": "evil", "sub": "u", "svc": "org/repo", "exp": now_unix() + 300}),
        SECRET,
    );
    let no_svc = token(
        json!({"iss": "shop", "sub": "u", "exp": now_unix() + 300}),
        SECRET,
    );
    for (tok, want) in [
        (wrong_secret, 401),
        (expired, 401),
        (unknown_platform, 401),
        (no_svc, 401),
    ] {
        assert_eq!(send(&e.app, report(&tok, "x")).await.0.as_u16(), want);
    }
    assert_eq!(
        send(
            &e.app,
            Request::post("/api/v1/report")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from("{\"title\":\"x\"}"))
                .unwrap()
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        send(&e.app, report(&good("u", "org/nope"), "x")).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        send(&e.app, report(&good("u", "org/watched"), "x")).await.0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        send(&e.app, report(&good("u", "org/repo"), "   ")).await.0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM tickets")
        .fetch_one(&e.pool)
        .await
        .unwrap();
    assert_eq!(n, 0, "nothing was created by any refused request");
}

#[tokio::test]
async fn a_verified_webhook_creates_the_ticket_and_an_unverified_one_does_nothing() {
    let Some(e) = env().await else { return };
    let ev = json!({"event": {"service": "org/repo", "ref": "55", "kind": "issue"}}).to_string();
    let bad = Request::post("/api/v1/webhook/source")
        .header("x-sig", "forged")
        .body(Body::from(ev.clone()))
        .unwrap();
    assert_eq!(send(&e.app, bad).await.0, StatusCode::UNAUTHORIZED);
    // the fake verifier returns the body's "event" object as the event
    let ok = Request::post("/api/v1/webhook/source")
        .header("x-sig", "good")
        .body(Body::from(
            json!({"service": "org/repo", "ref": "55", "kind": "issue"}).to_string(),
        ))
        .unwrap();
    assert_eq!(send(&e.app, ok).await.0, StatusCode::ACCEPTED);
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM tickets WHERE external_ref = '55'")
        .fetch_one(&e.pool)
        .await
        .unwrap();
    assert_eq!(n, 1);
}

#[tokio::test]
async fn the_console_needs_the_admin_token() {
    let Some(e) = env().await else { return };
    let (st, h, _) = send(&e.app, get("/", None)).await;
    assert_eq!(
        (st, h[header::LOCATION].to_str().unwrap()),
        (StatusCode::SEE_OTHER, "/login")
    );
    assert_eq!(
        send(&e.app, get("/services", None)).await.0,
        StatusCode::SEE_OTHER
    );
    assert_eq!(
        send(&e.app, get("/", Some("tl=forged".into()))).await.0,
        StatusCode::SEE_OTHER
    );
    assert_eq!(
        send(&e.app, form("/login", "token=wrong".into(), None))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    let (st, h, _) = send(&e.app, form("/login", format!("token={ADMIN}"), None)).await;
    assert_eq!(st, StatusCode::SEE_OTHER);
    let cookie = h[header::SET_COOKIE].to_str().unwrap();
    assert!(
        cookie.contains("HttpOnly") && cookie.contains("SameSite=Lax"),
        "{cookie}"
    );
    assert_eq!(
        send(&e.app, get("/", Some(e.cookie.clone()))).await.0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn ticket_text_is_escaped_in_the_console() {
    let Some(e) = env().await else { return };
    sqlx::query("INSERT INTO tickets (service_id, external_ref, title) SELECT id, '1', '<script>alert(1)</script>' FROM services WHERE external_id = 'org/repo'")
        .execute(&e.pool).await.unwrap();
    let (_, _, home) = send(&e.app, get("/", Some(e.cookie.clone()))).await;
    assert!(
        home.contains("&lt;script&gt;alert(1)&lt;/script&gt;") && !home.contains("<script>alert"),
        "{home}"
    );
}

#[tokio::test]
async fn approving_in_the_console_needs_csrf_and_the_current_plan_then_work_starts() {
    let Some(e) = env().await else { return };
    let (t,): (i64,) = sqlx::query_as("INSERT INTO tickets (service_id, external_ref, state, level, plan_hash) SELECT id, '7', 'awaiting_approval', 2, 'abc123' FROM services WHERE external_id = 'org/repo' RETURNING id")
        .fetch_one(&e.pool).await.unwrap();
    let page = send(
        &e.app,
        get(&format!("/tickets/{t}"), Some(e.cookie.clone())),
    )
    .await
    .2;
    assert!(page.contains("Approve") && page.contains("abc123"));

    let path = format!("/tickets/{t}/decide");
    assert_eq!(
        send(
            &e.app,
            form(
                &path,
                "csrf=nope&plan_hash=abc123&decision=approve".into(),
                Some(e.cookie.clone())
            )
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        send(
            &e.app,
            form(
                &path,
                format!("csrf={}&plan_hash=abc123&decision=approve", e.csrf),
                None
            )
        )
        .await
        .0,
        StatusCode::FORBIDDEN,
        "no session"
    );
    let (st, _, body) = send(
        &e.app,
        form(
            &path,
            format!("csrf={}&plan_hash=stale&decision=approve", e.csrf),
            Some(e.cookie.clone()),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT);
    assert!(body.contains("plan changed"));
    let state: String = sqlx::query_scalar("SELECT state FROM tickets WHERE id = $1")
        .bind(t)
        .fetch_one(&e.pool)
        .await
        .unwrap();
    assert_eq!(state, "awaiting_approval");

    assert_eq!(
        send(
            &e.app,
            form(
                &path,
                format!("csrf={}&plan_hash=abc123&decision=approve", e.csrf),
                Some(e.cookie.clone())
            )
        )
        .await
        .0,
        StatusCode::SEE_OTHER
    );
    let state: String = sqlx::query_scalar("SELECT state FROM tickets WHERE id = $1")
        .bind(t)
        .fetch_one(&e.pool)
        .await
        .unwrap();
    assert_eq!(state, "queued");
    let mode: String =
        sqlx::query_scalar("SELECT mode FROM services WHERE external_id = 'org/repo'")
            .fetch_one(&e.pool)
            .await
            .unwrap();
    assert_eq!(
        mode, "triage",
        "an approval never raises the mode by itself"
    );
}

#[tokio::test]
async fn pause_stops_all_work_and_resume_restarts_it() {
    let Some(e) = env().await else { return };
    let t: i64 = sqlx::query_scalar("INSERT INTO tickets (service_id, external_ref) SELECT id, '9' FROM services WHERE external_id = 'org/repo' RETURNING id").fetch_one(&e.pool).await.unwrap();
    e.orch.queue().enqueue(t, "triage", 0, 0.0).await.unwrap();
    assert_eq!(
        send(
            &e.app,
            form("/pause", "csrf=bad".into(), Some(e.cookie.clone()))
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        send(
            &e.app,
            form("/pause", format!("csrf={}", e.csrf), Some(e.cookie.clone()))
        )
        .await
        .0,
        StatusCode::SEE_OTHER
    );
    assert_eq!(
        e.orch.run_one().await.unwrap(),
        RunResult::Idle,
        "paused: nothing is claimed"
    );
    assert_eq!(
        send(
            &e.app,
            form(
                "/resume",
                format!("csrf={}", e.csrf),
                Some(e.cookie.clone())
            )
        )
        .await
        .0,
        StatusCode::SEE_OTHER
    );
    assert_ne!(e.orch.run_one().await.unwrap(), RunResult::Idle);
}

#[tokio::test]
async fn changing_a_repository_mode_is_audited_and_validated() {
    let Some(e) = env().await else { return };
    let id: i64 = sqlx::query_scalar("SELECT id FROM services WHERE external_id = 'org/watched'")
        .fetch_one(&e.pool)
        .await
        .unwrap();
    let path = format!("/services/{id}/mode");
    assert_eq!(
        send(
            &e.app,
            form(
                &path,
                format!("csrf={}&mode=turbo", e.csrf),
                Some(e.cookie.clone())
            )
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        send(
            &e.app,
            form(
                &path,
                format!("csrf={}&mode=triage", e.csrf),
                Some(e.cookie.clone())
            )
        )
        .await
        .0,
        StatusCode::SEE_OTHER
    );
    let mode: String = sqlx::query_scalar("SELECT mode FROM services WHERE id = $1")
        .bind(id)
        .fetch_one(&e.pool)
        .await
        .unwrap();
    assert_eq!(mode, "triage");
    let n: i64 =
        sqlx::query_scalar("SELECT count(*) FROM audit_events WHERE action = 'service.mode'")
            .fetch_one(&e.pool)
            .await
            .unwrap();
    assert_eq!(n, 1);
}

#[tokio::test]
async fn each_sign_in_gets_its_own_random_session_and_csrf_token() {
    let Some(e) = env().await else { return };
    let (_, h2, _) = send(&e.app, form("/login", format!("token={ADMIN}"), None)).await;
    let second = h2[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    assert_ne!(second, e.cookie);
    let csrf2: String = sqlx::query_scalar("SELECT csrf FROM console_sessions WHERE id = $1")
        .bind(second.trim_start_matches("tl="))
        .fetch_one(&e.pool)
        .await
        .unwrap();
    assert_ne!(csrf2, e.csrf);
    // a form token from one session does not work in another
    assert_eq!(
        send(
            &e.app,
            form("/pause", format!("csrf={}", e.csrf), Some(second.clone()))
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        send(
            &e.app,
            form("/pause", format!("csrf={csrf2}"), Some(second))
        )
        .await
        .0,
        StatusCode::SEE_OTHER
    );
}

#[tokio::test]
async fn signing_out_ends_the_session_on_the_server_and_expired_sessions_stop_working() {
    let Some(e) = env().await else { return };
    assert_eq!(
        send(&e.app, get("/", Some(e.cookie.clone()))).await.0,
        StatusCode::OK
    );
    sqlx::query("UPDATE console_sessions SET expires_at = now() - interval '1 second'")
        .execute(&e.pool)
        .await
        .unwrap();
    assert_eq!(
        send(&e.app, get("/", Some(e.cookie.clone()))).await.0,
        StatusCode::SEE_OTHER,
        "expired on the server"
    );
    let (_, h, _) = send(&e.app, form("/login", format!("token={ADMIN}"), None)).await;
    let fresh = h[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    let csrf: String = sqlx::query_scalar("SELECT csrf FROM console_sessions WHERE id = $1")
        .bind(fresh.trim_start_matches("tl="))
        .fetch_one(&e.pool)
        .await
        .unwrap();
    assert_eq!(
        send(
            &e.app,
            form("/logout", format!("csrf={csrf}"), Some(fresh.clone()))
        )
        .await
        .0,
        StatusCode::SEE_OTHER
    );
    assert_eq!(
        send(&e.app, get("/", Some(fresh))).await.0,
        StatusCode::SEE_OTHER,
        "the old cookie is dead after sign-out"
    );
}

#[tokio::test]
async fn five_wrong_tokens_lock_sign_in_for_a_while() {
    let Some(e) = env().await else { return };
    for _ in 0..5 {
        assert_eq!(
            send(&e.app, form("/login", "token=nope".into(), None))
                .await
                .0,
            StatusCode::UNAUTHORIZED
        );
    }
    assert_eq!(
        send(&e.app, form("/login", format!("token={ADMIN}"), None))
            .await
            .0,
        StatusCode::TOO_MANY_REQUESTS,
        "even the right token waits"
    );
}

fn login_from(peer: &str, forwarded: Option<&str>, token: &str) -> Request<Body> {
    let mut r = form("/login", format!("token={token}"), None);
    r.extensions_mut().insert(axum::extract::ConnectInfo(
        format!("{peer}:5000")
            .parse::<std::net::SocketAddr>()
            .unwrap(),
    ));
    if let Some(f) = forwarded {
        r.headers_mut()
            .insert("x-forwarded-for", f.parse().unwrap());
    }
    r
}

#[tokio::test]
async fn wrong_tokens_from_one_client_do_not_lock_out_another() {
    let Some(e) = env().await else { return };
    for _ in 0..5 {
        let (st, _, _) = send(&e.app, login_from("203.0.113.9", None, "nope")).await;
        assert_eq!(st, StatusCode::UNAUTHORIZED);
    }
    let (st, _, _) = send(&e.app, login_from("203.0.113.9", None, ADMIN)).await;
    assert_eq!(st, StatusCode::TOO_MANY_REQUESTS, "the guesser waits");
    let (st, _, _) = send(&e.app, login_from("198.51.100.4", None, ADMIN)).await;
    assert_eq!(st, StatusCode::SEE_OTHER, "somebody else signs in");
    // behind a proxy on the private network the client is the address the proxy reports
    for _ in 0..5 {
        send(&e.app, login_from("10.0.0.2", Some("192.0.2.7"), "nope")).await;
    }
    let (st, _, _) = send(&e.app, login_from("10.0.0.2", Some("192.0.2.7"), ADMIN)).await;
    assert_eq!(st, StatusCode::TOO_MANY_REQUESTS);
    let (st, _, _) = send(&e.app, login_from("10.0.0.2", Some("192.0.2.8"), ADMIN)).await;
    assert_eq!(st, StatusCode::SEE_OTHER);
}

#[tokio::test]
async fn the_approver_sees_the_plan_and_the_change_and_a_failed_ticket_can_be_retried() {
    let Some(e) = env().await else { return };
    let (t,): (i64,) = sqlx::query_as("INSERT INTO tickets (service_id, external_ref, state, level, plan_hash, plan_text, diff_text) SELECT id, '3', 'awaiting_approval', 2, 'abc123', 'Plan: <b>rotate</b>', E'diff --git a/README.md b/README.md\\n--- a/README.md\\n+++ b/README.md\\n@@ -1 +1,2 @@\\n keep\\n+new line' FROM services WHERE external_id = 'org/repo' RETURNING id")
        .fetch_one(&e.pool).await.unwrap();
    let page = send(
        &e.app,
        get(&format!("/tickets/{t}"), Some(e.cookie.clone())),
    )
    .await
    .2;
    assert!(
        page.contains("Plan: &lt;b&gt;rotate&lt;/b&gt;")
            && page.contains("README.md")
            && page.contains("dl add")
            && page.contains("new line"),
        "{page}"
    );
    sqlx::query("UPDATE tickets SET state = 'failed' WHERE id = $1")
        .bind(t)
        .execute(&e.pool)
        .await
        .unwrap();
    let page = send(
        &e.app,
        get(&format!("/tickets/{t}"), Some(e.cookie.clone())),
    )
    .await
    .2;
    assert!(page.contains("Try again") && page.contains("Close it"));
    let path = format!("/tickets/{t}/retry");
    assert_eq!(
        send(
            &e.app,
            form(
                &path,
                "csrf=bad&action=retry".into(),
                Some(e.cookie.clone())
            )
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        send(
            &e.app,
            form(
                &path,
                format!("csrf={}&action=retry", e.csrf),
                Some(e.cookie.clone())
            )
        )
        .await
        .0,
        StatusCode::SEE_OTHER
    );
    let state: String = sqlx::query_scalar("SELECT state FROM tickets WHERE id = $1")
        .bind(t)
        .fetch_one(&e.pool)
        .await
        .unwrap();
    assert_eq!(state, "queued");
}

// ---- the redesigned console ----

async fn seed_ticket(e: &Env, ext: &str, title: &str, state: &str, level: i16) -> i64 {
    sqlx::query_scalar("INSERT INTO tickets (service_id, external_ref, title, state, level, updated_at) SELECT id, $1, $2, $3, $4, now() FROM services WHERE external_id = 'org/repo' RETURNING id")
        .bind(ext)
        .bind(title)
        .bind(state)
        .bind(level)
        .fetch_one(&e.pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn the_overview_shows_what_needs_you_and_a_board_and_its_live_part_needs_a_session() {
    let Some(e) = env().await else { return };
    seed_ticket(&e, "1", "Rotate the <key>", "awaiting_approval", 3).await;
    seed_ticket(&e, "2", "Fix the footer", "ci_running", 1).await;
    seed_ticket(&e, "3", "Old fix", "merged", 1).await;
    let (st, _, page) = send(&e.app, get("/", Some(e.cookie.clone()))).await;
    assert_eq!(st, StatusCode::OK);
    assert!(
        page.contains("Needs you")
            && page.contains("Pipeline")
            && page.contains("data-live=\"/fragments/overview\"")
    );
    assert!(
        page.contains("Rotate the &lt;key&gt;") && !page.contains("Rotate the <key>"),
        "ticket text is escaped"
    );
    assert!(
        page.contains("waiting for your decision") && page.contains("merged in the last 7 days")
    );
    // the live fragment is only for signed-in users
    assert_eq!(
        send(&e.app, get("/fragments/overview", None)).await.0,
        StatusCode::SEE_OTHER
    );
    let (st, _, frag) = send(&e.app, get("/fragments/overview", Some(e.cookie.clone()))).await;
    assert_eq!(st, StatusCode::OK);
    assert!(
        !frag.contains("<html") && frag.contains("Fix the footer"),
        "a fragment, not a page"
    );
}

#[tokio::test]
async fn the_ticket_list_filters_by_view_and_searches_without_trusting_the_query() {
    let Some(e) = env().await else { return };
    seed_ticket(&e, "1", "Printing fails", "failed", 1).await;
    seed_ticket(&e, "2", "Footer typo", "merged", 1).await;
    let (_, _, all) = send(&e.app, get("/tickets", Some(e.cookie.clone()))).await;
    assert!(all.contains("Printing fails") && all.contains("Footer typo"));
    let (_, _, failed) = send(&e.app, get("/tickets?view=failed", Some(e.cookie.clone()))).await;
    assert!(failed.contains("Printing fails") && !failed.contains("Footer typo"));
    let (_, _, found) = send(&e.app, get("/tickets?q=footer", Some(e.cookie.clone()))).await;
    assert!(found.contains("Footer typo") && !found.contains("Printing fails"));
    let (st, _, evil) = send(
        &e.app,
        get(
            "/tickets?q=%22%3E%3Cscript%3Ealert(1)%3C/script%3E%25",
            Some(e.cookie.clone()),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert!(
        !evil.contains("<script>alert(1)</script>"),
        "the search text is escaped"
    );
    assert_eq!(
        send(&e.app, get("/tickets", None)).await.0,
        StatusCode::SEE_OTHER
    );
}

#[tokio::test]
async fn the_ticket_page_has_a_stepper_a_timeline_and_live_parts_and_unknown_tickets_are_a_404() {
    let Some(e) = env().await else { return };
    let t = seed_ticket(&e, "9", "Add a line", "ci_running", 1).await;
    sqlx::query("INSERT INTO audit_events (actor, action, ticket_id, detail) VALUES ('orchestrator','ticket.ci_green',$1,'{}'), ('orchestrator','ticket.failed',$1,'{\"reason\":\"x <b>\"}')").bind(t).execute(&e.pool).await.unwrap();
    let (st, _, page) = send(
        &e.app,
        get(&format!("/tickets/{t}"), Some(e.cookie.clone())),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert!(
        page.contains("class=\"stepper\"")
            && page.contains("What happened")
            && page.contains("CI passed")
    );
    assert!(page.contains("x &lt;b&gt;"), "reasons are escaped");
    for part in ["steps", "timeline"] {
        let (st, _, frag) = send(
            &e.app,
            get(
                &format!("/fragments/ticket/{t}?part={part}"),
                Some(e.cookie.clone()),
            ),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{part}");
        assert!(!frag.contains("<html"));
    }
    assert_eq!(
        send(&e.app, get("/tickets/99999", Some(e.cookie.clone())))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        send(
            &e.app,
            get("/fragments/ticket/99999", Some(e.cookie.clone()))
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn repository_agent_settings_are_saved_validated_and_protected_by_csrf() {
    let Some(e) = env().await else { return };
    let id: i64 = sqlx::query_scalar("SELECT id FROM services WHERE external_id = 'org/repo'")
        .fetch_one(&e.pool)
        .await
        .unwrap();
    let path = format!("/services/{id}/profile");
    let body =
        |sys: &str, cmd: &str| format!("csrf={}&system={}&test_command={}", e.csrf, sys, cmd);
    assert_eq!(
        send(
            &e.app,
            form(
                &path,
                "csrf=bad&system=x&test_command=y".into(),
                Some(e.cookie.clone())
            )
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        send(
            &e.app,
            form(
                &path,
                body("be+careful", "pytest+-q"),
                Some(e.cookie.clone())
            )
        )
        .await
        .0,
        StatusCode::SEE_OTHER
    );
    let profile: serde_json::Value =
        sqlx::query_scalar("SELECT agent_profile FROM services WHERE id = $1")
            .bind(id)
            .fetch_one(&e.pool)
            .await
            .unwrap();
    assert_eq!(profile["system"], "be careful");
    assert_eq!(profile["test_command"], "pytest -q");
    // a command the engine would refuse is refused here too
    assert_eq!(
        send(
            &e.app,
            form(&path, body("x", "rm+(-rf)"), Some(e.cookie.clone()))
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        send(
            &e.app,
            form(&path, body("x", "a%0Ab"), Some(e.cookie.clone()))
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let (_, _, page) = send(&e.app, get("/services", Some(e.cookie.clone()))).await;
    assert!(
        page.contains("pytest -q") && page.contains("be careful") && page.contains("aria-pressed")
    );
}

#[tokio::test]
async fn model_accounts_can_be_added_ordered_switched_off_and_removed_and_tokens_stay_hidden() {
    let Some(e) = env().await else { return };
    let token = "sk-ant-oat01-0123456789abcdefghij-SECRETTAIL";
    let post = |path: &str, body: String| form(path, body, Some(e.cookie.clone()));
    // a form without the session's token is refused
    assert_eq!(
        send(
            &e.app,
            post(
                "/accounts",
                format!("csrf=x&label=backup&kind=oauth&token={token}")
            )
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    for label in ["main", "backup"] {
        let (st, _, _) = send(
            &e.app,
            post(
                "/accounts",
                format!("csrf={}&label={label}&kind=oauth&token={token}", e.csrf),
            ),
        )
        .await;
        assert_eq!(st, StatusCode::SEE_OTHER, "{label}");
    }
    let (_, _, page) = send(&e.app, get("/accounts", Some(e.cookie.clone()))).await;
    assert!(page.contains("main") && page.contains("backup"), "{page}");
    assert!(page.contains("Model accounts"), "{page}");
    assert!(
        page.contains("TAIL"),
        "the last characters identify an account"
    );
    assert!(
        !page.contains("SECRETTAIL") && !page.contains("0123456789abcdefghij"),
        "the token itself is never shown: {page}"
    );
    // a mistake says what was wrong
    let (st, _, body) = send(
        &e.app,
        post(
            "/accounts",
            format!("csrf={}&label=main&kind=oauth&token={token}", e.csrf),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(body.contains("already exists"), "{body}");
    let act = |label: &str, action: &str| {
        post(
            "/accounts/action",
            format!("csrf={}&label={label}&action={action}", e.csrf),
        )
    };
    assert_eq!(
        send(&e.app, act("backup", "up")).await.0,
        StatusCode::SEE_OTHER
    );
    let (_, _, page) = send(&e.app, get("/accounts", Some(e.cookie.clone()))).await;
    assert!(
        page.find("backup").unwrap() < page.find(">main<").unwrap_or(usize::MAX),
        "backup is first after Up: {page}"
    );
    assert_eq!(
        send(&e.app, act("main", "disable")).await.0,
        StatusCode::SEE_OTHER
    );
    let (_, _, page) = send(&e.app, get("/accounts", Some(e.cookie.clone()))).await;
    assert!(page.contains("disabled"), "{page}");
    assert_eq!(
        send(&e.app, act("main", "frobnicate")).await.0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        send(&e.app, act("backup", "remove")).await.0,
        StatusCode::SEE_OTHER
    );
    let (_, _, page) = send(&e.app, get("/accounts", Some(e.cookie.clone()))).await;
    assert!(!page.contains("backup</td>"), "{page}");
    // the audit log records who changed what, never the token
    let log: Vec<String> = sqlx::query_scalar(
        "SELECT action || ' ' || detail::text FROM audit_events WHERE action LIKE 'accounts.%'",
    )
    .fetch_all(&e.pool)
    .await
    .unwrap();
    assert!(
        log.iter().any(|l| l.starts_with("accounts.added")),
        "{log:?}"
    );
    assert!(log.iter().all(|l| !l.contains("SECRETTAIL")), "{log:?}");
}

#[tokio::test]
async fn the_model_accounts_page_needs_a_session() {
    let Some(e) = env().await else { return };
    let (st, _, _) = send(&e.app, get("/accounts", None)).await;
    assert!(
        st == StatusCode::SEE_OTHER
            || st == StatusCode::UNAUTHORIZED
            || st == StatusCode::FORBIDDEN,
        "{st}"
    );
}

#[tokio::test]
async fn a_person_chooses_whether_the_history_credits_them_and_only_a_real_login_is_accepted() {
    let Some(e) = env().await else { return };
    let admin: i64 =
        sqlx::query_scalar("SELECT id FROM principals WHERE channel = 'console' AND ref = 'admin'")
            .fetch_one(&e.pool)
            .await
            .unwrap();
    let path = format!("/people/{admin}/credit");
    let post = |body: String| form(&path, body, Some(e.cookie.clone()));
    assert_eq!(
        send(&e.app, post("csrf=x&github_login=rino&credit=on".into()))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    // not a login: spaces, a line break that would add a trailer, too long
    for bad in ["a%20b", "rino%0AApproved-by:%20x", "-rino", &"a".repeat(40)] {
        let (st, _, _) = send(
            &e.app,
            post(format!("csrf={}&github_login={bad}&credit=on", e.csrf)),
        )
        .await;
        assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY, "{bad}");
    }
    // being credited needs a login (the console admin has none yet)
    assert_eq!(
        send(
            &e.app,
            post(format!("csrf={}&github_login=&credit=on", e.csrf))
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        send(
            &e.app,
            post(format!("csrf={}&github_login=rino&credit=on", e.csrf))
        )
        .await
        .0,
        StatusCode::SEE_OTHER
    );
    let (login, credit): (Option<String>, bool) =
        sqlx::query_as("SELECT github_login, credit_coauthor FROM principals WHERE id = $1")
            .bind(admin)
            .fetch_one(&e.pool)
            .await
            .unwrap();
    assert_eq!((login.as_deref(), credit), (Some("rino"), true));
    let (_, _, page) = send(&e.app, get("/people", Some(e.cookie.clone()))).await;
    assert!(
        page.contains("value=\"rino\"") && page.contains("checked"),
        "{page}"
    );
    // a different login is a different person: the remembered id must not carry over
    sqlx::query("UPDATE principals SET github_id = 777 WHERE id = $1")
        .bind(admin)
        .execute(&e.pool)
        .await
        .unwrap();
    send(
        &e.app,
        post(format!("csrf={}&github_login=rino&credit=on", e.csrf)),
    )
    .await;
    let kept: Option<i64> = sqlx::query_scalar("SELECT github_id FROM principals WHERE id = $1")
        .bind(admin)
        .fetch_one(&e.pool)
        .await
        .unwrap();
    assert_eq!(kept, Some(777), "same login keeps its id");
    send(
        &e.app,
        post(format!(
            "csrf={}&github_login=someone-else&credit=on",
            e.csrf
        )),
    )
    .await;
    let reset: Option<i64> = sqlx::query_scalar("SELECT github_id FROM principals WHERE id = $1")
        .bind(admin)
        .fetch_one(&e.pool)
        .await
        .unwrap();
    assert_eq!(reset, None, "another login drops the old id");
    // turning it off is just leaving the box empty
    send(
        &e.app,
        post(format!("csrf={}&github_login=someone-else", e.csrf)),
    )
    .await;
    let credit: bool = sqlx::query_scalar("SELECT credit_coauthor FROM principals WHERE id = $1")
        .bind(admin)
        .fetch_one(&e.pool)
        .await
        .unwrap();
    assert!(!credit);
}

#[tokio::test]
async fn people_can_be_added_changed_and_removed_but_not_the_built_in_admin() {
    let Some(e) = env().await else { return };
    let add = |channel: &str, who: &str, role: &str| {
        format!(
            "csrf={}&channel={channel}&reference={who}&role={role}",
            e.csrf
        )
    };
    assert_eq!(
        send(
            &e.app,
            form(
                "/people",
                "csrf=x&channel=telegram&reference=1&role=owner".into(),
                Some(e.cookie.clone())
            )
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        send(
            &e.app,
            form(
                "/people",
                add("telegram", "555", "team_approver"),
                Some(e.cookie.clone())
            )
        )
        .await
        .0,
        StatusCode::SEE_OTHER
    );
    assert_eq!(
        send(
            &e.app,
            form(
                "/people",
                add("telegram", "555", "owner"),
                Some(e.cookie.clone())
            )
        )
        .await
        .0,
        StatusCode::SEE_OTHER,
        "adding again changes the role"
    );
    let role: String = sqlx::query_scalar(
        "SELECT role FROM principals WHERE channel = 'telegram' AND ref = '555'",
    )
    .fetch_one(&e.pool)
    .await
    .unwrap();
    assert_eq!(role, "owner");
    for bad in [
        add("sms", "1", "owner"),
        add("telegram", "1", "god"),
        add("telegram", "", "owner"),
    ] {
        assert_eq!(
            send(&e.app, form("/people", bad, Some(e.cookie.clone())))
                .await
                .0,
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }
    let (_, _, page) = send(&e.app, get("/people", Some(e.cookie.clone()))).await;
    assert!(page.contains("555") && page.contains("signed in with the admin token"));
    let admin_id: i64 = sqlx::query_scalar("SELECT id FROM principals WHERE channel = 'console'")
        .fetch_one(&e.pool)
        .await
        .unwrap();
    let person_id: i64 = sqlx::query_scalar("SELECT id FROM principals WHERE ref = '555'")
        .fetch_one(&e.pool)
        .await
        .unwrap();
    send(
        &e.app,
        form(
            &format!("/people/{admin_id}/delete"),
            format!("csrf={}", e.csrf),
            Some(e.cookie.clone()),
        ),
    )
    .await;
    send(
        &e.app,
        form(
            &format!("/people/{person_id}/delete"),
            format!("csrf={}", e.csrf),
            Some(e.cookie.clone()),
        ),
    )
    .await;
    let left: Vec<String> = sqlx::query_scalar("SELECT ref FROM principals")
        .fetch_all(&e.pool)
        .await
        .unwrap();
    assert_eq!(
        left,
        vec!["admin".to_string()],
        "the admin stays, the person is gone"
    );
}

#[tokio::test]
async fn health_setup_and_activity_pages_render_and_the_checklist_reflects_reality() {
    let Some(e) = env().await else { return };
    let (st, _, health) = send(&e.app, get("/health", Some(e.cookie.clone()))).await;
    assert_eq!(st, StatusCode::OK);
    assert!(
        health.contains("Setup checklist")
            && health.contains("Connections")
            && health.contains("Work queue")
    );
    assert!(
        health.contains("running"),
        "the fake plug-ins answer describe"
    );
    seed_ticket(&e, "1", "x", "new", 1).await;
    // the source lists the repository, as a real one would (a repository it does not list is archived)
    std::fs::write(
        e.dir.path().join("state/services.json"),
        r#"[{"external_id":"org/repo","default_branch":"main","archived":false,"private":true}]"#,
    )
    .unwrap();
    sqlx::query("INSERT INTO audit_events (actor, action, detail) VALUES ('console:admin','service.mode','{\"mode\":\"full\"}')").execute(&e.pool).await.unwrap();
    let (_, _, activity) = send(&e.app, get("/activity?kind=admin", Some(e.cookie.clone()))).await;
    assert!(activity.contains("Repository mode set to full") && activity.contains("you"));
    let (_, _, none) = send(&e.app, get("/activity?kind=chat", Some(e.cookie.clone()))).await;
    assert!(none.contains("Nothing yet"));
    // connecting GitHub and the agent through the console completes the required steps (the optional
    // one does not count), so the card goes away
    let (_, _, home) = send(&e.app, get("/", Some(e.cookie.clone()))).await;
    assert!(
        home.contains("Connect GitHub") || home.contains("Getting started"),
        "still guiding while nothing is connected"
    );
    connect_github(&e).await;
    send(
        &e.app,
        get(
            "/connect/github/installed?installation_id=180094190",
            Some(e.cookie.clone()),
        ),
    )
    .await;
    send(
        &e.app,
        form(
            "/connect/agent",
            format!(
                "csrf={}&key=sk-ant-x-key-value&sandbox=docker&image=img&network=bridge",
                e.csrf
            ),
            Some(e.cookie.clone()),
        ),
    )
    .await;
    let (_, _, home) = send(&e.app, get("/", Some(e.cookie.clone()))).await;
    assert!(!home.contains("Getting started"), "{home}");
}

#[tokio::test]
async fn static_assets_and_the_app_manifest_are_served_without_signing_in() {
    let Some(e) = env().await else { return };
    for (path, ctype) in [
        ("/static/app.css", "text/css"),
        ("/static/app.js", "application/javascript"),
        ("/static/icon.svg", "image/svg+xml"),
        ("/manifest.webmanifest", "application/manifest+json"),
    ] {
        let (st, h, body) = send(&e.app, get(path, None)).await;
        assert_eq!(st, StatusCode::OK, "{path}");
        assert!(
            h[header::CONTENT_TYPE].to_str().unwrap().starts_with(ctype),
            "{path}"
        );
        assert!(!body.is_empty());
    }
    let (_, _, login) = send(&e.app, get("/login", None)).await;
    assert!(
        login.contains("/static/app.css")
            && login.contains("rel=\"manifest\"")
            && login.contains("autocomplete=\"current-password\"")
    );
    let (_, _, page) = send(&e.app, get("/", Some(e.cookie.clone()))).await;
    assert!(page.contains("<meta name=\"viewport\"") && page.contains("aria-label=\"Main\""));
}

// ---- the live map ----

#[tokio::test]
async fn the_map_page_and_its_script_are_served_and_need_a_session() {
    let Some(e) = env().await else { return };
    assert_eq!(
        send(&e.app, get("/map", None)).await.0,
        StatusCode::SEE_OTHER
    );
    let (st, _, page) = send(&e.app, get("/map", Some(e.cookie.clone()))).await;
    assert_eq!(st, StatusCode::OK);
    assert!(
        page.contains("<canvas id=\"map\"")
            && page.contains("data-map-mode=\"replay\"")
            && page.contains("/static/map.js")
            && page.contains("Working now")
    );
    let (st, h, js) = send(&e.app, get("/static/map.js", None)).await;
    assert_eq!(st, StatusCode::OK);
    assert!(
        h[header::CONTENT_TYPE]
            .to_str()
            .unwrap()
            .starts_with("application/javascript")
            && js.contains("textContent")
    );
}

#[tokio::test]
async fn the_map_snapshot_lists_repositories_tickets_and_the_agents_that_are_busy() {
    let Some(e) = env().await else { return };
    let t = seed_ticket(&e, "1", "Fix <b>footer</b>", "developing", 1).await;
    seed_ticket(&e, "2", "Old merged", "merged", 1).await;
    sqlx::query("INSERT INTO jobs (ticket_id, kind, status, lease_owner, lease_until, fence, attempts) VALUES ($1, 'develop', 'running', 'w', now() + interval '1 minute', 1, 1)").bind(t).execute(&e.pool).await.unwrap();
    sqlx::query("INSERT INTO jobs (ticket_id, kind, status) VALUES ($1, 'review', 'queued')")
        .bind(t)
        .execute(&e.pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO audit_events (actor, action, ticket_id, detail) VALUES ('orchestrator','ticket.developing',$1,'{}')").bind(t).execute(&e.pool).await.unwrap();
    assert_eq!(
        send(&e.app, get("/map/data", None)).await.0,
        StatusCode::SEE_OTHER
    );
    let (st, h, body) = send(&e.app, get("/map/data", Some(e.cookie.clone()))).await;
    assert_eq!(st, StatusCode::OK);
    assert!(h[header::CONTENT_TYPE]
        .to_str()
        .unwrap()
        .starts_with("application/json"));
    let v: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["paused"], false);
    assert!(v["repos"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r["name"] == "org/repo"));
    assert_eq!(v["tickets"].as_array().unwrap().len(), 2);
    let busy = v["busy"].as_array().unwrap();
    assert_eq!(busy.len(), 1, "only running jobs count, not queued ones");
    assert_eq!(
        (busy[0]["agent"].as_str(), busy[0]["kind"].as_str()),
        (Some("develop"), Some("develop"))
    );
    assert_eq!(v["recent"].as_array().unwrap().len(), 1);
    assert!(
        v["tickets"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x["title"].as_str().unwrap().contains("<b>")),
        "JSON carries raw text; the page writes it with textContent"
    );
}

#[tokio::test]
async fn the_replay_returns_ordered_events_with_only_the_few_fields_it_needs_and_never_free_text() {
    let Some(e) = env().await else { return };
    let t = seed_ticket(&e, "1", "A ticket", "failed", 1).await;
    sqlx::query("INSERT INTO audit_events (at, actor, action, ticket_id, detail) VALUES (now() - interval '3 hours','sync','ticket.created',$1,'{}'), (now() - interval '2 hours','orchestrator','ticket.triaged',$1,'{\"to\":\"queued\",\"level\":1,\"reasons\":[\"secret reason\"],\"questions\":[\"private question\"]}'), (now() - interval '10 minutes','orchestrator','ticket.failed',$1,'{\"reason\":\"token ghs_abcdef leaked in a message\"}'), (now() - interval '30 hours','sync','ticket.created',$1,'{}')")
        .bind(t).execute(&e.pool).await.unwrap();
    assert_eq!(
        send(&e.app, get("/map/replay?hours=6", None)).await.0,
        StatusCode::SEE_OTHER
    );
    let (st, _, body) = send(&e.app, get("/map/replay?hours=6", Some(e.cookie.clone()))).await;
    assert_eq!(st, StatusCode::OK);
    for secret in ["secret reason", "private question", "ghs_abcdef", "leaked"] {
        assert!(
            !body.contains(secret),
            "{secret} must not be in the replay data"
        );
    }
    let v: Value = serde_json::from_str(&body).unwrap();
    let ev = v["events"].as_array().unwrap();
    assert_eq!(
        ev.len(),
        3,
        "the 30-hour-old event is outside a 6-hour range"
    );
    let times: Vec<i64> = ev.iter().map(|x| x["t"].as_i64().unwrap()).collect();
    assert!(times.windows(2).all(|w| w[0] <= w[1]), "time order");
    assert_eq!(ev[1]["d"]["to"], "queued");
    assert_eq!(v["tickets"].as_array().unwrap().len(), 1);
    assert!(v["now"].as_i64().unwrap() > 0);
    // a huge range is clamped, a missing one defaults
    assert_eq!(
        send(
            &e.app,
            get("/map/replay?hours=999999", Some(e.cookie.clone()))
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        send(&e.app, get("/map/replay", Some(e.cookie.clone())))
            .await
            .0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn the_map_shows_paused_when_work_is_paused() {
    let Some(e) = env().await else { return };
    e.orch.set_paused(true).await.unwrap();
    let (_, _, body) = send(&e.app, get("/map/data", Some(e.cookie.clone()))).await;
    let v: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["paused"], true);
}

// ---- connections ----

fn state_in(html: &str) -> String {
    let i = html.find("state=").expect("a state in the page") + 6;
    html[i..i + 64].to_string()
}

async fn start_github(e: &Env, extra: &str) -> (StatusCode, String) {
    let body = format!("csrf={}&name=tasklane-test&owner_type=user{extra}", e.csrf);
    let (st, _, html) = send(
        &e.app,
        form("/connect/github/start", body, Some(e.cookie.clone())),
    )
    .await;
    (st, html)
}

#[tokio::test]
async fn the_connections_page_says_what_is_missing_and_the_wizard_links_to_it() {
    let Some(e) = env().await else { return };
    let (st, _, page) = send(&e.app, get("/connections", Some(e.cookie.clone()))).await;
    assert_eq!(st, StatusCode::OK);
    assert!(
        page.contains("Connect GitHub")
            && page.contains("Connect the agent")
            && page.contains("not connected")
    );
    assert_eq!(
        send(&e.app, get("/connections", None)).await.0,
        StatusCode::SEE_OTHER
    );
    let (_, _, home) = send(&e.app, get("/", Some(e.cookie.clone()))).await;
    assert!(
        home.contains("/connections"),
        "the getting-started card links to it"
    );
}

#[tokio::test]
async fn starting_the_github_app_hands_the_browser_to_github_with_a_manifest_and_a_one_time_state()
{
    let Some(e) = env().await else { return };
    let (st, html) = start_github(&e, "").await;
    assert_eq!(st, StatusCode::OK);
    assert!(
        html.contains("action=\"https://github.com/settings/apps/new?state="),
        "{html}"
    );
    assert!(html.contains("data-autosubmit") && html.contains("name=\"manifest\""));
    let state = state_in(&html);
    let stored: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM setup_states WHERE state = $1 AND purpose = 'github_manifest'",
    )
    .bind(&state)
    .fetch_one(&e.pool)
    .await
    .unwrap();
    assert_eq!(stored, 1);
    // the manifest in the page is JSON with the permissions and the browser's own address
    let m = html
        .split("name=\"manifest\" value=\"")
        .nth(1)
        .unwrap()
        .split('"')
        .next()
        .unwrap()
        .replace("&quot;", "\"")
        .replace("&amp;", "&");
    let m: Value = serde_json::from_str(&m).unwrap();
    assert_eq!(m["default_permissions"]["issues"], "write");
    assert!(m["redirect_url"]
        .as_str()
        .unwrap()
        .ends_with("/connect/github/callback"));
    assert_eq!(m["name"], "tasklane-test");
    // an organization goes to the organization's page
    let (_, html) = start_github(&e, "&org=my-company").await;
    assert!(
        html.contains("/organizations/my-company/settings/apps/new"),
        "a name typed in the organization box means an organization"
    );
    let (_, html) = start_github(&e, "&org=https%3A%2F%2Fgithub.com%2FAquila-Team-Group").await;
    assert!(
        html.contains("/organizations/Aquila-Team-Group/settings/apps/new"),
        "a pasted link works too"
    );
    let body = format!(
        "csrf={}&name=tasklane-test&owner_type=org&org=my-company",
        e.csrf
    );
    let (_, _, html) = send(
        &e.app,
        form("/connect/github/start", body, Some(e.cookie.clone())),
    )
    .await;
    assert!(html.contains("https://github.com/organizations/my-company/settings/apps/new?state="));
}

#[tokio::test]
async fn starting_needs_a_valid_form_a_session_and_a_sane_name() {
    let Some(e) = env().await else { return };
    assert_eq!(
        send(
            &e.app,
            form(
                "/connect/github/start",
                "csrf=bad&name=abc&owner_type=user".into(),
                Some(e.cookie.clone())
            )
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        send(
            &e.app,
            form(
                "/connect/github/start",
                format!("csrf={}&name=ab&owner_type=user", e.csrf),
                Some(e.cookie.clone())
            )
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        send(
            &e.app,
            form(
                "/connect/github/start",
                format!("csrf={}&name=abc&owner_type=org&org=", e.csrf),
                Some(e.cookie.clone())
            )
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        send(
            &e.app,
            form(
                "/connect/github/start",
                format!("csrf={}&name=abc&owner_type=org&org=a%20b", e.csrf),
                Some(e.cookie.clone())
            )
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY,
        "no spaces in an organization name"
    );
}

#[tokio::test]
async fn the_callback_stores_the_credentials_privately_once_and_only_for_the_session_that_started_it(
) {
    let Some(e) = env().await else { return };
    let (_, html) = start_github(&e, "").await;
    let state = state_in(&html);
    // another session cannot use this state
    let (_, h2, _) = send(&e.app, form("/login", format!("token={ADMIN}"), None)).await;
    let other = h2[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    assert_eq!(
        send(
            &e.app,
            get(
                &format!("/connect/github/callback?code=abcdef0123456789&state={state}"),
                Some(other)
            )
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    // no state, a made-up state, no session
    assert_eq!(
        send(
            &e.app,
            get(
                "/connect/github/callback?code=abcdef0123456789",
                Some(e.cookie.clone())
            )
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        send(
            &e.app,
            get(
                &format!(
                    "/connect/github/callback?code=abcdef0123456789&state={}",
                    "0".repeat(64)
                ),
                Some(e.cookie.clone())
            )
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        send(
            &e.app,
            get(
                &format!("/connect/github/callback?code=abcdef0123456789&state={state}"),
                None
            )
        )
        .await
        .0,
        StatusCode::SEE_OTHER
    );
    // the right session works
    let (st, h, _) = send(
        &e.app,
        get(
            &format!("/connect/github/callback?code=abcdef0123456789&state={state}"),
            Some(e.cookie.clone()),
        ),
    )
    .await;
    assert_eq!(
        (st, h[header::LOCATION].to_str().unwrap()),
        (StatusCode::SEE_OTHER, "/connect/github/install")
    );
    let key = e.dir.path().join("secrets/github_app.pem");
    assert!(std::fs::read_to_string(&key)
        .unwrap()
        .contains("FAKEPEMDATA"));
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        std::fs::metadata(&key).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        std::fs::read_to_string(e.dir.path().join("secrets/github_webhook_secret"))
            .unwrap()
            .trim(),
        "fake-webhook-secret-value"
    );
    let saved: Value = sqlx::query_scalar("SELECT value FROM settings WHERE key = 'github'")
        .fetch_one(&e.pool)
        .await
        .unwrap();
    assert_eq!(
        (
            saved["app_id"].as_str(),
            saved["slug"].as_str(),
            saved["owner"].as_str()
        ),
        (Some("4242"), Some("tasklane-test"), Some("someone"))
    );
    assert!(
        !saved.to_string().contains("FAKEPEM") && !saved.to_string().contains("fake-webhook"),
        "no secret in the database"
    );
    // one use only
    assert_eq!(
        send(
            &e.app,
            get(
                &format!("/connect/github/callback?code=abcdef0123456789&state={state}"),
                Some(e.cookie.clone())
            )
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    // the install page now offers the install link for this App
    let (_, _, page) = send(
        &e.app,
        get("/connect/github/install", Some(e.cookie.clone())),
    )
    .await;
    assert!(page.contains("https://github.com/settings/apps/tasklane-test/installations"));
    // no page ever shows the credentials
    for path in ["/connections", "/connect/github/install", "/health", "/"] {
        let (_, _, body) = send(&e.app, get(path, Some(e.cookie.clone()))).await;
        assert!(
            !body.contains("FAKEPEMDATA") && !body.contains("fake-webhook-secret-value"),
            "{path}"
        );
    }
}

#[tokio::test]
async fn a_refused_code_saves_nothing_and_says_so() {
    let Some(e) = env().await else { return };
    let (_, html) = start_github(&e, "").await;
    let state = state_in(&html);
    let (st, _, page) = send(
        &e.app,
        get(
            &format!("/connect/github/callback?code=badcodebadcode&state={state}"),
            Some(e.cookie.clone()),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_GATEWAY);
    assert!(page.contains("did not hand over"));
    assert!(!e.dir.path().join("secrets/github_app.pem").exists());
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM settings WHERE key = 'github'")
        .fetch_one(&e.pool)
        .await
        .unwrap();
    assert_eq!(n, 0);
}

async fn installation_id(e: &Env) -> Option<String> {
    sqlx::query_scalar::<_, Option<String>>(
        "SELECT value->>'installation_id' FROM settings WHERE key = 'github'",
    )
    .fetch_one(&e.pool)
    .await
    .unwrap()
}

async fn connect_github(e: &Env) {
    let (_, html) = start_github(e, "").await;
    let state = state_in(&html);
    let (st, _, _) = send(
        &e.app,
        get(
            &format!("/connect/github/callback?code=abcdef0123456789&state={state}"),
            Some(e.cookie.clone()),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::SEE_OTHER);
}

#[tokio::test]
async fn after_the_install_github_sends_the_installation_id_and_it_is_kept_if_valid() {
    let Some(e) = env().await else { return };
    connect_github(&e).await;
    for bad in ["abc", "1;drop", "", "123456789012345678901234"] {
        send(
            &e.app,
            get(
                &format!("/connect/github/installed?installation_id={bad}&setup_action=install"),
                Some(e.cookie.clone()),
            ),
        )
        .await;
        assert_eq!(
            installation_id(&e).await,
            None,
            "{bad:?} is not an installation id"
        );
    }
    let (st, h, _) = send(
        &e.app,
        get(
            "/connect/github/installed?installation_id=180094190&setup_action=install",
            Some(e.cookie.clone()),
        ),
    )
    .await;
    assert_eq!(
        (st, h[header::LOCATION].to_str().unwrap()),
        (StatusCode::SEE_OTHER, "/connections?msg=github_connected")
    );
    assert_eq!(installation_id(&e).await.as_deref(), Some("180094190"));
    let (_, _, page) = send(
        &e.app,
        get("/connections?msg=github_connected", Some(e.cookie.clone())),
    )
    .await;
    assert!(
        page.contains("GitHub is connected.")
            && page.contains("tasklane-test")
            && page.contains("Disconnect")
    );
}

#[tokio::test]
async fn if_github_does_not_send_the_user_back_the_installation_is_found_by_itself() {
    let Some(e) = env().await else { return };
    connect_github(&e).await;
    std::fs::write(e.dir.path().join("state/installations.json"), r#"[{"id":"77","account":"someone","repository_selection":"selected"},{"id":"88","account":"other","repository_selection":"all"}]"#).unwrap();
    let (_, _, page) = send(&e.app, get("/connections", Some(e.cookie.clone()))).await;
    assert!(page.contains("connected"));
    let id: Option<String> =
        sqlx::query_scalar("SELECT value->>'installation_id' FROM settings WHERE key = 'github'")
            .fetch_one(&e.pool)
            .await
            .unwrap();
    assert_eq!(
        id.as_deref(),
        Some("77"),
        "the installation on the App owner's account is chosen"
    );
}

#[tokio::test]
async fn disconnecting_github_removes_the_secrets_and_the_setting() {
    let Some(e) = env().await else { return };
    connect_github(&e).await;
    sqlx::query("INSERT INTO services (kind, source, external_id) VALUES ('repo', 'github', 'org/lingering')")
        .execute(&e.pool)
        .await
        .unwrap();
    assert_eq!(
        send(
            &e.app,
            form(
                "/connect/github/disconnect",
                "csrf=bad".into(),
                Some(e.cookie.clone())
            )
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert!(e.dir.path().join("secrets/github_app.pem").exists());
    assert_eq!(
        send(
            &e.app,
            form(
                "/connect/github/disconnect",
                format!("csrf={}", e.csrf),
                Some(e.cookie.clone())
            )
        )
        .await
        .0,
        StatusCode::SEE_OTHER
    );
    assert!(
        !e.dir.path().join("secrets/github_app.pem").exists()
            && !e.dir.path().join("secrets/github_webhook_secret").exists()
    );
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM settings WHERE key = 'github'")
        .fetch_one(&e.pool)
        .await
        .unwrap();
    assert_eq!(n, 0);
    let live: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM services WHERE source = 'github' AND NOT archived",
    )
    .fetch_one(&e.pool)
    .await
    .unwrap();
    assert_eq!(live, 0, "repositories must not linger after disconnect");
}

#[tokio::test]
async fn the_agent_key_is_saved_privately_validated_and_never_shown_again() {
    let Some(e) = env().await else { return };
    let save = |body: String| {
        let (app, cookie) = (e.app.clone(), e.cookie.clone());
        async move { send(&app, form("/connect/agent", body, Some(cookie))).await }
    };
    assert_eq!(
        save("csrf=bad&key=x&sandbox=none".into()).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        save(format!("csrf={}&key=has+a+space&sandbox=none", e.csrf))
            .await
            .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        save(format!("csrf={}&key=ok&sandbox=sudo", e.csrf)).await.0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        save(format!(
            "csrf={}&key=ok&sandbox=docker&image=--privileged&network=net",
            e.csrf
        ))
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY,
        "an image name cannot carry options"
    );
    assert!(
        !e.dir.path().join("secrets/anthropic_key").exists(),
        "nothing was saved by the refused requests"
    );

    let (st, h, _) = save(format!(
        "csrf={}&key=sk-ant-secret-key-value&sandbox=docker&image=&network=",
        e.csrf
    ))
    .await;
    assert_eq!(
        (st, h[header::LOCATION].to_str().unwrap()),
        (StatusCode::SEE_OTHER, "/connect/agent?msg=agent_saved")
    );
    use std::os::unix::fs::PermissionsExt;
    let kf = e.dir.path().join("secrets/anthropic_key");
    assert_eq!(
        std::fs::read_to_string(&kf).unwrap().trim(),
        "sk-ant-secret-key-value"
    );
    assert_eq!(
        std::fs::metadata(&kf).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let s: Value = sqlx::query_scalar("SELECT value FROM settings WHERE key = 'agent'")
        .fetch_one(&e.pool)
        .await
        .unwrap();
    assert_eq!(
        (
            s["sandbox"].as_str(),
            s["image"].as_str(),
            s["network"].as_str()
        ),
        (Some("docker"), Some("tasklane-sandbox"), Some("bridge"))
    );
    assert!(
        !s.to_string().contains("sk-ant"),
        "the key is not in the database"
    );
    // saving again with an empty key keeps the saved one; the key is not in any page
    save(format!("csrf={}&key=&sandbox=none", e.csrf)).await;
    assert_eq!(
        std::fs::read_to_string(&kf).unwrap().trim(),
        "sk-ant-secret-key-value"
    );
    for path in ["/connect/agent", "/connections", "/health", "/"] {
        let (_, _, body) = send(&e.app, get(path, Some(e.cookie.clone()))).await;
        assert!(!body.contains("sk-ant-secret-key-value"), "{path}");
    }
    let (_, _, page) = send(&e.app, get("/connect/agent", Some(e.cookie.clone()))).await;
    assert!(page.contains("Leave empty to keep the saved one") && page.contains("saved"));
    // removing it
    assert_eq!(
        send(
            &e.app,
            form(
                "/connect/agent/disconnect",
                format!("csrf={}", e.csrf),
                Some(e.cookie.clone())
            )
        )
        .await
        .0,
        StatusCode::SEE_OTHER
    );
    assert!(!kf.exists());
}

#[tokio::test]
async fn the_agent_test_button_reports_success_and_failure_in_plain_words() {
    let Some(e) = env().await else { return };
    assert_eq!(
        send(
            &e.app,
            form(
                "/connect/agent/test",
                "csrf=bad".into(),
                Some(e.cookie.clone())
            )
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let (st, _, page) = send(
        &e.app,
        form(
            "/connect/agent/test",
            format!("csrf={}", e.csrf),
            Some(e.cookie.clone()),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert!(page.contains("passed") && page.contains("It works"));
    std::fs::write(e.dir.path().join("state/check.json"), r#"{"ok": false, "kind": "unauthorized", "message": "The key was refused <by> the provider", "secs": 0.4}"#).unwrap();
    let (_, _, page) = send(
        &e.app,
        form(
            "/connect/agent/test",
            format!("csrf={}", e.csrf),
            Some(e.cookie.clone()),
        ),
    )
    .await;
    assert!(
        page.contains("failed") && page.contains("The key was refused &lt;by&gt; the provider"),
        "the message is escaped"
    );
}

#[tokio::test]
async fn the_repository_list_is_paged_searchable_filterable_and_returns_to_the_same_view() {
    let Some(e) = env().await else { return };
    for i in 0..30 {
        sqlx::query("INSERT INTO services (kind, source, external_id, mode, facts) VALUES ('repo','test',$1,$2,'{}')")
            .bind(format!("paged/repo-{i:02}"))
            .bind(if i % 3 == 0 { "full" } else { "triage" })
            .execute(&e.pool)
            .await
            .unwrap();
    }
    let page = |q: &str| get(&format!("/services{q}"), Some(e.cookie.clone()));
    let (_, _, first) = send(&e.app, page("?q=paged%2F")).await;
    assert!(first.contains("Showing 1\u{2013}12 of 30"), "{first}");
    assert!(first.contains("repo-00") && !first.contains("repo-12"));
    let (_, _, third) = send(&e.app, page("?q=paged%2F&page=3")).await;
    assert!(third.contains("Showing 25\u{2013}30 of 30"));
    let (_, _, beyond) = send(&e.app, page("?q=paged%2F&page=99")).await;
    assert!(
        beyond.contains("Showing 25\u{2013}30 of 30"),
        "an out-of-range page falls back to the last"
    );
    let (_, _, full) = send(&e.app, page("?q=paged%2F&mode=full")).await;
    assert!(full.contains("Showing 1\u{2013}10 of 10"));
    let (_, _, per) = send(&e.app, page("?q=paged%2F&per=24")).await;
    assert!(per.contains("Showing 1\u{2013}24 of 30"));
    // a LIKE wildcard in the search text is meant literally
    let (_, _, wild) = send(&e.app, page("?q=%25")).await;
    assert!(wild.contains("No repository matches"), "{wild}");
    // a mode change goes back to the view it came from, and only a plain query string is accepted
    let id: i64 = sqlx::query_scalar("SELECT id FROM services WHERE external_id = 'paged/repo-01'")
        .fetch_one(&e.pool)
        .await
        .unwrap();
    let post = |ret: &str| {
        form(
            &format!("/services/{id}/mode"),
            format!("csrf={}&mode=watch&ret={ret}", e.csrf),
            Some(e.cookie.clone()),
        )
    };
    let (st, h, _) = send(&e.app, post("q%3Dpaged%252F%26page%3D2")).await;
    assert_eq!(st, StatusCode::SEE_OTHER);
    assert_eq!(h[header::LOCATION], "/services?q=paged%2F&page=2");
    let (_, h, _) = send(&e.app, post("%2F%2Fevil.example")).await;
    assert_eq!(
        h[header::LOCATION],
        "/services",
        "a return value that is not a plain query string is ignored"
    );
}

#[tokio::test]
async fn a_repository_owned_by_another_install_says_so_and_a_person_can_take_it_over() {
    let Some(e) = env().await else { return };
    sqlx::query("UPDATE services SET owner_state = 'foreign', owner_name = 'office_bbbb2222', owner_checked_at = now() WHERE external_id = 'org/repo'")
        .execute(&e.pool)
        .await
        .unwrap();
    let (_, _, page) = send(&e.app, get("/services", Some(e.cookie.clone()))).await;
    assert!(
        page.contains("Handled by another tasklane install") && page.contains("office_bbbb2222"),
        "{page}"
    );
    assert!(page.contains("This install is called"));
    let id: i64 = sqlx::query_scalar("SELECT id FROM services WHERE external_id = 'org/repo'")
        .fetch_one(&e.pool)
        .await
        .unwrap();
    // no cookie, no take-over
    let (st, _, _) = send(
        &e.app,
        form(&format!("/services/{id}/takeover"), "csrf=x".into(), None),
    )
    .await;
    assert!(st == StatusCode::SEE_OTHER || st == StatusCode::FORBIDDEN);
    let (st, h, _) = send(
        &e.app,
        form(
            &format!("/services/{id}/takeover"),
            format!("csrf={}&ret=mode%3Dfull", e.csrf),
            Some(e.cookie.clone()),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::SEE_OTHER);
    assert_eq!(h[header::LOCATION], "/services?mode=full");
    let state: String = sqlx::query_scalar("SELECT owner_state FROM services WHERE id = $1")
        .bind(id)
        .fetch_one(&e.pool)
        .await
        .unwrap();
    assert_eq!(state, "mine");
}

#[tokio::test]
async fn the_ticket_list_is_paged_and_a_wildcard_in_the_search_is_literal() {
    let Some(e) = env().await else { return };
    let svc: i64 = sqlx::query_scalar("INSERT INTO services (kind, source, external_id, mode, facts) VALUES ('repo','test','paged/tickets','triage','{}') RETURNING id")
        .fetch_one(&e.pool)
        .await
        .unwrap();
    for i in 0..60 {
        sqlx::query("INSERT INTO tickets (service_id, external_ref, state, title, updated_at) VALUES ($1, $2, 'new', $3, now() - make_interval(secs => $4::int))")
            .bind(svc)
            .bind(format!("{i}"))
            .bind(format!("ticket number {i:02}"))
            .bind(i)
            .execute(&e.pool)
            .await
            .unwrap();
    }
    let page = |q: &str| get(&format!("/tickets{q}"), Some(e.cookie.clone()));
    let (_, _, first) = send(&e.app, page("")).await;
    assert!(first.contains("Showing 1\u{2013}25 of 60"), "{first}");
    assert!(first.contains("ticket number 00") && !first.contains("ticket number 25"));
    let (_, _, last) = send(&e.app, page("?page=3")).await;
    assert!(last.contains("Showing 51\u{2013}60 of 60"));
    let (_, _, beyond) = send(&e.app, page("?page=99")).await;
    assert!(beyond.contains("Showing 51\u{2013}60 of 60"));
    let (_, _, found) = send(&e.app, page("?q=number+4")).await;
    assert!(found.contains("Showing 1\u{2013}10 of 10"), "{found}");
    let (_, _, wild) = send(&e.app, page("?q=%25")).await;
    assert!(wild.contains("No tickets match"), "{wild}");
}

#[tokio::test]
async fn a_duplicate_proposal_is_answered_in_the_console_and_needs_csrf() {
    let Some(e) = env().await else { return };
    let t = seed_ticket(&e, "31", "Login page fails", "duplicate_proposed", 1).await;
    sqlx::query("UPDATE tickets SET duplicate_of = '7' WHERE id = $1")
        .bind(t)
        .execute(&e.pool)
        .await
        .unwrap();
    let (_, _, page) = send(
        &e.app,
        get(&format!("/tickets/{t}"), Some(e.cookie.clone())),
    )
    .await;
    assert!(
        page.contains("Is this a duplicate?") && page.contains("#7"),
        "{page}"
    );
    let path = format!("/tickets/{t}/duplicate");
    assert_eq!(
        send(
            &e.app,
            form(
                &path,
                "csrf=bad&action=confirm".into(),
                Some(e.cookie.clone())
            )
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        send(
            &e.app,
            form(
                &path,
                format!("csrf={}&action=confirm", e.csrf),
                Some(e.cookie.clone())
            )
        )
        .await
        .0,
        StatusCode::SEE_OTHER
    );
    let state: String = sqlx::query_scalar("SELECT state FROM tickets WHERE id = $1")
        .bind(t)
        .fetch_one(&e.pool)
        .await
        .unwrap();
    assert_eq!(state, "closed_duplicate");
}

#[tokio::test]
async fn notes_for_a_repository_are_added_listed_and_removed_with_csrf() {
    let Some(e) = env().await else { return };
    let svc: i64 = sqlx::query_scalar("SELECT id FROM services WHERE external_id = 'org/repo'")
        .fetch_one(&e.pool)
        .await
        .unwrap();
    let add = format!("/services/{svc}/notes");
    assert_eq!(
        send(
            &e.app,
            form(&add, "csrf=bad&fact=x".into(), Some(e.cookie.clone()))
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        send(
            &e.app,
            form(
                &add,
                format!("csrf={}&fact=Never+touch+%3Cgenerated%3E", e.csrf),
                Some(e.cookie.clone())
            )
        )
        .await
        .0,
        StatusCode::SEE_OTHER
    );
    let (_, _, page) = send(&e.app, get("/services", Some(e.cookie.clone()))).await;
    assert!(
        page.contains("Never touch &lt;generated&gt;"),
        "escaped and listed: {page}"
    );
    assert_eq!(
        send(
            &e.app,
            form(
                &add,
                format!("csrf={}&fact=", e.csrf),
                Some(e.cookie.clone())
            )
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let nid: i64 = sqlx::query_scalar("SELECT id FROM memory_notes WHERE service_id = $1")
        .bind(svc)
        .fetch_one(&e.pool)
        .await
        .unwrap();
    assert_eq!(
        send(
            &e.app,
            form(
                &format!("/services/{svc}/notes/{nid}/delete"),
                format!("csrf={}", e.csrf),
                Some(e.cookie.clone())
            )
        )
        .await
        .0,
        StatusCode::SEE_OTHER
    );
    let left: i64 = sqlx::query_scalar("SELECT count(*) FROM memory_notes")
        .fetch_one(&e.pool)
        .await
        .unwrap();
    assert_eq!(left, 0);
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

#[tokio::test]
async fn the_preview_settings_are_saved_together_or_not_at_all() {
    let Some(e) = env().await else { return };
    let svc: i64 = sqlx::query_scalar("SELECT id FROM services WHERE external_id = 'org/repo'")
        .fetch_one(&e.pool)
        .await
        .unwrap();
    let path = format!("/services/{svc}/profile");
    let post = |body: String| {
        let (app, cookie) = (e.app.clone(), e.cookie.clone());
        let path = path.clone();
        async move { send(&app, form(&path, body, Some(cookie))).await.0 }
    };
    let base = format!("csrf={}&system=&test_command=", e.csrf);
    // a command without a port, a port below 1024, and a non-number are refused
    for bad in [
        "&preview_command=python3+-m+http.server+3000&preview_port=",
        "&preview_command=&preview_port=3000",
        "&preview_command=x&preview_port=80",
        "&preview_command=x&preview_port=abc",
    ] {
        assert_eq!(
            post(format!("{base}{bad}")).await,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{bad}"
        );
    }
    assert_eq!(
        post(format!(
            "{base}&preview_command=python3+-m+http.server+3000&preview_port=3000"
        ))
        .await,
        StatusCode::SEE_OTHER
    );
    let p: serde_json::Value =
        sqlx::query_scalar("SELECT agent_profile FROM services WHERE id = $1")
            .bind(svc)
            .fetch_one(&e.pool)
            .await
            .unwrap();
    assert_eq!(p["preview_command"], "python3 -m http.server 3000");
    assert_eq!(p["preview_port"], 3000);
    let (_, _, page) = send(&e.app, get("/services", Some(e.cookie.clone()))).await;
    assert!(page.contains("python3 -m http.server 3000"), "{page}");
    // both empty turns it off
    assert_eq!(
        post(format!("{base}&preview_command=&preview_port=")).await,
        StatusCode::SEE_OTHER
    );
    let p: serde_json::Value =
        sqlx::query_scalar("SELECT agent_profile FROM services WHERE id = $1")
            .bind(svc)
            .fetch_one(&e.pool)
            .await
            .unwrap();
    assert_eq!(p["preview_command"], "");
    assert!(p["preview_port"].is_null());
}

#[tokio::test]
async fn metrics_need_their_own_bearer_token_and_show_the_queue_and_the_tickets() {
    let Some(e) = env().await else { return };
    seed_ticket(&e, "41", "Metrics", "merged", 1).await;
    let ask = |token: Option<&str>| {
        let mut r = Request::get("/metrics");
        if let Some(t) = token {
            r = r.header(header::AUTHORIZATION, format!("Bearer {t}"));
        }
        r.body(Body::empty()).unwrap()
    };
    assert_eq!(send(&e.app, ask(None)).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(
        send(&e.app, ask(Some("wrong"))).await.0,
        StatusCode::UNAUTHORIZED
    );
    // the admin token is not the metrics token
    assert_eq!(
        send(&e.app, ask(Some(ADMIN))).await.0,
        StatusCode::UNAUTHORIZED
    );
    let (st, _, body) = send(&e.app, ask(Some("metrics-token-for-tests-1234"))).await;
    assert_eq!(st, StatusCode::OK);
    assert!(body.contains("tasklane_up 1"), "{body}");
    assert!(
        body.contains("tasklane_tickets{state=\"merged\"} 1"),
        "{body}"
    );
    assert!(body.contains("tasklane_model_tokens_total"), "{body}");
}

#[tokio::test]
async fn metrics_tell_first_try_merges_from_ones_that_needed_another_round() {
    let Some(e) = env().await else { return };
    let clean = seed_ticket(&e, "61", "Clean", "merged", 1).await;
    let redone = seed_ticket(&e, "62", "Redone", "merged", 1).await;
    let events: [(i64, &str, &str); 8] = [
        (clean, "ticket.developing", "{}"),
        (clean, "ticket.reviewed", "{\"verdict\": \"approve\"}"),
        (clean, "ticket.merged", "{}"),
        (redone, "ticket.developing", "{}"),
        (redone, "ticket.ci_red", "{}"),
        (redone, "ticket.developing", "{}"),
        (redone, "ticket.reviewed", "{\"verdict\": \"changes\"}"),
        (redone, "ticket.merged", "{}"),
    ];
    for (t, action, detail) in events {
        sqlx::query("INSERT INTO audit_events (actor, action, ticket_id, detail) VALUES ('orchestrator', $1, $2, $3::jsonb)")
            .bind(action)
            .bind(t)
            .bind(detail)
            .execute(&e.pool)
            .await
            .unwrap();
    }
    let req = Request::get("/metrics")
        .header(header::AUTHORIZATION, "Bearer metrics-token-for-tests-1234")
        .body(Body::empty())
        .unwrap();
    let (_, _, body) = send(&e.app, req).await;
    for line in [
        "tasklane_tickets_merged 2",
        "tasklane_tickets_merged_first_try 1",
        "tasklane_merged_dev_attempts 3",
        "tasklane_ci_red 1",
        "tasklane_reviews{verdict=\"changes\"} 1",
        "tasklane_reviews{verdict=\"approve\"} 1",
    ] {
        assert!(body.contains(line), "missing {line}: {body}");
    }
}

#[tokio::test]
async fn the_repository_card_shows_the_model_tokens_of_the_last_thirty_days() {
    let Some(e) = env().await else { return };
    let t = seed_ticket(&e, "51", "Costly", "merged", 1).await;
    let job: i64 = sqlx::query_scalar(
        "INSERT INTO jobs (ticket_id, kind, status) VALUES ($1, 'triage', 'done') RETURNING id",
    )
    .bind(t)
    .fetch_one(&e.pool)
    .await
    .unwrap();
    for ago in ["1 day", "90 days"] {
        sqlx::query("INSERT INTO runs (job_id, actor, started_at, usage) VALUES ($1, 'triage', now() - $2::interval, '{\"tokens_in\": 1000, \"tokens_out\": 500}')")
            .bind(job)
            .bind(ago)
            .execute(&e.pool)
            .await
            .unwrap();
    }
    let (_, _, page) = send(&e.app, get("/services", Some(e.cookie.clone()))).await;
    assert!(
        page.contains("1.5k tokens (30 d)"),
        "only the recent run counts: {page}"
    );
}

#[tokio::test]
async fn repositories_the_source_no_longer_lists_are_hidden_until_asked_for() {
    let Some(e) = env().await else { return };
    sqlx::query("INSERT INTO services (kind, source, external_id, mode, archived) VALUES ('repo','test','old/gone','watch',true)")
        .execute(&e.pool)
        .await
        .unwrap();
    let (_, _, page) = send(&e.app, get("/services", Some(e.cookie.clone()))).await;
    assert!(page.contains("org/repo") && !page.contains("old/gone"));
    assert!(page.contains("Show archived"));
    let (_, _, all) = send(&e.app, get("/services?archived=1", Some(e.cookie.clone()))).await;
    assert!(all.contains("old/gone") && all.contains("Hide archived"));
}

#[tokio::test]
async fn issues_that_were_open_before_the_connection_can_be_imported_on_request() {
    let Some(e) = env().await else { return };
    std::fs::write(
        e.dir.path().join("state/open_issues_org_repo.json"),
        r#"["3","4","closed-5"]"#,
    )
    .unwrap();
    let id: i64 = sqlx::query_scalar("SELECT id FROM services WHERE external_id = 'org/repo'")
        .fetch_one(&e.pool)
        .await
        .unwrap();
    let (_, _, page) = send(&e.app, get("/services", Some(e.cookie.clone()))).await;
    assert!(page.contains(&format!("/services/{id}/import")));
    let url = format!("/services/{id}/import");
    let (st, _, _) = send(
        &e.app,
        form(&url, "csrf=nope".into(), Some(e.cookie.clone())),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, _, _) = send(
        &e.app,
        form(&url, format!("csrf={}", e.csrf), Some(e.cookie.clone())),
    )
    .await;
    assert!(st.is_redirection());
    wait_import(&e, id).await;
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM tickets WHERE service_id = $1")
        .bind(id)
        .fetch_one(&e.pool)
        .await
        .unwrap();
    assert_eq!(n, 2, "the closed issue stays out");
    let titles: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM tickets WHERE service_id = $1 AND title IN ('Fix 3', 'Fix 4')",
    )
    .bind(id)
    .fetch_one(&e.pool)
    .await
    .unwrap();
    assert_eq!(titles, 2, "tickets carry the issue title");
    let (_, _, page) = send(&e.app, get("/services", Some(e.cookie.clone()))).await;
    assert!(page.contains("Import finished: 2 new ticket(s)"), "{page}");
    send(
        &e.app,
        form(&url, format!("csrf={}", e.csrf), Some(e.cookie.clone())),
    )
    .await;
    wait_import(&e, id).await;
    let (_, _, again) = send(&e.app, get("/services", Some(e.cookie.clone()))).await;
    assert!(
        again.contains("Import finished: 0 new ticket(s)"),
        "a second run adds nothing"
    );
}

async fn wait_import(e: &Env, id: i64) {
    for _ in 0..100 {
        let v: Option<String> = sqlx::query_scalar("SELECT value FROM sync_state WHERE key = $1")
            .bind(format!("import_{id}"))
            .fetch_optional(&e.pool)
            .await
            .unwrap();
        if v.is_some_and(|v| v.contains("\"done\"") || v.contains("\"failed\"")) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("the import did not finish");
}

#[tokio::test]
async fn the_branch_pull_requests_go_to_is_set_per_repository_and_checked() {
    let Some(e) = env().await else { return };
    let id: i64 = sqlx::query_scalar("SELECT id FROM services WHERE external_id = 'org/repo'")
        .fetch_one(&e.pool)
        .await
        .unwrap();
    let url = format!("/services/{id}/profile");
    let body = |b: &str| {
        format!(
            "csrf={}&system=&test_command=&preview_command=&preview_port=&target_branch={b}",
            e.csrf
        )
    };
    let (st, _, _) = send(&e.app, form(&url, body("--evil"), Some(e.cookie.clone()))).await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY);
    std::fs::write(
        e.dir.path().join("state/missing_branches.json"),
        r#"["nowhere"]"#,
    )
    .unwrap();
    let (st, _, msg) = send(&e.app, form(&url, body("nowhere"), Some(e.cookie.clone()))).await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(msg.contains("does not exist"), "{msg}");
    let (st, _, _) = send(&e.app, form(&url, body("dev"), Some(e.cookie.clone()))).await;
    assert!(st.is_redirection());
    let t: String =
        sqlx::query_scalar("SELECT agent_profile->>'target_branch' FROM services WHERE id = $1")
            .bind(id)
            .fetch_one(&e.pool)
            .await
            .unwrap();
    assert_eq!(t, "dev");
    let (_, _, page) = send(&e.app, get("/services", Some(e.cookie.clone()))).await;
    assert!(page.contains("Branch pull requests go to") && page.contains("value=\"dev\""));
}

#[tokio::test]
async fn a_ticket_that_stopped_with_nothing_to_change_can_be_closed_as_done() {
    let Some(e) = env().await else { return };
    let (t,): (i64,) = sqlx::query_as("INSERT INTO tickets (service_id, external_ref, state, last_error) SELECT id, '31', 'failed', 'Nothing to change: the development agent made no edit. The agent said: it exists' FROM services WHERE external_id = 'org/repo' RETURNING id")
        .fetch_one(&e.pool)
        .await
        .unwrap();
    let (_, _, page) = send(
        &e.app,
        get(&format!("/tickets/{t}"), Some(e.cookie.clone())),
    )
    .await;
    assert!(page.contains("Nothing needed changing") && page.contains("close the issue"));
    let (st, _, _) = send(
        &e.app,
        form(
            &format!("/tickets/{t}/retry"),
            format!("csrf={}&action=done", e.csrf),
            Some(e.cookie.clone()),
        ),
    )
    .await;
    assert!(st.is_redirection());
    let state: String = sqlx::query_scalar("SELECT state FROM tickets WHERE id = $1")
        .bind(t)
        .fetch_one(&e.pool)
        .await
        .unwrap();
    assert_eq!(state, "closed_done");
    assert_eq!(
        std::fs::read_to_string(e.dir.path().join("state/closed.log"))
            .unwrap_or_default()
            .trim(),
        "31"
    );
    // an ordinary failure cannot be closed this way
    let (u,): (i64,) = sqlx::query_as("INSERT INTO tickets (service_id, external_ref, state, last_error) SELECT id, '32', 'failed', 'git exploded' FROM services WHERE external_id = 'org/repo' RETURNING id")
        .fetch_one(&e.pool)
        .await
        .unwrap();
    let (st, _, _) = send(
        &e.app,
        form(
            &format!("/tickets/{u}/retry"),
            format!("csrf={}&action=done", e.csrf),
            Some(e.cookie.clone()),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT);
}

#[tokio::test]
async fn the_model_is_pinned_per_repository_and_the_ticket_shows_what_ran() {
    let Some(e) = env().await else { return };
    let id: i64 = sqlx::query_scalar("SELECT id FROM services WHERE external_id = 'org/repo'")
        .fetch_one(&e.pool)
        .await
        .unwrap();
    let url = format!("/services/{id}/profile");
    let body = |m: &str| {
        format!(
            "csrf={}&system=&test_command=&preview_command=&preview_port=&model={m}",
            e.csrf
        )
    };
    let (st, _, _) = send(&e.app, form(&url, body("a%20b"), Some(e.cookie.clone()))).await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY);
    let (st, _, _) = send(
        &e.app,
        form(&url, body("claude-sonnet-5-5"), Some(e.cookie.clone())),
    )
    .await;
    assert!(st.is_redirection());
    let m: String =
        sqlx::query_scalar("SELECT agent_profile->>'model' FROM services WHERE id = $1")
            .bind(id)
            .fetch_one(&e.pool)
            .await
            .unwrap();
    assert_eq!(m, "claude-sonnet-5-5");
    let t = seed_ticket(&e, "41", "Model check", "merged", 1).await;
    let (before_job,): (i64,) = sqlx::query_as(
        "INSERT INTO jobs (ticket_id, kind, status) VALUES ($1, 'triage', 'done') RETURNING id",
    )
    .bind(t)
    .fetch_one(&e.pool)
    .await
    .unwrap();
    let (_, _, none) = send(
        &e.app,
        get(&format!("/tickets/{t}"), Some(e.cookie.clone())),
    )
    .await;
    assert!(none.contains("not reported yet"));
    sqlx::query("INSERT INTO runs (job_id, actor, usage) VALUES ($1, 'triage', '{\"model\":\"claude-sonnet-5-5\"}')")
        .bind(before_job)
        .execute(&e.pool)
        .await
        .unwrap();
    let (_, _, shown) = send(
        &e.app,
        get(&format!("/tickets/{t}"), Some(e.cookie.clone())),
    )
    .await;
    assert!(
        shown.contains("<dt>Model</dt><dd>claude-sonnet-5-5</dd>"),
        "{shown}"
    );
}

#[tokio::test]
async fn the_knowledge_page_holds_notes_for_every_repository_and_notes_can_be_edited() {
    let Some(e) = env().await else { return };
    let (st, _, _) = send(&e.app, get("/knowledge", None)).await;
    assert_ne!(st, StatusCode::OK, "needs a session");
    let post = |path: &str, body: String| form(path, body, Some(e.cookie.clone()));
    let (st, _, _) = send(
        &e.app,
        post("/knowledge/notes", "csrf=nope&fact=x".to_string()),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, _, _) = send(
        &e.app,
        post(
            "/knowledge/notes",
            format!("csrf={}&fact=Answer%20in%20Indonesian%20%3Cb%3E", e.csrf),
        ),
    )
    .await;
    assert!(st.is_redirection());
    let (_, _, page) = send(&e.app, get("/knowledge", Some(e.cookie.clone()))).await;
    assert!(page.contains("Answer in Indonesian &lt;b&gt;") && page.contains("org/repo"));
    let nid: i64 = sqlx::query_scalar("SELECT id FROM memory_notes WHERE service_id IS NULL")
        .fetch_one(&e.pool)
        .await
        .unwrap();
    send(
        &e.app,
        post(
            &format!("/knowledge/notes/{nid}/edit"),
            format!("csrf={}&fact=Answer%20in%20English", e.csrf),
        ),
    )
    .await;
    let fact: String = sqlx::query_scalar("SELECT fact FROM memory_notes WHERE id = $1")
        .bind(nid)
        .fetch_one(&e.pool)
        .await
        .unwrap();
    assert_eq!(fact, "Answer in English");
    // documents to read first are saved per repository and checked
    let id: i64 = sqlx::query_scalar("SELECT id FROM services WHERE external_id = 'org/repo'")
        .fetch_one(&e.pool)
        .await
        .unwrap();
    let prof = |docs: &str| {
        format!(
            "csrf={}&system=&test_command=&preview_command=&preview_port=&docs={docs}",
            e.csrf
        )
    };
    let (st, _, _) = send(
        &e.app,
        post(
            &format!("/services/{id}/profile"),
            prof("..%2Fetc%2Fpasswd"),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY);
    let (st, _, _) = send(
        &e.app,
        post(
            &format!("/services/{id}/profile"),
            prof("CONTRIBUTING.md%0Adocs%2Farchitecture.md"),
        ),
    )
    .await;
    assert!(st.is_redirection());
    let (_, _, page) = send(&e.app, get("/knowledge", Some(e.cookie.clone()))).await;
    assert!(page.contains("docs/architecture.md"));
    send(
        &e.app,
        post(
            &format!("/knowledge/notes/{nid}/delete"),
            format!("csrf={}", e.csrf),
        ),
    )
    .await;
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM memory_notes WHERE service_id IS NULL")
        .fetch_one(&e.pool)
        .await
        .unwrap();
    assert_eq!(n, 0);
}

#[tokio::test]
async fn a_new_project_can_be_planned_with_the_ai_before_anything_is_created() {
    let Some(e) = env().await else { return };
    let post = |path: &str, body: String| form(path, body, Some(e.cookie.clone()));
    let base = format!(
        "csrf={}&owner=acme&name=plan-app&template=blank&visibility=private",
        e.csrf
    );
    // no brief, nothing to plan from
    let (st, _, _) = send(&e.app, post("/projects/plan", base.clone())).await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY);
    // the AI asks first
    let (st, _, asked) = send(
        &e.app,
        post(
            "/projects/plan",
            format!("{base}&brief=askfirst+an+invoice+tool"),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert!(
        asked.contains("Which stack?") && asked.contains("Answer and continue"),
        "{asked}"
    );
    assert!(
        !e.dir.path().join("state/created_repo.json").exists(),
        "nothing created yet"
    );
    // with the answer it proposes tickets
    let (_, _, planned) = send(
        &e.app,
        post(
            "/projects/plan",
            format!("{base}&brief=askfirst+an+invoice+tool&q0=Which+stack%3F&a0=Python"),
        ),
    )
    .await;
    assert!(
        planned.contains("Project skeleton")
            && planned.contains("Create the repository and 2 ticket(s)"),
        "{planned}"
    );
    let log = std::fs::read_to_string(e.dir.path().join("state/planner.log")).unwrap();
    assert!(log.contains("Answer: Python"), "{log}");
    // creating takes the ticked tickets, the unticked one is left out
    let (st, _, done) = send(
        &e.app,
        post(
            "/projects/new",
            format!(
                "{base}&brief=an+invoice+tool&t0_on=on&t0_title=Project+skeleton&t0_body=Set+up&t1_title=First+feature&t1_body=Main+page"
            ),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert!(
        done.contains("Project skeleton") && !done.contains("First feature"),
        "{done}"
    );
    let n: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM tickets t JOIN services s ON s.id = t.service_id WHERE s.external_id = 'acme/plan-app'",
    )
    .fetch_one(&e.pool)
    .await
    .unwrap();
    assert_eq!(n, 1);
}

/// Sign in with a token and give back the cookie.
async fn sign_in(e: &Env, token: &str) -> Option<String> {
    let (st, headers, _) = send(&e.app, form("/login", format!("token={token}"), None)).await;
    if st != StatusCode::SEE_OTHER {
        return None;
    }
    headers
        .get(header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .map(|c| c.split(';').next().unwrap_or("").to_string())
}

#[tokio::test]
async fn a_person_signs_in_with_their_own_token_and_acts_as_themselves() {
    let Some(e) = env().await else { return };
    let (alice,): (i64,) = sqlx::query_as(
        "INSERT INTO principals (channel, ref, role) VALUES ('github', 'alice', 'team_approver') RETURNING id",
    )
    .fetch_one(&e.pool)
    .await
    .unwrap();
    let (_, _, shown) = send(
        &e.app,
        form(
            &format!("/people/{alice}/token"),
            format!("csrf={}", e.csrf),
            Some(e.cookie.clone()),
        ),
    )
    .await;
    let token = shown
        .split("<code")
        .nth(1)
        .and_then(|t| t.split('>').nth(1))
        .and_then(|t| t.split('<').next())
        .unwrap()
        .to_string();
    assert_eq!(token.len(), 64, "{shown}");
    let (_, _, people) = send(&e.app, get("/people", Some(e.cookie.clone()))).await;
    assert!(
        people.contains("/people/") && !people.contains(&token),
        "the token is not shown again"
    );
    assert!(sign_in(&e, "not-a-token").await.is_none());
    let cookie = sign_in(&e, &token).await.expect("the person signs in");
    // they see the work and their own page, not the settings
    for ok in ["/", "/tickets", "/me", "/pulls"] {
        let (st, _, _) = send(&e.app, get(ok, Some(cookie.clone()))).await;
        assert_eq!(st, StatusCode::OK, "{ok}");
    }
    for owner_only in [
        "/services",
        "/people",
        "/accounts",
        "/connections",
        "/knowledge",
    ] {
        let (st, _, _) = send(&e.app, get(owner_only, Some(cookie.clone()))).await;
        assert_eq!(st, StatusCode::FORBIDDEN, "{owner_only}");
    }
    let (_, _, me) = send(&e.app, get("/me", Some(cookie.clone()))).await;
    assert!(
        me.contains("github:alice") && !me.contains("/services"),
        "their menu is short"
    );
    assert!(
        !me.contains("action=\"/pause\""),
        "pausing everything is for owners"
    );
    let (st, _, _) = send(
        &e.app,
        form(
            "/me/admin-token",
            "csrf=x&token=a&again=a".into(),
            Some(cookie.clone()),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    // they approve as themselves, within their role
    let t = seed_ticket(&e, "61", "Needs approval", "awaiting_approval", 2).await;
    sqlx::query("UPDATE tickets SET plan_hash = 'h1' WHERE id = $1")
        .bind(t)
        .execute(&e.pool)
        .await
        .unwrap();
    let csrf: String =
        sqlx::query_scalar("SELECT s.csrf FROM console_sessions s WHERE s.principal_id = $1")
            .bind(alice)
            .fetch_one(&e.pool)
            .await
            .unwrap();
    let (st, _, _) = send(
        &e.app,
        form(
            &format!("/tickets/{t}/decide"),
            format!("csrf={csrf}&plan_hash=h1&decision=approve"),
            Some(cookie.clone()),
        ),
    )
    .await;
    assert!(st.is_redirection());
    let who: String = sqlx::query_scalar("SELECT approver FROM approvals WHERE ticket_id = $1")
        .bind(t)
        .fetch_one(&e.pool)
        .await
        .unwrap();
    assert_eq!(who, "github:alice");
    // a member cannot approve
    sqlx::query("UPDATE principals SET role = 'member' WHERE id = $1")
        .bind(alice)
        .execute(&e.pool)
        .await
        .unwrap();
    let t2 = seed_ticket(&e, "62", "Another", "awaiting_approval", 1).await;
    sqlx::query("UPDATE tickets SET plan_hash = 'h2' WHERE id = $1")
        .bind(t2)
        .execute(&e.pool)
        .await
        .unwrap();
    let (st, _, _) = send(
        &e.app,
        form(
            &format!("/tickets/{t2}/decide"),
            format!("csrf={csrf}&plan_hash=h2&decision=approve"),
            Some(cookie.clone()),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT);
    // turning the sign-in off ends their sessions at once
    send(
        &e.app,
        form(
            &format!("/people/{alice}/token/remove"),
            format!("csrf={}", e.csrf),
            Some(e.cookie.clone()),
        ),
    )
    .await;
    let (st, _, _) = send(&e.app, get("/me", Some(cookie))).await;
    assert_ne!(st, StatusCode::OK);
    assert!(sign_in(&e, &token).await.is_none());
}

#[tokio::test]
async fn reference_documents_are_added_read_and_removed_and_their_credential_stays_secret() {
    let Some(e) = env().await else { return };
    let post = |path: &str, body: String| form(path, body, Some(e.cookie.clone()));
    let add = |extra: &str| post("/knowledge/sources", format!("csrf={}&{extra}", e.csrf));
    for bad in [
        "name=&url=https%3A%2F%2Fwiki.example.com%2Fa",
        "name=Guide&url=ftp%3A%2F%2Fx%2Fy",
        "name=Guide&url=https%3A%2F%2Fuser%3Apw%40wiki.example.com%2Fa",
        "name=Guide&url=https%3A%2F%2Fwiki.example.com%2Fa&header_name=Authorization",
        "name=Guide&url=https%3A%2F%2Fwiki.example.com%2Fa&header_name=Bad%20Name&header_value=x",
    ] {
        let (st, _, _) = send(&e.app, add(bad)).await;
        assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY, "{bad}");
    }
    let (st, _, _) = send(
        &e.app,
        add("name=Deploy+guide&url=https%3A%2F%2Fwiki.example.com%2Fdeploy&header_name=Authorization&header_value=Bearer+topsecret123"),
    )
    .await;
    assert!(st.is_redirection());
    let id: i64 =
        sqlx::query_scalar("SELECT id FROM reference_sources WHERE name = 'Deploy guide'")
            .fetch_one(&e.pool)
            .await
            .unwrap();
    let secret = e.dir.path().join("secrets").join(format!("ref_{id}"));
    assert_eq!(
        std::fs::read_to_string(&secret).unwrap().trim(),
        "Bearer topsecret123"
    );
    let (_, _, page) = send(&e.app, get("/knowledge", Some(e.cookie.clone()))).await;
    assert!(page.contains("Deploy guide") && page.contains("wiki.example.com/deploy"));
    assert!(
        !page.contains("topsecret123"),
        "the credential is never shown again"
    );
    // reading it now goes through the engine and shows what the agents get
    let (_, _, read) = send(
        &e.app,
        post(
            &format!("/knowledge/sources/{id}/test"),
            format!("csrf={}", e.csrf),
        ),
    )
    .await;
    assert!(
        read.contains("Doc from https://wiki.example.com/deploy"),
        "{read}"
    );
    let log = std::fs::read_to_string(e.dir.path().join("state/references.log")).unwrap();
    assert!(
        log.contains("Bearer topsecret123"),
        "the daemon passes the credential to the fetcher only"
    );
    // turn off, then remove (the credential file goes too)
    send(
        &e.app,
        post(
            &format!("/knowledge/sources/{id}/toggle"),
            format!("csrf={}", e.csrf),
        ),
    )
    .await;
    let on: bool = sqlx::query_scalar("SELECT enabled FROM reference_sources WHERE id = $1")
        .bind(id)
        .fetch_one(&e.pool)
        .await
        .unwrap();
    assert!(!on);
    send(
        &e.app,
        post(
            &format!("/knowledge/sources/{id}/delete"),
            format!("csrf={}", e.csrf),
        ),
    )
    .await;
    assert!(!secret.exists());
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM reference_sources")
        .fetch_one(&e.pool)
        .await
        .unwrap();
    assert_eq!(n, 0);
}

#[tokio::test]
async fn the_admin_token_can_be_changed_in_the_console_and_the_old_one_stops_working() {
    let Some(e) = env().await else { return };
    let other = sign_in(&e, ADMIN).await.expect("a second admin session");
    let post = |body: String| form("/me/admin-token", body, Some(e.cookie.clone()));
    let new = "a-brand-new-admin-token-1234567890";
    let (st, _, _) = send(
        &e.app,
        post(format!(
            "csrf={}&token={new}&again=different-token-1234567890xx",
            e.csrf
        )),
    )
    .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY, "the two must match");
    let (st, _, _) = send(
        &e.app,
        post(format!("csrf={}&token=short&again=short", e.csrf)),
    )
    .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY, "too short");
    let (st, _, _) = send(&e.app, post(format!("csrf=nope&token={new}&again={new}"))).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, _, _) = send(
        &e.app,
        post(format!("csrf={}&token={new}&again={new}", e.csrf)),
    )
    .await;
    assert!(st.is_redirection());
    assert!(sign_in(&e, ADMIN).await.is_none(), "the old token is dead");
    assert!(sign_in(&e, new).await.is_some(), "the new one works");
    let (st, _, _) = send(&e.app, get("/me", Some(e.cookie.clone()))).await;
    assert_eq!(st, StatusCode::OK, "the session that made the change stays");
    let (st, _, _) = send(&e.app, get("/me", Some(other))).await;
    assert_ne!(st, StatusCode::OK, "other admin sessions ended");
    // deleting the row brings the environment token back
    sqlx::query("DELETE FROM sync_state WHERE key = 'admin_token_hash'")
        .execute(&e.pool)
        .await
        .unwrap();
    assert!(sign_in(&e, ADMIN).await.is_some());
}

#[tokio::test]
async fn each_person_manages_only_their_own_model_account() {
    let Some(e) = env().await else { return };
    let mut cookies = vec![];
    for who in ["alice", "bob"] {
        let (id,): (i64,) = sqlx::query_as(
            "INSERT INTO principals (channel, ref, role) VALUES ('github', $1, 'member') RETURNING id",
        )
        .bind(who)
        .fetch_one(&e.pool)
        .await
        .unwrap();
        let (_, _, shown) = send(
            &e.app,
            form(
                &format!("/people/{id}/token"),
                format!("csrf={}", e.csrf),
                Some(e.cookie.clone()),
            ),
        )
        .await;
        let token = shown
            .split("<code")
            .nth(1)
            .and_then(|t| t.split('>').nth(1))
            .and_then(|t| t.split('<').next())
            .unwrap()
            .to_string();
        let cookie = sign_in(&e, &token).await.unwrap();
        let csrf: String =
            sqlx::query_scalar("SELECT csrf FROM console_sessions WHERE principal_id = $1")
                .bind(id)
                .fetch_one(&e.pool)
                .await
                .unwrap();
        cookies.push((who, cookie, csrf));
    }
    let (_, ca, ka) = &cookies[0];
    let (_, cb, kb) = &cookies[1];
    let add = |csrf: &str, cookie: &str, tok: &str| {
        form(
            "/me/account",
            format!("csrf={csrf}&kind=oauth&token={tok}"),
            Some(cookie.to_string()),
        )
    };
    let (st, _, _) = send(&e.app, add(ka, ca, "sk-ant-oat01-alicealicealicealice1111")).await;
    assert!(st.is_redirection());
    let (_, _, mine) = send(&e.app, get("/me", Some(ca.clone()))).await;
    assert!(mine.contains("1111") && mine.contains("alice"), "{mine}");
    let (_, _, theirs) = send(&e.app, get("/me", Some(cb.clone()))).await;
    assert!(
        !theirs.contains("1111"),
        "bob does not see alice account: {theirs}"
    );
    // bob cannot remove alice's account by naming it
    let (st, _, _) = send(
        &e.app,
        form(
            "/me/account/remove",
            format!("csrf={kb}&label=alice"),
            Some(cb.clone()),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY);
    let (_, _, still) = send(&e.app, get("/me", Some(ca.clone()))).await;
    assert!(still.contains("1111"));
    // the admin token has no account of its own
    let (st, _, _) = send(
        &e.app,
        form(
            "/me/account",
            format!(
                "csrf={}&kind=oauth&token=sk-ant-oat01-x1111111111111111111",
                e.csrf
            ),
            Some(e.cookie.clone()),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn tickets_of_an_archived_repository_leave_the_lists_but_not_the_database() {
    let Some(e) = env().await else { return };
    let id = seed_ticket(&e, "7", "Old account work", "merged", 1).await;
    let (_, _, before) = send(&e.app, get("/tickets", Some(e.cookie.clone()))).await;
    assert!(before.contains("Old account work"));
    sqlx::query("UPDATE services SET archived = true WHERE external_id = 'org/repo'")
        .execute(&e.pool)
        .await
        .unwrap();
    let (_, _, after) = send(&e.app, get("/tickets", Some(e.cookie.clone()))).await;
    assert!(!after.contains("Old account work"));
    let (_, _, home) = send(&e.app, get("/", Some(e.cookie.clone()))).await;
    assert!(!home.contains("Old account work"));
    let (st, _, _) = send(
        &e.app,
        get(&format!("/tickets/{id}"), Some(e.cookie.clone())),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "a direct link still opens the history");
}

#[tokio::test]
async fn the_pull_requests_page_lists_open_pull_requests_of_watched_repositories_and_needs_a_session(
) {
    let Some(e) = env().await else { return };
    std::fs::write(
        e.dir.path().join("state/pulls_org_repo.json"),
        r#"[{"number":"5","title":"Fix <login>","author":"nabil","bot":false,"draft":true,"branch":"fix-login","base":"main","url":"https://github.com/org/repo/pull/5","updated_at":"2026-11-10T01:00:00Z","ours":false},
            {"number":"6","title":"By tasklane","author":"app","bot":true,"draft":false,"branch":"tasklane/41","base":"main","url":"https://github.com/org/repo/pull/6","updated_at":"2026-11-10T02:00:00Z","ours":true}]"#,
    )
    .unwrap();
    let (st, _, _) = send(&e.app, get("/pulls", None)).await;
    assert_ne!(st, StatusCode::OK, "no session, no list");
    let (st, _, shell) = send(&e.app, get("/pulls?refresh=1", Some(e.cookie.clone()))).await;
    assert_eq!(st, StatusCode::OK);
    assert!(
        shell.contains("/fragments/pulls"),
        "the page opens at once and the list fills in"
    );
    let (st, _, page) = send(&e.app, get("/fragments/pulls", Some(e.cookie.clone()))).await;
    assert_eq!(st, StatusCode::OK);
    assert!(page.contains("org/repo") && page.contains("nabil") && page.contains("fix-login"));
    assert!(
        page.contains("Fix &lt;login&gt;") && !page.contains("Fix <login>"),
        "titles are escaped"
    );
    assert!(page.contains("draft") && page.contains(">tasklane<"));
}

#[tokio::test]
async fn the_review_button_needs_a_session_a_clean_number_and_a_repository_that_is_not_in_watch_mode(
) {
    let Some(e) = env().await else { return };
    let post = |body: String, cookie: Option<String>| form("/pulls/review", body, cookie);
    assert_eq!(
        send(
            &e.app,
            post(
                "csrf=bad&service=org/repo&pr=5".into(),
                Some(e.cookie.clone())
            )
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    for pr in ["", "5%3Bx", "1234567890"] {
        let body = format!("csrf={}&service=org%2Frepo&pr={pr}", e.csrf);
        assert_eq!(
            send(&e.app, post(body, Some(e.cookie.clone()))).await.0,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{pr}"
        );
    }
    let watched = format!("csrf={}&service=org%2Fwatched&pr=5", e.csrf);
    assert_eq!(
        send(&e.app, post(watched, Some(e.cookie.clone()))).await.0,
        StatusCode::UNPROCESSABLE_ENTITY,
        "watch mode means nothing happens"
    );
    let ok = format!("csrf={}&service=org%2Frepo&pr=5", e.csrf);
    assert_eq!(
        send(&e.app, post(ok, Some(e.cookie.clone()))).await.0,
        StatusCode::SEE_OTHER
    );
    // the list offers the button on a pull request
    std::fs::write(
        e.dir.path().join("state/pulls_org_repo.json"),
        r#"[{"number":"9","title":"x","author":"a","bot":false,"draft":false,"branch":"b","base":"main","url":"https://github.com/org/repo/pull/9","updated_at":"t","ours":false}]"#,
    )
    .unwrap();
    let (_, _, list) = send(&e.app, get("/fragments/pulls", Some(e.cookie.clone()))).await;
    assert!(list.contains("/pulls/review") && list.contains("Review with AI"));
    sqlx::query("INSERT INTO audit_events (actor, action, detail) VALUES ('console:admin','pull.reviewed','{\"service\":\"org/repo\",\"pr\":\"9\",\"verdict\":\"changes\",\"notes\":\"split <this> PR\",\"complexity\":\"O(n^2) over rows\"}')")
        .execute(&e.pool)
        .await
        .unwrap();
    let (_, _, list) = send(&e.app, get("/fragments/pulls", Some(e.cookie.clone()))).await;
    assert!(
        list.contains("split &lt;this&gt; PR")
            && list.contains("Complexity (Big-O): O(n^2) over rows")
            && list.contains("Ask the author")
            && list.contains("Review again"),
        "the review is readable in the row, with what to do next: {list}"
    );
}

#[tokio::test]
async fn the_merge_button_appears_only_for_an_approved_commit_in_a_switched_on_repository() {
    let Some(e) = env().await else { return };
    let post = |path: &str, body: String| form(path, body, Some(e.cookie.clone()));
    let pr = |base: &str, head: &str| {
        std::fs::write(
            e.dir.path().join("state/pulls_org_repo.json"),
            format!(
                r#"[{{"number":"9","title":"x","author":"a","bot":false,"draft":false,"branch":"b","base":"{base}","head_sha":"{head}","url":"https://github.com/org/repo/pull/9","updated_at":"t","ours":false}}]"#
            ),
        )
        .unwrap();
    };
    sqlx::query("INSERT INTO audit_events (actor, action, detail) VALUES ('console:admin','pull.reviewed','{\"service\":\"org/repo\",\"pr\":\"9\",\"verdict\":\"approve\",\"notes\":\"ok\",\"head_sha\":\"aaa\"}')")
        .execute(&e.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE services SET facts = facts || '{\"default_branch\":\"main\"}' WHERE external_id = 'org/repo'")
        .execute(&e.pool)
        .await
        .unwrap();
    let list = |label: &'static str| {
        let (app, cookie) = (e.app.clone(), e.cookie.clone());
        async move {
            let _ = send(&app, get("/pulls?refresh=1", Some(cookie.clone()))).await;
            let (_, _, l) = send(&app, get("/fragments/pulls", Some(cookie))).await;
            let _ = label;
            l
        }
    };
    pr("dev", "aaa");
    let off = list("off").await;
    assert!(!off.contains("/pulls/merge"), "off until switched on");
    assert!(
        off.contains("No Merge button") && off.contains("not in full mode"),
        "it says why: {off}"
    );
    // the switch needs full mode, a session and a clean answer
    assert_eq!(
        send(
            &e.app,
            post(
                "/pulls/switch",
                "csrf=bad&service=org%2Frepo&on=1".to_string()
            )
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        send(
            &e.app,
            post(
                "/pulls/switch",
                format!("csrf={}&service=org%2Fwatched&on=1", e.csrf)
            )
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY,
        "a repository in watch mode cannot be switched on"
    );
    sqlx::query("UPDATE services SET mode = 'full' WHERE external_id = 'org/repo'")
        .execute(&e.pool)
        .await
        .unwrap();
    let full_but_off = list("full").await;
    assert!(
        full_but_off.contains("Switch it on") && full_but_off.contains("merging from here is off"),
        "full mode, switch off: one click away"
    );
    assert_eq!(
        send(
            &e.app,
            post(
                "/pulls/switch",
                format!("csrf={}&service=org%2Frepo&on=1", e.csrf)
            )
        )
        .await
        .0,
        StatusCode::SEE_OTHER
    );
    assert!(
        list("on").await.contains("/pulls/merge"),
        "approved, same commit, switched on"
    );
    pr("dev", "zzz");
    let moved = list("moved").await;
    assert!(!moved.contains("/pulls/merge") && moved.contains("changed since review"));
    pr("main", "aaa");
    assert!(
        !list("default").await.contains("/pulls/merge"),
        "never into the default branch from here"
    );
    // the endpoint says no by itself, whatever the page offered
    let (st, _, _) = send(
        &e.app,
        post("/pulls/merge", "csrf=bad&service=org%2Frepo&pr=9".into()),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, loc, _) = send(
        &e.app,
        post(
            "/pulls/merge",
            format!("csrf={}&service=org%2Frepo&pr=9", e.csrf),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::SEE_OTHER);
    let _ = loc;
    let n: i64 =
        sqlx::query_scalar("SELECT count(*) FROM audit_events WHERE action = 'pull.merged'")
            .fetch_one(&e.pool)
            .await
            .unwrap();
    assert_eq!(n, 0, "nothing was merged");
}

#[tokio::test]
async fn the_live_map_ties_pull_requests_to_their_repository_ticket_and_review() {
    let Some(e) = env().await else { return };
    let tid = seed_ticket(&e, "7", "Ours", "ci_running", 1).await;
    std::fs::write(
        e.dir.path().join("state/pulls_org_repo.json"),
        format!(
            r#"[{{"number":"9","title":"By a person","author":"nabil","bot":false,"draft":false,"branch":"fix","base":"dev","head_sha":"a","url":"https://github.com/org/repo/pull/9","updated_at":"t","ours":false}},
                {{"number":"10","title":"By tasklane","author":"app","bot":true,"draft":false,"branch":"tasklane/{tid}","base":"dev","head_sha":"b","url":"https://github.com/org/repo/pull/10","updated_at":"t","ours":true}}]"#
        ),
    )
    .unwrap();
    sqlx::query("INSERT INTO audit_events (actor, action, detail) VALUES ('console:admin','pull.reviewed','{\"service\":\"org/repo\",\"pr\":\"9\",\"verdict\":\"changes\",\"notes\":\"n\",\"head_sha\":\"a\"}')")
        .execute(&e.pool)
        .await
        .unwrap();
    // the map never asks GitHub itself: the first look starts a background load, a later one has the list
    let mut pulls = serde_json::Value::Null;
    for _ in 0..40 {
        let (_, _, body) = send(&e.app, get("/map/data", Some(e.cookie.clone()))).await;
        let d: serde_json::Value = serde_json::from_str(&body).unwrap();
        if d["pulls"].as_array().is_some_and(|a| !a.is_empty()) {
            pulls = d["pulls"].clone();
            assert!(d["pull_events"]
                .as_array()
                .is_some_and(|a| a.iter().any(|x| x["key"] == "org/repo#9")));
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
    let by = |n: &str| {
        pulls
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["num"] == n)
            .cloned()
            .unwrap()
    };
    assert_eq!(by("9")["state"], "changes");
    assert!(by("9")["ticket"].is_null());
    assert_eq!(
        by("10")["ticket"],
        tid,
        "tasklane's own pull request is tied to its ticket"
    );
    assert_eq!(by("10")["state"], "open");
}

#[tokio::test]
async fn a_new_project_is_created_registered_in_triage_and_gets_a_first_ticket() {
    let Some(e) = env().await else { return };
    let (st, _, _) = send(&e.app, get("/projects/new", None)).await;
    assert_ne!(st, StatusCode::OK, "needs a session");
    let (st, _, page) = send(&e.app, get("/projects/new", Some(e.cookie.clone()))).await;
    assert_eq!(st, StatusCode::OK);
    assert!(page.contains("Create the project") && page.contains("Administration"));
    let post = |body: String| form("/projects/new", body, Some(e.cookie.clone()));
    assert_eq!(
        send(&e.app, post("csrf=bad&owner=acme&name=app".into()))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    for bad in [
        "owner=ac%2Fme&name=app",
        "owner=acme&name=..%2Fx",
        "owner=acme&name=app&template=rust",
        "owner=acme&name=x.git",
        "owner=&name=app",
    ] {
        let (st, _, _) = send(&e.app, post(format!("csrf={}&{bad}", e.csrf))).await;
        assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY, "{bad}");
    }
    // GitHub refusing (no Administration permission) says why and registers nothing
    std::fs::write(e.dir.path().join("state/create_denied"), "").unwrap();
    let (st, _, denied) = send(&e.app, post(format!("csrf={}&owner=acme&name=app", e.csrf))).await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(denied.contains("Administration") && denied.contains("Not created"));
    std::fs::remove_file(e.dir.path().join("state/create_denied")).unwrap();
    let body = format!(
        "csrf={}&owner=acme&name=new-app&description=A+thing&template=python&visibility=private&brief=Build+a+small+invoice+tool",
        e.csrf
    );
    let (st, _, ok) = send(&e.app, post(body)).await;
    assert_eq!(st, StatusCode::OK);
    assert!(
        ok.contains("created") && ok.contains("acme/new-app") && ok.contains("First ticket"),
        "{ok}"
    );
    let made = std::fs::read_to_string(e.dir.path().join("state/created_repo.json")).unwrap();
    assert!(
        made.contains("\"private\": true") && made.contains("python"),
        "{made}"
    );
    let (mode, source): (String, String) =
        sqlx::query_as("SELECT mode, source FROM services WHERE external_id = 'acme/new-app'")
            .fetch_one(&e.pool)
            .await
            .unwrap();
    assert_eq!(mode, "triage", "a project starts asking first");
    assert_eq!(source, "test");
    let tickets: i64 = sqlx::query_scalar("SELECT count(*) FROM tickets t JOIN services s ON s.id = t.service_id WHERE s.external_id = 'acme/new-app'")
        .fetch_one(&e.pool)
        .await
        .unwrap();
    assert_eq!(tickets, 1);
    let jobs: i64 = sqlx::query_scalar("SELECT count(*) FROM jobs j JOIN tickets t ON t.id = j.ticket_id JOIN services s ON s.id = t.service_id WHERE s.external_id = 'acme/new-app' AND j.kind = 'triage'")
        .fetch_one(&e.pool)
        .await
        .unwrap();
    assert_eq!(jobs, 1, "the brief goes straight to a plan");
    let audited: i64 =
        sqlx::query_scalar("SELECT count(*) FROM audit_events WHERE action = 'project.created'")
            .fetch_one(&e.pool)
            .await
            .unwrap();
    assert_eq!(audited, 1);
}
