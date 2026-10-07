//! The pilot's pages: Dashboard and login, groups, tokens, access tokens,
//! notifications, Secure Groups and Corporation Stats.

// The page infrastructure (PageError, Shell, render, stay and the rest)
// is the core's: `super::render` and the like read as they always did.
pub use tether_web_core::pages::*;

pub mod access_tokens;
pub mod account;
pub mod corpstats;
pub mod groups;
pub mod notifications;
pub mod securegroups;
pub mod tokens;
pub mod whats_new;
