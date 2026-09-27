//! The pilot's pages: Dashboard and login, groups, tokens, access tokens,
//! notifications, Secure Groups and Corporation Stats.

// The core, at this crate's root: `crate::auth` and the like read as
// they did when these pages lived in one crate.
pub use tether_web_core::*;

pub mod pages;
