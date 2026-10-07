//! Administration's pages: states, groups' admin, users, the blacklist, Discord,
//! pings, the menu, compliance, the audit log, System, the first-run setup
//! and the Apps admin.

// The page infrastructure (PageError, Shell, render, stay and the rest)
// is the core's: `super::render` and the like read as they always did.
pub use tether_web_core::pages::*;

pub mod admin;
pub mod audit;
pub mod autogroups;
pub mod blacklist;
pub mod compliance;
pub mod discord;
pub mod menu;
pub mod permissions_audit;
pub mod pings;
pub mod plugins;
pub mod setup;
pub mod states;
pub mod system;
pub mod users;
