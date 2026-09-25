//! Domain types and rules: accounts, characters, states, groups, permissions.

pub mod crypto;
pub mod nickname;
pub mod permissions;
pub mod scram;
mod secret;
pub mod states;
mod token;

pub use secret::Secret;
pub use token::{hash_token, new_token};
