#![allow(clippy::unwrap_used, clippy::expect_used)] // test code

//! Tether's web tests, one binary: linked once, run in parallel.

mod common;

mod access_tokens;
mod admin_pages;
mod auth;
mod autogroups;
mod blacklist;
mod compliance;
mod dashboard;
mod dev_login;
mod discord;
mod fleet_activity_tracking;
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
mod plugin_keys;
mod plugin_pages;
mod plugins;
mod setup;
mod states;
mod storage;
mod structure_timers;
mod sync;
mod system;
mod theme;
mod tokens;
mod transfers;
mod users;
mod vault;
