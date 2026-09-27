//! Administration's pages: states, groups' admin, users, the blacklist, Discord,
//! pings, the menu, compliance, the audit log, System, the first-run setup
//! and the Apps admin.

// The core, at this crate's root: `crate::auth` and the like read as
// they did when these pages lived in one crate.
// dev-upload installs apps from an uploaded .zip: for app developers only.
#[cfg(all(feature = "dev-upload", not(debug_assertions)))]
compile_error!("the dev-upload feature must never be enabled in release builds");

pub use tether_web_core::*;

pub mod pages;
