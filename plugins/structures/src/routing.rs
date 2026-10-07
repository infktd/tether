//! Where each owner's notifications go, which types are sent, and whom
//! they ping (aa-structures' webhooks per owner, their notification-type
//! filters, default pings and ping groups). Plugins can't reach Discord
//! webhooks; the host's assigned channels stand in for them, and the
//! Discord roles of states for @everyone and @here. The settings' choices
//! are the defaults; an owner can route a kind to its own channel, or not
//! send it, pick its own types, and turn its pings on or off. Ping groups
//! are the owner's and the channel's together, on every message, as
//! aa-structures adds an owner's and a webhook's; by the group's id, so
//! they follow the group as aa-structures' do.

use tether_plugin_sdk::discord;
use tether_plugin_sdk::storage::{self, Value as Db};

use crate::notification::{Category, Severity};
use crate::{Settings, int, text};

pub struct Routes {
    defaults: [Option<String>; 7],
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
    /// (corporation, its ping groups).
    owner_groups: Vec<(i64, Vec<i64>)>,
    /// (channel, its ping groups).
    channel_groups: Vec<(String, Vec<i64>)>,
    /// The groups that have a Discord role now: kept first.
    pingable: Vec<i64>,
}

/// Groups a message pings at most: the host takes 10 pings, one of them
/// a state's.
pub const MAX_PING_GROUPS: usize = 9;

/// A stored list of group ids (`array_to_string(..., ',')`).
pub fn group_list(value: Option<&Db>) -> Vec<i64> {
    value
        .and_then(Db::as_text)
        .map(|t| t.split(',').filter_map(|g| g.trim().parse().ok()).collect())
        .unwrap_or_default()
}

/// Group ids as stored (`string_to_array($n, ',')::bigint[]`), or none.
pub fn group_ids(groups: &[i64]) -> Db {
    (!groups.is_empty())
        .then(|| {
            groups
                .iter()
                .map(i64::to_string)
                .collect::<Vec<_>>()
                .join(",")
        })
        .into()
}

fn index(category: Category) -> usize {
    match category {
        Category::Attack => 0,
        Category::Fuel => 1,
        Category::State => 2,
        Category::Moon => 3,
        Category::Sov => 4,
        Category::War => 5,
        Category::Corp => 6,
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
            "SELECT corporation_id, mention, array_to_string(notification_types, ','), \
                    array_to_string(ping_groups, ',') \
             FROM owner_settings",
            &[],
        )?;
        let channel_groups = storage::query(
            "SELECT channel, array_to_string(groups, ',') FROM channel_ping_groups",
            &[],
        )?;
        Ok(Self {
            pingable: discord::ping_groups().into_iter().map(|g| g.id).collect(),
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
            owner_groups: own
                .rows
                .iter()
                .map(|r| (int(r, 0), group_list(r.get(3))))
                .filter(|(_, g)| !g.is_empty())
                .collect(),
            channel_groups: channel_groups
                .rows
                .iter()
                .map(|r| (text(r, 0), group_list(r.get(1))))
                .collect(),
        })
    }

    /// The groups a message from an owner to a channel pings
    /// (aa-structures' ping groups): the owner's and the channel's, each
    /// once, whatever its pings are set to; at most [`MAX_PING_GROUPS`].
    /// A group deleted or without a Discord role now goes last; the host
    /// skips it.
    pub fn groups(&self, corporation: i64, channel: &str) -> Vec<i64> {
        let owner = self
            .owner_groups
            .iter()
            .filter(|(c, _)| *c == corporation)
            .flat_map(|(_, g)| g);
        let channel = self
            .channel_groups
            .iter()
            .filter(|(c, _)| c == channel)
            .flat_map(|(_, g)| g);
        let mut groups: Vec<i64> = Vec::new();
        for group in owner.chain(channel) {
            if !groups.contains(group) {
                groups.push(*group);
            }
        }
        // Those with a role first, so a dead one never crowds out a live
        // one. The dead are kept for the host to leave out and log, and
        // an empty list (the host couldn't say) drops none.
        groups.sort_by_key(|g| !self.pingable.contains(g));
        groups.truncate(MAX_PING_GROUPS);
        groups
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
