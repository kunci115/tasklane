//! Domain rules shared by every tasklane service.
//!
//! Nothing here does I/O. The state machine and the risk-level rule are
//! pure functions so they can be tested exhaustively.

pub mod floor;
pub mod level;
pub mod state;

pub use floor::floor_from_text;
pub use level::Level;
pub use state::{transition, Actor, State, TransitionError};
