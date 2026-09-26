//! ESI access for the host: wraps eve-esi-client.

pub mod budget;
pub mod cache;
mod client;
pub mod jwt;
pub mod names;
pub mod plugin;
pub mod sso;
pub mod vault;

pub use client::{
    CharacterAffiliation, Entity, Esi, EsiError, NamedEntity, Priority, ResolvedNames,
};

/// ESI's address, as eve-esi-client publishes it.
pub const ESI_BASE_URL: &str = eve_esi_client::BASE_URL;
