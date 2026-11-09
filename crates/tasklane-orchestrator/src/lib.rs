//! The orchestrator: one worker loop that claims jobs from the queue, calls
//! plug-in programs, and moves tickets through the state machine.
//!
//! Rules this crate keeps (docs/design/01-flow-and-state-machine.md):
//! * A ticket only changes state in a transaction that first re-checks the
//!   job's lease and fence, so a stale worker changes nothing.
//! * Every transition goes through `tasklane_core::transition`.
//! * The final level is the deterministic floor, possibly raised by the model.
//! * Plug-ins get only the environment they are given; nothing is inherited.

pub mod host;
mod triage;

pub use triage::{Config, Orchestrator, RunResult};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Queue(#[from] tasklane_queue::Error),
    #[error(transparent)]
    Db(#[from] sqlx::Error),
    #[error(transparent)]
    Plugin(#[from] host::CallError),
    #[error("plug-in returned something unusable: {0}")]
    BadOutput(String),
    #[error(transparent)]
    Transition(#[from] tasklane_core::TransitionError),
    #[error("unknown ticket state in database: {0}")]
    UnknownState(String),
}
