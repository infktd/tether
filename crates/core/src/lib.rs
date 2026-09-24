//! Domain types and rules: accounts, characters, tiers, groups, permissions.

mod secret;
mod token;

pub use secret::Secret;
pub use token::{hash_token, new_token};
