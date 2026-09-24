//! Build Tether plugins.
//!
//! A plugin is a Rust library compiled to a WebAssembly component
//! (`cargo build --target wasm32-wasip2 --release`). It implements
//! [`Plugin`] and hands its type to [`export!`]:
//!
//! ```ignore
//! use tether_plugin_sdk::{Page, PageError, Plugin, Request, log};
//!
//! struct Hello;
//!
//! impl Plugin for Hello {
//!     fn render(request: Request) -> Result<Page, PageError> {
//!         log::info("rendering");
//!         match request.path.as_str() {
//!             "" => Ok(Page::new("Hello").text("o7")),
//!             _ => Err(PageError::NotFound),
//!         }
//!     }
//! }
//!
//! tether_plugin_sdk::export!(Hello);
//! ```
//!
//! Pages are data: the host renders them with its own templates, so every
//! plugin looks native. See `AGENTS.md` next to this crate for the whole
//! contract, the limits a plugin runs under, and patterns.

#![deny(unsafe_code)]

/// The generated bindings (their ABI glue needs `unsafe`). Plugins use the
/// re-exports below.
#[doc(hidden)]
#[allow(unsafe_code)]
pub mod bindings {
    wit_bindgen::generate!({
        world: "plugin",
        path: "../../wit",
        pub_export_macro: true,
        default_bindings_module: "tether_plugin_sdk::bindings",
    });
}

/// What a plugin implements.
pub use bindings::Guest as Plugin;
/// Exports a type implementing [`Plugin`] as the plugin:
/// `tether_plugin_sdk::export!(MyPlugin);`
pub use bindings::export;
pub use bindings::tether::plugin::page::{
    Badge, Card, Column, Link, Section, Stat, Tab, Table, Tone, Value,
};
pub use bindings::{Page, PageError, Request};

/// Logs into the plugin's log in the admin panel. Keep messages short:
/// the host keeps the first 100 per call, 1 KiB each.
pub mod log {
    use crate::bindings::tether::plugin::log::{Level, write};

    pub fn debug(message: impl AsRef<str>) {
        write(Level::Debug, message.as_ref());
    }

    pub fn info(message: impl AsRef<str>) {
        write(Level::Info, message.as_ref());
    }

    pub fn warn(message: impl AsRef<str>) {
        write(Level::Warn, message.as_ref());
    }

    pub fn error(message: impl AsRef<str>) {
        write(Level::Error, message.as_ref());
    }
}

impl Page {
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            description: None,
            sections: Vec::new(),
            tabs: Vec::new(),
        }
    }

    /// The one-line description under the title.
    pub fn description(mut self, text: impl Into<String>) -> Self {
        self.description = Some(text.into());
        self
    }

    pub fn section(mut self, section: Section) -> Self {
        self.sections.push(section);
        self
    }

    /// A row of stats (at most 8).
    pub fn stats(self, stats: Vec<Stat>) -> Self {
        self.section(Section::Stats(stats))
    }

    pub fn table(self, table: Table) -> Self {
        self.section(Section::Table(table))
    }

    pub fn card(self, card: Card) -> Self {
        self.section(Section::Card(card))
    }

    /// A paragraph of plain text.
    pub fn text(self, text: impl Into<String>) -> Self {
        self.section(Section::Text(text.into()))
    }

    pub fn tab(mut self, label: impl Into<String>, sections: Vec<Section>) -> Self {
        self.tabs.push(Tab {
            label: label.into(),
            sections,
        });
        self
    }
}

impl Stat {
    pub fn new(label: impl Into<String>, value: impl Into<Value>) -> Self {
        Self {
            label: label.into(),
            value: value.into(),
            caption: None,
        }
    }

    pub fn caption(mut self, caption: impl Into<String>) -> Self {
        self.caption = Some(caption.into());
        self
    }
}

impl Column {
    pub fn text(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            numeric: false,
        }
    }

    /// Right-aligned, for numbers, ISK and times.
    pub fn numeric(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            numeric: true,
        }
    }
}

impl Table {
    pub fn new(columns: Vec<Column>) -> Self {
        Self {
            title: None,
            columns,
            rows: Vec::new(),
            empty: None,
        }
    }

    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// One row: as many values as there are columns.
    pub fn row(mut self, values: Vec<Value>) -> Self {
        self.rows.push(values);
        self
    }

    /// What to show when there are no rows.
    pub fn empty(mut self, text: impl Into<String>) -> Self {
        self.empty = Some(text.into());
        self
    }
}

impl Card {
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            description: None,
            fields: Vec::new(),
        }
    }

    pub fn description(mut self, text: impl Into<String>) -> Self {
        self.description = Some(text.into());
        self
    }

    pub fn field(mut self, label: impl Into<String>, value: impl Into<Value>) -> Self {
        self.fields.push((label.into(), value.into()));
        self
    }
}

impl From<&str> for Value {
    fn from(text: &str) -> Self {
        Value::Text(text.to_owned())
    }
}

impl From<String> for Value {
    fn from(text: String) -> Self {
        Value::Text(text)
    }
}

impl From<i64> for Value {
    fn from(n: i64) -> Self {
        Value::Number(n)
    }
}

impl From<Badge> for Value {
    fn from(badge: Badge) -> Self {
        Value::Badge(badge)
    }
}

impl From<Link> for Value {
    fn from(link: Link) -> Self {
        Value::Link(link)
    }
}

/// An amount of ISK.
pub fn isk(amount: f64) -> Value {
    Value::Isk(amount)
}

/// An instant in EVE time (UTC), RFC 3339, e.g. `2026-09-24T18:00:00Z`.
pub fn time(rfc3339: impl Into<String>) -> Value {
    Value::Time(rfc3339.into())
}

pub fn badge(label: impl Into<String>, tone: Tone) -> Badge {
    Badge {
        label: label.into(),
        tone,
    }
}

/// A link to another page of this plugin, relative to its pages.
pub fn link(label: impl Into<String>, path: impl Into<String>) -> Link {
    Link {
        label: label.into(),
        path: path.into(),
    }
}
