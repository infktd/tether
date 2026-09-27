//! Where each owner's notifications go, which types are sent, and whom
//! they ping (aa-structures' webhooks per owner, their notification-type
//! filters and default pings). Plugins can't reach Discord webhooks; the
//! host's assigned channels stand in for them, and the Discord roles of
//! states for @everyone and @here. The settings' choices are the
//! defaults; an owner can route a kind to its own channel, or not send it,
//! pick its own types, and turn its pings on or off.

use tether_plugin_sdk::storage::{self, Value as Db};

use crate::notification::{Category, Severity};
use crate::{Settings, int, text};

pub struct Routes {
    defaults: [Option<String>; 4],
    default_pings: bool,
    danger_ping: Option<String>,
    warning_ping: Option<String>,
    default_types: Option<Vec<String>>,
    /// (corporation, category, channel or none).
    owners: Vec<(i64, String, Option<String>)>,
    /// (corporation, "on" or "off").
    pings: Vec<(i64, String)>,
    /// (corporation, its own types).
    types: Vec<(i64, Vec<String>)>,
}

fn index(category: Category) -> usize {
    match category {
        Category::Attack => 0,
        Category::Fuel => 1,
        Category::State => 2,
        Category::Moon => 3,
    }
}

/// A stored list of types (`array_to_string(..., ',')`), or none.
pub fn type_list(value: Option<&Db>) -> Option<Vec<String>> {
    value.and_then(Db::as_text).map(|t| {
        t.split(',')
            .filter(|k| !k.is_empty())
            .map(str::to_owned)
            .collect()
    })
}

impl Routes {
    pub fn load(settings: &Settings) -> Result<Self, storage::Error> {
        let owners = storage::query(
            "SELECT corporation_id, category, channel FROM owner_channels",
            &[],
        )?;
        let own = storage::query(
            "SELECT corporation_id, mention, array_to_string(notification_types, ',') \
             FROM owner_settings",
            &[],
        )?;
        Ok(Self {
            defaults: Category::ALL.map(|c| settings.channel(c).map(str::to_owned)),
            default_pings: settings.default_pings,
            danger_ping: settings.danger_ping.clone(),
            warning_ping: settings.warning_ping.clone(),
            default_types: settings.notification_types.clone(),
            owners: owners
                .rows
                .iter()
                .map(|r| {
                    (
                        int(r, 0),
                        text(r, 1),
                        r.get(2)
                            .and_then(Db::as_text)
                            .filter(|c| !c.is_empty())
                            .map(str::to_owned),
                    )
                })
                .collect(),
            pings: own
                .rows
                .iter()
                .filter(|r| text(r, 1) != "default")
                .map(|r| (int(r, 0), text(r, 1)))
                .collect(),
            types: own
                .rows
                .iter()
                .filter_map(|r| Some((int(r, 0), type_list(r.get(2))?)))
                .collect(),
        })
    }

    /// The channel an owner's notifications of a kind go to, if any.
    pub fn channel(&self, corporation: i64, category: Category) -> Option<&str> {
        match self
            .owners
            .iter()
            .find(|(c, k, _)| *c == corporation && k == category.name())
        {
            Some((_, _, channel)) => channel.as_deref(),
            None => self.defaults[index(category)].as_deref(),
        }
    }

    /// Whether an owner sends notifications of this type (its own types,
    /// else the default's, else every type).
    pub fn sends(&self, corporation: i64, kind: &str) -> bool {
        let list = self
            .types
            .iter()
            .find(|(c, _)| *c == corporation)
            .map(|(_, t)| t)
            .or(self.default_types.as_ref());
        list.is_none_or(|types| types.iter().any(|t| t == kind))
    }

    /// Whom a message of this severity pings for an owner: the state whose
    /// Discord role stands in for @everyone (danger) or @here (warning),
    /// when the owner's pings are on.
    pub fn ping(&self, corporation: i64, severity: Severity) -> Option<&str> {
        let on = match self.pings.iter().find(|(c, _)| *c == corporation) {
            Some((_, m)) => m == "on",
            None => self.default_pings,
        };
        if !on {
            return None;
        }
        match severity {
            Severity::Danger => self.danger_ping.as_deref(),
            Severity::Warning => self.warning_ping.as_deref(),
            Severity::Info => None,
        }
    }
}
