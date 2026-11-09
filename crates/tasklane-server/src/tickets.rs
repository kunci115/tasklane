//! Creating a ticket on behalf of a reporter: shared by the report API and the chat bridge.

use serde_json::{json, Value};
use tasklane_orchestrator::Orchestrator;

#[derive(Debug, PartialEq, Eq)]
pub enum Created {
    New(String),
    Duplicate(String),
}

#[derive(Debug, PartialEq, Eq)]
pub enum CreateError {
    BadTitle,
    BodyTooLong,
    UnknownService,
    ReadOnlyService,
    LimitReached,
    Source,
    Db,
}

/// Create the issue on the ticket source and the ticket row with a triage job.
/// `reporter` is a stable id such as `shop:42` or `telegram:12345`; the limit and
/// the duplicate check are per reporter.
pub async fn create_ticket(
    orch: &Orchestrator,
    service: &str,
    reporter: &str,
    title: &str,
    body: &str,
    via: &str,
    daily_limit: i64,
) -> Result<Created, CreateError> {
    let title = title.trim();
    if title.is_empty() || title.chars().count() > 200 {
        return Err(CreateError::BadTitle);
    }
    if body.chars().count() > 20_000 {
        return Err(CreateError::BodyTooLong);
    }
    let pool = orch.pool();
    let svc: Option<(i64, String)> = sqlx::query_as(
        "SELECT id, mode FROM services WHERE source = $1 AND external_id = $2 AND NOT archived",
    )
    .bind(&orch.config().source_name)
    .bind(service)
    .fetch_optional(pool)
    .await
    .map_err(|_| CreateError::Db)?;
    let Some((service_id, mode)) = svc else {
        return Err(CreateError::UnknownService);
    };
    if mode == "watch" {
        return Err(CreateError::ReadOnlyService);
    }
    let recent: i64 = sqlx::query_scalar("SELECT count(*) FROM tickets WHERE reporter_ref = $1 AND created_at > now() - interval '1 day'")
        .bind(reporter)
        .fetch_one(pool)
        .await
        .map_err(|_| CreateError::Db)?;
    if recent >= daily_limit {
        return Err(CreateError::LimitReached);
    }
    let dup: Option<String> = sqlx::query_scalar(
        "SELECT external_ref FROM tickets WHERE reporter_ref = $1 AND service_id = $2 AND lower(title) = lower($3) AND created_at > now() - interval '1 day' LIMIT 1",
    )
    .bind(reporter)
    .bind(service_id)
    .bind(title)
    .fetch_optional(pool)
    .await
    .map_err(|_| CreateError::Db)?;
    if let Some(reference) = dup {
        return Ok(Created::Duplicate(reference));
    }

    let body = format!("{body}\n\n_Reported through {via}._");
    let made = orch
        .source_call(
            "create_item",
            json!({"service": service, "title": title, "body": body, "labels": ["tasklane"]}),
        )
        .await
        .map_err(|e| {
            eprintln!("create_item failed: {e}");
            CreateError::Source
        })?;
    let reference = made
        .get("ref")
        .and_then(Value::as_str)
        .ok_or(CreateError::Source)?
        .to_string();

    let mut tx = pool.begin().await.map_err(|_| CreateError::Db)?;
    let inserted: Option<bool> = sqlx::query_scalar(
        "INSERT INTO tickets (service_id, external_ref, reporter_ref, title) VALUES ($1,$2,$3,$4)
         ON CONFLICT (service_id, external_ref) DO UPDATE SET reporter_ref = COALESCE(tickets.reporter_ref, $3), title = COALESCE(tickets.title, $4)
         RETURNING (xmax = 0)",
    )
    .bind(service_id)
    .bind(&reference)
    .bind(reporter)
    .bind(title)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|_| CreateError::Db)?;
    // Inserted now, as opposed to created earlier by the sync job (which already queued triage).
    if inserted == Some(true) {
        sqlx::query("INSERT INTO jobs (ticket_id, kind) SELECT id, 'triage' FROM tickets WHERE service_id = $1 AND external_ref = $2")
            .bind(service_id)
            .bind(&reference)
            .execute(&mut *tx)
            .await
            .map_err(|_| CreateError::Db)?;
    }
    tx.commit().await.map_err(|_| CreateError::Db)?;
    Ok(Created::New(reference))
}
