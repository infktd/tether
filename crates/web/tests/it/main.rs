#![allow(clippy::unwrap_used, clippy::expect_used)] // test code

//! Tether's web tests, one binary: linked once, run in parallel.

mod common;

mod access_tokens;
mod admin_pages;
mod auth;
mod autogroups;
mod blacklist;
mod bundled;
mod compliance;
mod dashboard;
mod dev_login;
mod discord;
mod fleet_activity_tracking;
mod github;
mod groups;
mod hr_applications;
mod jobs;
mod maintenance;
mod member_audit;
mod menu;
mod moon_mining;
mod notifications;
mod openapi;
mod ownership;
mod pages;
mod permissions_audit;
mod plugin_esi;
mod plugin_http;
mod plugin_keys;
mod plugin_pages;
mod plugins;
mod setup;
mod ship_replacement;
mod signed_out;
mod smart_groups;
mod states;
mod storage;
mod structure_timers;
mod structures;
mod sudo;
mod sync;
mod system;
mod theme;
mod tokens;
mod transfers;
mod upgrades;
mod users;
mod vault;
