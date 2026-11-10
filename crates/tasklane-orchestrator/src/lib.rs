//! The orchestrator: a worker loop that claims jobs from the queue, calls
//! plug-in programs, and moves tickets through the state machine.
//!
//! Rules this crate keeps (docs/design/01-flow-and-state-machine.md):
//! * A ticket only changes state in a transaction that first re-checks the
//!   job's lease and fence, so a stale worker changes nothing.
//! * Every transition goes through `tasklane_core::transition`.
//! * The final level is the deterministic floor, possibly raised by the model.
//! * Plug-ins get only the environment they are given; nothing is inherited.
//! * The agent never pushes or merges; this crate's git and source calls do,
//!   with tokens that live for one run.

pub mod approval;
mod ask;
mod attribution;
mod ctx;
mod duplicates;
mod feedback;
pub mod host;
pub mod memory;
mod messages;
mod ownership;
mod pulls;
mod references;
mod steps;
pub mod sync;
pub mod workspace;

pub use attribution::valid_login;
pub use ctx::{Config, Orchestrator, RunResult, MARKER};
pub use ownership::{Instance, Owner};
pub use steps::sha256_hex;

/// Apply every migration in `migrations/` to the database.
pub async fn migrate(pool: &sqlx::PgPool) -> Result<(), sqlx::migrate::MigrateError> {
    sqlx::migrate!("../../migrations").run(pool).await
}

/// Start of the reason of a ticket whose developer found nothing to change (the console offers to close it).
pub const NOTHING_TO_CHANGE: &str = "Nothing to change";

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
    #[error("git: {0}")]
    Git(String),
    /// The ticket was not in the state this step expects (someone else moved it).
    #[error("ticket {ticket_id} is {found:?}, step needed {wanted}")]
    WrongState {
        ticket_id: i64,
        found: tasklane_core::State,
        wanted: &'static str,
    },
    /// A step reached a dead end that retrying will not fix; the ticket goes to `failed`.
    #[error("{0}")]
    Fatal(String),
}
