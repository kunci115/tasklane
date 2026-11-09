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
        login_throttle: std::sync::Mutex::new((0, std::time::Instant::now())),
    });
    let app = api::routes().merge(console::routes()).with_state(state);
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
        cookie.contains("HttpOnly") && cookie.contains("SameSite=Strict"),
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
    assert_eq!(mode, "full", "the first approval wakes the repository");
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

#[tokio::test]
async fn the_approver_sees_the_plan_and_the_change_and_a_failed_ticket_can_be_retried() {
    let Some(e) = env().await else { return };
    let (t,): (i64,) = sqlx::query_as("INSERT INTO tickets (service_id, external_ref, state, level, plan_hash, plan_text, diff_text) SELECT id, '3', 'awaiting_approval', 2, 'abc123', 'Plan: <b>rotate</b>', '+ new line' FROM services WHERE external_id = 'org/repo' RETURNING id")
        .fetch_one(&e.pool).await.unwrap();
    let page = send(
        &e.app,
        get(&format!("/tickets/{t}"), Some(e.cookie.clone())),
    )
    .await
    .2;
    assert!(
        page.contains("Plan: &lt;b&gt;rotate&lt;/b&gt;") && page.contains("+ new line"),
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
