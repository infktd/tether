//! Where each owner's notifications go (aa-structures' webhooks per
//! owner). Plugins can't reach Discord webhooks; the host's assigned
//! channels stand in for them. The settings' channels are the defaults;
//! an owner can route a kind to its own channel, or not send it, and
//! mention Members on attacks or not.

use tether_plugin_sdk::storage::{self, Value as Db};

use crate::notification::Category;
use crate::{Settings, int, text};

pub struct Routes {
    defaults: [Option<String>; 4],
    default_mention: bool,
    /// (corporation, category, channel or none).
    owners: Vec<(i64, String, Option<String>)>,
    /// (corporation, "on" or "off").
    mentions: Vec<(i64, String)>,
}

fn index(category: Category) -> usize {
    match category {
        Category::Attack => 0,
        Category::Fuel => 1,
        Category::State => 2,
        Category::Moon => 3,
    }
}

impl Routes {
    pub fn load(settings: &Settings) -> Result<Self, storage::Error> {
        let owners = storage::query(
            "SELECT corporation_id, category, channel FROM owner_channels",
            &[],
        )?;
        let mentions = storage::query(
            "SELECT corporation_id, mention FROM owner_settings WHERE mention <> 'default'",
            &[],
        )?;
        Ok(Self {
            defaults: Category::ALL.map(|c| settings.channel(c).map(str::to_owned)),
            default_mention: settings.mention,
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
            mentions: mentions
                .rows
                .iter()
                .map(|r| (int(r, 0), text(r, 1)))
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

    /// Whether an owner's attacks mention Members.
    pub fn mention(&self, corporation: i64) -> bool {
        match self.mentions.iter().find(|(c, _)| *c == corporation) {
            Some((_, m)) => m == "on",
            None => self.default_mention,
        }
    }
}
