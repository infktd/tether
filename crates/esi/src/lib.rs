//! ESI access for the host: wraps eve-esi-client.

mod client;
pub mod sso;

pub use client::{CharacterAffiliation, Esi, EsiError};
