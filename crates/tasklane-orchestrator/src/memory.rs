//! Per-repository memory: short notes a person recorded ("the staging branch is `develop`",
//! "never touch `generated/`"). They are added to the agent's instructions for that repository.
//! Only people write notes, so they are as trusted as the instructions typed on the Repositories
//! page; nothing a model or a ticket says is ever stored here.

use serde_json::{json, Value};
use sqlx::PgPool;

/// Notes sent to an agent: the newest first, and not more than this many.
const MAX_NOTES: i64 = 20;
const MAX_CHARS: usize = 1000;

#[derive(Debug, thiserror::Error)]
pub enum MemoryError {
    #[error("a note needs 1 to 1000 characters")]
    BadNote,
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

pub async fn add(
    pool: &PgPool,
    service_id: i64,
    fact: &str,
    who: &str,
) -> Result<i64, MemoryError> {
    let fact = fact.trim();
    let n = fact.chars().count();
    if n == 0 || n > MAX_CHARS {
        return Err(MemoryError::BadNote);
    }
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO memory_notes (service_id, fact, provenance) VALUES ($1, $2, $3) RETURNING id",
    )
    .bind(service_id)
    .bind(fact)
    .bind(who)
    .fetch_one(pool)
    .await?;
    Ok(id)
}

pub async fn delete(pool: &PgPool, service_id: i64, note_id: i64) -> Result<(), MemoryError> {
    sqlx::query("DELETE FROM memory_notes WHERE id = $1 AND service_id = $2")
        .bind(note_id)
        .bind(service_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// The repository's notes, newest first: (id, text, who and when).
pub async fn list(
    pool: &PgPool,
    service_id: i64,
) -> Result<Vec<(i64, String, String)>, sqlx::Error> {
    sqlx::query_as(
        "SELECT id, fact, provenance || ', ' || to_char(created_at, 'YYYY-MM-DD') FROM memory_notes
         WHERE service_id = $1 ORDER BY id DESC LIMIT $2",
    )
    .bind(service_id)
    .bind(MAX_NOTES)
    .fetch_all(pool)
    .await
}

/// `profile` with the repository's notes added to its `system` instructions.
pub async fn with_notes(
    pool: &PgPool,
    source: &str,
    service: &str,
    profile: &Value,
) -> Result<Value, sqlx::Error> {
    let notes: Vec<String> = sqlx::query_scalar(
        "SELECT m.fact FROM memory_notes m JOIN services s ON s.id = m.service_id
         WHERE s.source = $1 AND s.external_id = $2 ORDER BY m.id DESC LIMIT $3",
    )
    .bind(source)
    .bind(service)
    .bind(MAX_NOTES)
    .fetch_all(pool)
    .await?;
    if notes.is_empty() {
        return Ok(profile.clone());
    }
    let mut p = match profile {
        Value::Object(_) => profile.clone(),
        _ => json!({}),
    };
    let base = p
        .get("system")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let list: String = notes.iter().map(|n| format!("- {n}\n")).collect();
    let text = if base.is_empty() {
        format!("Notes the team recorded about this repository:\n{list}")
    } else {
        format!("{base}\n\nNotes the team recorded about this repository:\n{list}")
    };
    p["system"] = Value::String(text);
    Ok(p)
}
