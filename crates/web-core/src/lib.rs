//! Tether's web core: application state, sessions and auth, the domain
//! modules, jobs, and the page infrastructure every page crate builds on.
//! The router (`tether-web`) puts the pages together.

// dev-upload installs apps from an uploaded .zip: for app developers only.
// Release builds install from GitHub and ship the first-party apps.
#[cfg(all(feature = "dev-upload", not(debug_assertions)))]
compile_error!("the dev-upload feature must never be enabled in release builds");
// plugin-http-test sends every plugin HTTP request to a local stand-in
// (tests only).
#[cfg(all(feature = "plugin-http-test", not(debug_assertions)))]
compile_error!("the plugin-http-test feature must never be enabled in release builds");

pub mod admin;
pub mod admin_nav;
pub mod auth;
pub mod autogroups;
pub mod backups;
pub mod blacklist;
pub mod bundled;
pub mod compliance;
pub mod csrf;
pub mod discord;
pub mod discord_sync;
pub mod error;
pub mod groups;
pub mod maintenance;
pub mod menu;
pub mod notifications;
pub mod ownership;
pub mod pages;
pub mod personal_tokens;
pub mod pings;
pub mod plugin_consent;
pub mod plugin_downloads;
pub mod plugin_github;
pub mod plugin_http;
pub mod plugin_jobs;
pub mod plugin_notify;
pub mod plugin_review;
pub mod plugin_services;
pub mod plugin_shared;
pub mod plugins;
pub mod ratelimit;
pub mod setup;
pub mod site_name;
pub mod smart_groups;
pub mod state;
pub mod state_admin;
pub mod states;
pub mod static_data;
pub mod structure_names;
pub mod sudo;
pub mod sync;
pub mod theme;
pub mod tokens;
pub mod updates;
pub mod upgrader;
pub mod whats_new;

pub use state::{AppState, Limits, Site, StripStatus};
