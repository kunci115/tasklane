//! Per-repository memory: short notes a person recorded ("the staging branch is `develop`",
//! "never touch `generated/`"). They are added to the agent's instructions for that repository.
//! Only people write notes, so they are as trusted as the instructions typed on the Repositories
//! page; nothing a model or a ticket says is ever stored here.

use serde_json::{json, Value};
use sqlx::PgPool;

/// Notes sent to an agent: the newest first, and not more than this many.
const MAX_NOTES: i64 = 20;
/// Notes for every repository sent to an agent.
const MAX_GLOBAL: i64 = 10;
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

/// A note for every repository.
pub async fn add_global(pool: &PgPool, fact: &str, who: &str) -> Result<i64, MemoryError> {
    let fact = fact.trim();
    let n = fact.chars().count();
    if n == 0 || n > MAX_CHARS {
        return Err(MemoryError::BadNote);
    }
    Ok(sqlx::query_scalar(
        "INSERT INTO memory_notes (service_id, fact, provenance) VALUES (NULL, $1, $2) RETURNING id",
    )
    .bind(fact)
    .bind(who)
    .fetch_one(pool)
    .await?)
}

/// Change the text of a note (repository or global).
pub async fn update(pool: &PgPool, note_id: i64, fact: &str) -> Result<(), MemoryError> {
    let fact = fact.trim();
    let n = fact.chars().count();
    if n == 0 || n > MAX_CHARS {
        return Err(MemoryError::BadNote);
    }
    sqlx::query("UPDATE memory_notes SET fact = $2 WHERE id = $1")
        .bind(note_id)
        .bind(fact)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn delete_global(pool: &PgPool, note_id: i64) -> Result<(), MemoryError> {
    sqlx::query("DELETE FROM memory_notes WHERE id = $1 AND service_id IS NULL")
        .bind(note_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Notes for every repository, newest first: (id, text, who and when).
pub async fn list_global(pool: &PgPool) -> Result<Vec<(i64, String, String)>, sqlx::Error> {
    sqlx::query_as(
        "SELECT id, fact, provenance || ', ' || to_char(created_at, 'YYYY-MM-DD') FROM memory_notes
         WHERE service_id IS NULL ORDER BY id DESC LIMIT $1",
    )
    .bind(MAX_GLOBAL)
    .fetch_all(pool)
    .await
}

/// Paths in the repository the agents are told to read first, from the profile's `docs` list.
pub fn docs_of(profile: &Value) -> Vec<String> {
    profile
        .get("docs")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .filter(|p| valid_doc_path(p))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// A relative path inside the repository: no `..`, no leading slash, plain characters.
pub fn valid_doc_path(p: &str) -> bool {
    !p.is_empty()
        && p.len() <= 200
        && !p.starts_with('/')
        && !p.starts_with('-')
        && !p.contains("..")
        && p.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._/-".contains(&b))
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

/// `profile` with the team's notes (for every repository, and for this one) and the documents to
/// read first added to its `system` instructions.
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
    let global: Vec<String> = list_global(pool)
        .await?
        .into_iter()
        .map(|(_, f, _)| f)
        .collect();
    let docs = docs_of(profile);
    if notes.is_empty() && global.is_empty() && docs.is_empty() {
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
    let mut text = base;
    let mut section = |title: &str, lines: String| {
        if !text.is_empty() {
            text.push_str("\n\n");
        }
        text.push_str(title);
        text.push('\n');
        text.push_str(&lines);
    };
    if !global.is_empty() {
        section(
            "Notes the team recorded for every repository:",
            global.iter().map(|n| format!("- {n}\n")).collect(),
        );
    }
    if !notes.is_empty() {
        section(
            "Notes the team recorded about this repository:",
            notes.iter().map(|n| format!("- {n}\n")).collect(),
        );
    }
    if !docs.is_empty() {
        section(
            "Read these paths in the repository first when they exist (they describe how the team works):",
            docs.iter().map(|d| format!("- {d}\n")).collect(),
        );
    }
    p["system"] = Value::String(text);
    Ok(p)
}
