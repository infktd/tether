#![allow(clippy::unwrap_used, clippy::expect_used)] // test code

//! Tether's web tests, one binary: linked once, run in parallel.

mod common;

mod admin_pages;
mod auth;
mod autogroups;
mod compliance;
mod dev_login;
mod discord;
mod groups;
mod jobs;
mod maintenance;
mod member_audit;
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
mod sync;
mod system;
mod tokens;
mod transfers;
mod vault;
