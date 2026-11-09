//! JSON endpoints: health, the report button, and source webhooks.

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::auth::verify_hs256;
use crate::tickets::{create_ticket, CreateError, Created};
use crate::{now_unix, AppState};

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/readyz", get(ready))
        .route("/metrics", get(metrics))
        .route(
            "/api/v1/report",
            post(report).layer(DefaultBodyLimit::max(64 * 1024)),
        )
        .route(
            "/api/v1/webhook/source",
            post(webhook).layer(DefaultBodyLimit::max(1024 * 1024)),
        )
}

/// Prometheus text metrics. Off unless `TASKLANE_METRICS_TOKEN` is set (16 characters or more);
/// then the caller sends it as `Authorization: Bearer <token>`.
async fn metrics(State(st): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    use subtle::ConstantTimeEq;
    let Some(want) = st.metrics_token.as_deref() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let got = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");
    let same: bool = got.as_bytes().ct_eq(want.as_bytes()).into();
    if !same {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let pool = st.orch.pool();
    let mut out = String::new();
    let rows = |sql: &'static str| async move {
        sqlx::query_as::<_, (String, i64)>(sql)
            .fetch_all(pool)
            .await
            .unwrap_or_default()
    };
    out.push_str(
        "# HELP tasklane_up Is the daemon answering.\n# TYPE tasklane_up gauge\ntasklane_up 1\n",
    );
    out.push_str("# HELP tasklane_tickets Tickets by state.\n# TYPE tasklane_tickets gauge\n");
    for (state, n) in rows("SELECT state, count(*) FROM tickets GROUP BY state").await {
        out.push_str(&format!("tasklane_tickets{{state=\"{state}\"}} {n}\n"));
    }
    out.push_str("# HELP tasklane_jobs Queue jobs by status.\n# TYPE tasklane_jobs gauge\n");
    for (status, n) in rows("SELECT status, count(*) FROM jobs GROUP BY status").await {
        out.push_str(&format!("tasklane_jobs{{status=\"{status}\"}} {n}\n"));
    }
    out.push_str(
        "# HELP tasklane_repositories Repositories by mode.\n# TYPE tasklane_repositories gauge\n",
    );
    for (mode, n) in
        rows("SELECT mode, count(*) FROM services WHERE NOT archived GROUP BY mode").await
    {
        out.push_str(&format!("tasklane_repositories{{mode=\"{mode}\"}} {n}\n"));
    }
    let (tokens, runs): (i64, i64) = (
        sqlx::query_scalar("SELECT coalesce(sum(tokens_used), 0)::bigint FROM tickets")
            .fetch_one(pool)
            .await
            .unwrap_or(0),
        sqlx::query_scalar("SELECT count(*) FROM runs")
            .fetch_one(pool)
            .await
            .unwrap_or(0),
    );
    out.push_str(&format!(
        "# HELP tasklane_model_tokens_total Model tokens used by all tickets.\n# TYPE tasklane_model_tokens_total counter\ntasklane_model_tokens_total {tokens}\n\
# HELP tasklane_model_runs_total Model runs recorded.\n# TYPE tasklane_model_runs_total counter\ntasklane_model_runs_total {runs}\n"
    ));
    (
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; version=0.0.4",
        )],
        out,
    )
        .into_response()
}

async fn ready(State(st): State<Arc<AppState>>) -> Response {
    match sqlx::query_scalar::<_, i32>("SELECT 1")
        .fetch_one(st.orch.pool())
        .await
    {
        Ok(_) if st.orch.plugins_ready().await => (StatusCode::OK, "ready").into_response(),
        Ok(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            "a plug-in is not answering",
        )
            .into_response(),
        Err(_) => (StatusCode::SERVICE_UNAVAILABLE, "database unavailable").into_response(),
    }
}

fn err(status: StatusCode, msg: &str) -> Response {
    (status, Json(json!({"error": msg}))).into_response()
}

#[derive(Deserialize)]
struct ReportIn {
    title: String,
    #[serde(default)]
    body: String,
}

/// The "report a problem" button. The app signs a short token (HS256) with a
/// secret it shares with tasklane. Claims: `iss` = platform name, `sub` = the
/// reporter, `svc` = the repository the report is for, `exp`. The app, not the
/// browser, decides `svc`, so a reporter cannot aim a report at another repository.
async fn report(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(input): Json<ReportIn>,
) -> Response {
    let Some(token) = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
    else {
        return err(StatusCode::UNAUTHORIZED, "missing bearer token");
    };
    // The issuer is read from the unverified payload only to choose which secret to check with.
    let iss = token
        .split('.')
        .nth(1)
        .and_then(base64_decode)
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .and_then(|v| v.get("iss").and_then(Value::as_str).map(str::to_string));
    let Some(secret) = iss.as_ref().and_then(|i| st.jwt_secrets.get(i)) else {
        return err(StatusCode::UNAUTHORIZED, "unknown platform");
    };
    let claims = match verify_hs256(token, secret.as_bytes(), now_unix()) {
        Ok(c) => c,
        Err(_) => return err(StatusCode::UNAUTHORIZED, "invalid token"),
    };
    let (Some(sub), Some(svc)) = (
        claims.get("sub").and_then(Value::as_str),
        claims.get("svc").and_then(Value::as_str),
    ) else {
        return err(StatusCode::UNAUTHORIZED, "token needs sub and svc");
    };
    let reporter = format!("{}:{}", iss.as_deref().unwrap_or(""), sub);
    let via = format!("{} by {}", iss.as_deref().unwrap_or(""), sub);
    match create_ticket(
        &st.orch,
        svc,
        &reporter,
        &input.title,
        &input.body,
        &via,
        st.user_daily_limit,
    )
    .await
    {
        Ok(Created::New(reference)) => (
            StatusCode::CREATED,
            Json(json!({"ref": reference, "duplicate": false})),
        )
            .into_response(),
        Ok(Created::Duplicate(reference)) => (
            StatusCode::OK,
            Json(json!({"ref": reference, "duplicate": true})),
        )
            .into_response(),
        Err(CreateError::BadTitle) => err(
            StatusCode::UNPROCESSABLE_ENTITY,
            "title must be 1 to 200 characters",
        ),
        Err(CreateError::BodyTooLong) => err(StatusCode::UNPROCESSABLE_ENTITY, "body is too long"),
        Err(CreateError::UnknownService) => err(StatusCode::NOT_FOUND, "unknown service"),
        Err(CreateError::ReadOnlyService) => err(
            StatusCode::CONFLICT,
            "this service is not accepting reports",
        ),
        Err(CreateError::LimitReached) => {
            err(StatusCode::TOO_MANY_REQUESTS, "daily report limit reached")
        }
        Err(CreateError::Source) => err(StatusCode::BAD_GATEWAY, "could not create the ticket"),
        Err(CreateError::Db) => err(StatusCode::INTERNAL_SERVER_ERROR, "database error"),
    }
}

fn base64_decode(s: &str) -> Option<Vec<u8>> {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;
    URL_SAFE_NO_PAD.decode(s).ok()
}

/// Webhooks from the ticket source: the plug-in checks the signature; a valid
/// event makes the sync job look at that item right away.
async fn webhook(State(st): State<Arc<AppState>>, headers: HeaderMap, body: Bytes) -> Response {
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine;
    let hdrs: serde_json::Map<String, Value> = headers
        .iter()
        .filter_map(|(k, v)| {
            v.to_str()
                .ok()
                .map(|v| (k.as_str().to_string(), Value::String(v.to_string())))
        })
        .collect();
    let r = st
        .orch
        .source_call(
            "verify_webhook",
            json!({"headers": hdrs, "body_b64": STANDARD.encode(&body)}),
        )
        .await;
    let event = match r {
        Ok(v) => v,
        Err(_) => return err(StatusCode::UNAUTHORIZED, "not accepted"),
    };
    let ev = event.get("event").cloned().unwrap_or(Value::Null);
    if let (Some(service), Some(reference), Some(kind)) = (
        ev.get("service").and_then(Value::as_str),
        ev.get("ref").and_then(Value::as_str),
        ev.get("kind").and_then(Value::as_str),
    ) {
        if matches!(kind, "issue" | "comment" | "label") {
            if let Err(e) = st.orch.note_issue(service, reference).await {
                eprintln!("webhook: {e}");
            }
        }
    }
    StatusCode::ACCEPTED.into_response()
}
