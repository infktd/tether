//! Admin CLI commands and `doctor` (F7, N3).
//!
//! Run inside the app container, e.g. `docker compose exec app tether users`.
//! Nothing here is needed to operate an instance; it's for inspection and
//! repairs. Every change is written to the audit log as the `cli` actor.
//!
//! Output goes to a `Write` so tests can capture it.

mod commands;
pub mod doctor;

pub use commands::{Command, JobsCommand, StateArg, StatesCommand, UsersCommand, run};
