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
use crate::{now_unix, AppState};

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/readyz", get(ready))
        .route(
            "/api/v1/report",
            post(report).layer(DefaultBodyLimit::max(64 * 1024)),
        )
        .route(
            "/api/v1/webhook/source",
            post(webhook).layer(DefaultBodyLimit::max(1024 * 1024)),
        )
}

async fn ready(State(st): State<Arc<AppState>>) -> Response {
    match sqlx::query_scalar::<_, i32>("SELECT 1")
        .fetch_one(st.orch.pool())
        .await
    {
        Ok(_) => (StatusCode::OK, "ready").into_response(),
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
    let title = input.title.trim();
    if title.is_empty() || title.chars().count() > 200 {
        return err(
            StatusCode::UNPROCESSABLE_ENTITY,
            "title must be 1 to 200 characters",
        );
    }
    if input.body.chars().count() > 20_000 {
        return err(StatusCode::UNPROCESSABLE_ENTITY, "body is too long");
    }

    let pool = st.orch.pool();
    let svc_row: Option<(i64, String)> = match sqlx::query_as(
        "SELECT id, mode FROM services WHERE source = $1 AND external_id = $2 AND NOT archived",
    )
    .bind(&st.orch.config().source_name)
    .bind(svc)
    .fetch_optional(pool)
    .await
    {
        Ok(r) => r,
        Err(_) => return err(StatusCode::INTERNAL_SERVER_ERROR, "database error"),
    };
    let Some((service_id, mode)) = svc_row else {
        return err(StatusCode::NOT_FOUND, "unknown service");
    };
    if mode == "watch" {
        return err(
            StatusCode::CONFLICT,
            "this service is not accepting reports",
        );
    }

    // Per-reporter limit over the last day.
    let recent: i64 = sqlx::query_scalar("SELECT count(*) FROM tickets WHERE reporter_ref = $1 AND created_at > now() - interval '1 day'")
        .bind(&reporter)
        .fetch_one(pool)
        .await
        .unwrap_or(0);
    if recent >= st.user_daily_limit {
        return err(StatusCode::TOO_MANY_REQUESTS, "daily report limit reached");
    }
    // The same title from the same reporter in the last day is the same report.
    let dup: Option<String> = sqlx::query_scalar(
        "SELECT external_ref FROM tickets WHERE reporter_ref = $1 AND service_id = $2 AND lower(title) = lower($3) AND created_at > now() - interval '1 day' LIMIT 1",
    )
    .bind(&reporter)
    .bind(service_id)
    .bind(title)
    .fetch_optional(pool)
    .await
    .unwrap_or(None);
    if let Some(reference) = dup {
        return (
            StatusCode::OK,
            Json(json!({"ref": reference, "duplicate": true})),
        )
            .into_response();
    }

    let body = format!(
        "{}\n\n_Reported through {} by {}._",
        input.body,
        iss.as_deref().unwrap_or(""),
        sub
    );
    let created = match st
        .orch
        .source_call(
            "create_item",
            json!({"service": svc, "title": title, "body": body, "labels": ["tasklane"]}),
        )
        .await
    {
        Ok(v) => v,
        Err(e) => {
            eprintln!("report: create_item failed: {e}");
            return err(StatusCode::BAD_GATEWAY, "could not create the ticket");
        }
    };
    let Some(reference) = created.get("ref").and_then(Value::as_str) else {
        return err(
            StatusCode::BAD_GATEWAY,
            "ticket source returned no reference",
        );
    };
    let r: Result<(), sqlx::Error> = async {
        let mut tx = pool.begin().await?;
        let inserted: Option<bool> = sqlx::query_scalar(
            "INSERT INTO tickets (service_id, external_ref, reporter_ref, title) VALUES ($1,$2,$3,$4)
             ON CONFLICT (service_id, external_ref) DO UPDATE SET reporter_ref = COALESCE(tickets.reporter_ref, $3), title = COALESCE(tickets.title, $4)
             RETURNING (xmax = 0)",
        )
        .bind(service_id)
        .bind(reference)
        .bind(&reporter)
        .bind(title)
        .fetch_optional(&mut *tx)
        .await?;
        // Inserted now, as opposed to created earlier by the sync job (which already queued triage).
        if inserted == Some(true) {
            sqlx::query("INSERT INTO jobs (ticket_id, kind) SELECT id, 'triage' FROM tickets WHERE service_id = $1 AND external_ref = $2")
                .bind(service_id)
                .bind(reference)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await
    }
    .await;
    if r.is_err() {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "database error");
    }
    (
        StatusCode::CREATED,
        Json(json!({"ref": reference, "duplicate": false})),
    )
        .into_response()
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
