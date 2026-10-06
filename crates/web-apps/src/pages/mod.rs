//! Apps' pages: the host's renderer for plugin page descriptions, with its
//! instruments and timelines.

// The page infrastructure (PageError, Shell, render, stay and the rest)
// is the core's: `super::render` and the like read as they always did.
pub use tether_web_core::pages::*;

pub mod plugin_lists;
pub mod plugin_pages;
pub mod plugin_visuals;
