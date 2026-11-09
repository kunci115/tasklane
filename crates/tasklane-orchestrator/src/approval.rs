//! Approvals. A person with the right role says yes or no to one exact plan;
//! nothing else moves a ticket out of `awaiting_approval` (not a timer, not free text).

use serde_json::json;
use sqlx::PgPool;
use tasklane_core::{transition, Actor, Level, State};

#[derive(Debug, Clone)]
pub struct Principal {
    pub channel: String,
    pub reference: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Approve,
    Reject,
}

#[derive(Debug, thiserror::Error)]
pub enum ApprovalError {
    #[error("{0}")]
    NotAllowed(String),
    #[error("no such ticket")]
    UnknownTicket,
    #[error("ticket is {0:?}, not awaiting approval")]
    WrongState(State),
    #[error("the plan changed since this approval was requested; look at the ticket again")]
    StalePlan,
    #[error(transparent)]
    Transition(#[from] tasklane_core::TransitionError),
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

/// state, level, plan hash, branch, pull request
type TicketRow = (
    String,
    Option<i16>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

fn role_allows(role: &str, level: Level) -> bool {
    match (role, level) {
        ("owner", _) => true,
        ("team_approver", l) => l <= Level::L2,
        _ => false,
    }
}

/// Record a decision on a ticket. `plan_hash` is the hash the approver was shown;
/// if the ticket's plan has changed since, the decision is refused.
pub async fn decide(
    pool: &PgPool,
    ticket_id: i64,
    who: &Principal,
    via: &str,
    decision: Decision,
    plan_hash: &str,
) -> Result<State, ApprovalError> {
    let mut tx = pool.begin().await?;
    let role: Option<String> =
        sqlx::query_scalar("SELECT role FROM principals WHERE channel = $1 AND ref = $2")
            .bind(&who.channel)
            .bind(&who.reference)
            .fetch_optional(&mut *tx)
            .await?;
    let Some(role) = role else {
        return Err(ApprovalError::NotAllowed(
            "you are not registered as an approver".into(),
        ));
    };
    let row: Option<TicketRow> = sqlx::query_as(
        "SELECT state, level, plan_hash, branch, pr_ref, head_sha, pushed_head FROM tickets WHERE id = $1 FOR UPDATE",
    )
    .bind(ticket_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((state, level, current_hash, branch, pr_ref, head_sha, pushed_head)) = row else {
        return Err(ApprovalError::UnknownTicket);
    };
    let state = State::parse(&state).unwrap_or(State::Failed);
    if state != State::AwaitingApproval {
        return Err(ApprovalError::WrongState(state));
    }
    let level = level
        .and_then(|l| u8::try_from(l).ok())
        .and_then(|l| Level::try_from(l).ok())
        .unwrap_or(Level::L3);
    if !role_allows(&role, level) {
        return Err(ApprovalError::NotAllowed(format!(
            "level {} needs a higher role than {role}",
            u8::from(level)
        )));
    }
    if current_hash.as_deref() != Some(plan_hash) {
        return Err(ApprovalError::StalePlan);
    }
    let (to, kind) = match decision {
        Decision::Reject => (State::ClosedRejected, None),
        Decision::Approve => {
            let kind = resume_kind(
                branch.as_deref(),
                pr_ref.as_deref(),
                head_sha.as_deref(),
                pushed_head.as_deref(),
            );
            (State::Queued, Some(kind))
        }
    };
    transition(State::AwaitingApproval, to, Actor::Approver)?;
    sqlx::query("UPDATE tickets SET state = $2, updated_at = now() WHERE id = $1")
        .bind(ticket_id)
        .bind(to.as_str())
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO approvals (ticket_id, level, plan_hash, approver, decision, via) VALUES ($1,$2,$3,$4,$5,$6)")
        .bind(ticket_id)
        .bind(i16::from(u8::from(level)))
        .bind(plan_hash)
        .bind(format!("{}:{}", who.channel, who.reference))
        .bind(if decision == Decision::Approve { "approve" } else { "reject" })
        .bind(via)
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO audit_events (actor, action, ticket_id, detail) VALUES ($1, $2, $3, $4)")
        .bind(format!("{}:{}", who.channel, who.reference))
        .bind("ticket.decided")
        .bind(ticket_id)
        .bind(json!({"decision": if decision == Decision::Approve {"approve"} else {"reject"}, "via": via, "level": u8::from(level), "plan_hash": plan_hash}))
        .execute(&mut *tx)
        .await?;
    if decision == Decision::Approve {
        // The first approved ticket wakes a read-only repo for development.
        sqlx::query("UPDATE services SET mode = 'full' WHERE mode = 'triage' AND id = (SELECT service_id FROM tickets WHERE id = $1)")
            .bind(ticket_id)
            .execute(&mut *tx)
            .await?;
    }
    if let Some(kind) = kind {
        sqlx::query("INSERT INTO jobs (ticket_id, kind) VALUES ($1, $2)")
            .bind(ticket_id)
            .bind(kind)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(to)
}

/// Which job continues the work after an approval or a retry. Work that exists
/// only locally must be pushed first; only a pushed, unchanged head can be merged.
pub fn resume_kind(
    branch: Option<&str>,
    pr_ref: Option<&str>,
    head: Option<&str>,
    pushed: Option<&str>,
) -> &'static str {
    match (branch, pr_ref) {
        (_, Some(_)) if head.is_some() && head == pushed => "merge",
        (Some(_), _) => "pr",
        _ => "develop",
    }
}

/// A person puts a failed ticket back to work (or closes it). Attempt counters
/// start again; the reason of the failure stays on the ticket until work succeeds.
pub async fn retry_or_close(
    pool: &PgPool,
    ticket_id: i64,
    who: &Principal,
    close: bool,
) -> Result<State, ApprovalError> {
    let mut tx = pool.begin().await?;
    let role: Option<String> =
        sqlx::query_scalar("SELECT role FROM principals WHERE channel = $1 AND ref = $2")
            .bind(&who.channel)
            .bind(&who.reference)
            .fetch_optional(&mut *tx)
            .await?;
    if !matches!(role.as_deref(), Some("owner") | Some("team_approver")) {
        return Err(ApprovalError::NotAllowed(
            "you are not registered as an approver".into(),
        ));
    }
    let row: Option<TicketRow> = sqlx::query_as("SELECT state, level, plan_hash, branch, pr_ref, head_sha, pushed_head FROM tickets WHERE id = $1 FOR UPDATE")
        .bind(ticket_id)
        .fetch_optional(&mut *tx)
        .await?;
    let Some((state, _level, _hash, branch, pr_ref, head, pushed)) = row else {
        return Err(ApprovalError::UnknownTicket);
    };
    let state = State::parse(&state).unwrap_or(State::Failed);
    if state != State::Failed {
        return Err(ApprovalError::WrongState(state));
    }
    let to = if close {
        State::ClosedRejected
    } else {
        State::Queued
    };
    transition(State::Failed, to, Actor::Approver)?;
    sqlx::query("UPDATE tickets SET state = $2, ci_runs = 0, dev_attempts = 0, updated_at = now() WHERE id = $1")
        .bind(ticket_id)
        .bind(to.as_str())
        .execute(&mut *tx)
        .await?;
    if !close {
        // A retry never skips the review: only a head a reviewer approved may go straight to merge.
        let reviewed: Option<String> =
            sqlx::query_scalar("SELECT reviewed_head FROM tickets WHERE id = $1")
                .bind(ticket_id)
                .fetch_one(&mut *tx)
                .await?;
        let head = head.filter(|h| reviewed.as_deref() == Some(h.as_str()));
        let kind = resume_kind(
            branch.as_deref(),
            pr_ref.as_deref(),
            head.as_deref(),
            pushed.as_deref(),
        );
        sqlx::query("INSERT INTO jobs (ticket_id, kind) VALUES ($1, $2)")
            .bind(ticket_id)
            .bind(kind)
            .execute(&mut *tx)
            .await?;
    }
    sqlx::query(
        "INSERT INTO audit_events (actor, action, ticket_id, detail) VALUES ($1, $2, $3, '{}')",
    )
    .bind(format!("{}:{}", who.channel, who.reference))
    .bind(if close {
        "ticket.closed_by_person"
    } else {
        "ticket.retried_by_person"
    })
    .bind(ticket_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(to)
}

/// A person answers a duplicate proposal: close the ticket as a duplicate, or send it to triage
/// as an ordinary ticket. Only the roles that may approve can do this.
pub async fn resolve_duplicate(
    pool: &PgPool,
    ticket_id: i64,
    who: &Principal,
    confirm: bool,
) -> Result<State, ApprovalError> {
    let mut tx = pool.begin().await?;
    let role: Option<String> =
        sqlx::query_scalar("SELECT role FROM principals WHERE channel = $1 AND ref = $2")
            .bind(&who.channel)
            .bind(&who.reference)
            .fetch_optional(&mut *tx)
            .await?;
    if !matches!(role.as_deref(), Some("owner") | Some("team_approver")) {
        return Err(ApprovalError::NotAllowed(
            "you are not registered as an approver".into(),
        ));
    }
    let state: Option<String> =
        sqlx::query_scalar("SELECT state FROM tickets WHERE id = $1 FOR UPDATE")
            .bind(ticket_id)
            .fetch_optional(&mut *tx)
            .await?;
    let Some(state) = state else {
        return Err(ApprovalError::UnknownTicket);
    };
    let state = State::parse(&state).unwrap_or(State::Failed);
    if state != State::DuplicateProposed {
        return Err(ApprovalError::WrongState(state));
    }
    let to = if confirm {
        State::ClosedDuplicate
    } else {
        State::New
    };
    transition(State::DuplicateProposed, to, Actor::Approver)?;
    sqlx::query("UPDATE tickets SET state = $2, updated_at = now() WHERE id = $1")
        .bind(ticket_id)
        .bind(to.as_str())
        .execute(&mut *tx)
        .await?;
    if !confirm {
        sqlx::query("INSERT INTO jobs (ticket_id, kind) VALUES ($1, 'triage')")
            .bind(ticket_id)
            .execute(&mut *tx)
            .await?;
    }
    sqlx::query(
        "INSERT INTO audit_events (actor, action, ticket_id, detail) VALUES ($1, $2, $3, '{}')",
    )
    .bind(format!("{}:{}", who.channel, who.reference))
    .bind(if confirm {
        "ticket.duplicate_confirmed"
    } else {
        "ticket.duplicate_rejected"
    })
    .bind(ticket_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(to)
}
