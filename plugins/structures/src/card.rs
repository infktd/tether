//! The Discord card around a message, as notification bots post them:
//! the owning corporation above, the headline as its title, the message
//! with a countdown beside each EVE time, the structure's render, and a
//! bar coloured by how urgent it is.

use chrono::{DateTime, NaiveDateTime, Utc};
use serde::{Deserialize, Serialize};
use tether_plugin_sdk::discord::{Embed, Image};

use crate::notification::{self, Category, Severity};

/// What a card shows besides the message, kept with it in the outbox
/// (`outbox.card`): names as they were when it was queued.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Card {
    /// The notification type (or Structures' own, such as
    /// `StructureRefueledExtra`).
    pub kind: String,
    /// When it happened (RFC 3339).
    pub at: String,
    pub corporation_id: i64,
    pub corporation: Option<String>,
    pub structure: Option<String>,
    /// The structure's type: its render is the thumbnail.
    pub type_id: Option<i64>,
    pub system: Option<String>,
    pub moon: Option<String>,
}

const RED: u32 = 0xe7_4c3c;
const ORANGE: u32 = 0xf3_9c12;
const GREEN: u32 = 0x2e_cc71;
const BLUE: u32 = 0x34_98db;

/// The bar's colour: danger red, warning orange, moons green, the rest
/// blue.
fn color(kind: &str) -> u32 {
    match notification::severity(kind) {
        Severity::Danger => RED,
        Severity::Warning => ORANGE,
        Severity::Info if notification::category(kind) == Some(Category::Moon) => GREEN,
        Severity::Info => BLUE,
    }
}

/// The notification's name for a manager ("Moon mining extraction
/// started"), or its type.
fn label(kind: &str) -> &str {
    notification::TYPES
        .iter()
        .find(|(k, _, _, _)| *k == kind)
        .map_or(kind, |(_, label, _, _)| label)
}

/// Each "YYYY-MM-DD HH:MM EVE" followed by a countdown Discord shows in
/// every reader's clock: "2026-11-02 04:00 EVE · <t:1793592000:R>".
pub fn countdowns(text: &str) -> String {
    const STAMP: usize = "2026-11-02 04:00".len();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < text.len() {
        let stamp = text
            .get(i..i + STAMP)
            .filter(|_| text.get(i + STAMP..).is_some_and(|r| r.starts_with(" EVE")))
            .and_then(|s| NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M").ok());
        if let Some(stamp) = stamp {
            let end = i + STAMP + " EVE".len();
            out.push_str(&text[i..end]);
            out.push_str(&format!(" · <t:{}:R>", stamp.and_utc().timestamp()));
            i = end;
            continue;
        }
        let Some(c) = text[i..].chars().next() else {
            break;
        };
        out.push(c);
        i += c.len_utf8();
    }
    out
}

/// The card for a queued message (as `notification::message` words it:
/// "Headline: the rest").
pub fn embed(message: &str, card: &Card) -> Embed {
    let (title, rest) = match message.split_once(": ") {
        Some((head, rest)) if head.chars().count() <= 60 => (head.to_owned(), rest.to_owned()),
        _ => (label(&card.kind).to_owned(), message.to_owned()),
    };
    let mut rest = rest;
    if let Some(first) = rest.get(..1) {
        rest = first.to_uppercase() + rest.get(1..).unwrap_or_default();
    }
    let mut embed = Embed::new(clip(&title, 256))
        .description(clip(&countdowns(&rest), 2000))
        .color(color(&card.kind))
        .footer(clip(&format!("Structures · {}", label(&card.kind)), 256));
    if let Some(corporation) = card.corporation.as_deref().filter(|c| !c.is_empty()) {
        let logo = (card.corporation_id > 0).then_some(Image::Corporation(card.corporation_id));
        embed = embed.author(clip(corporation, 256), logo);
    }
    if let Some(type_id) = card.type_id.filter(|t| *t > 0) {
        embed = embed.thumbnail(Image::TypeRender(type_id));
    }
    for (name, value) in [
        ("Structure", &card.structure),
        ("System", &card.system),
        ("Moon", &card.moon),
    ] {
        if let Some(value) = value.as_deref().filter(|v| !v.trim().is_empty()) {
            embed = embed.field(name, clip(&notification::escape(value), 1024));
        }
    }
    if DateTime::parse_from_rfc3339(&card.at).is_ok() {
        embed = embed.timestamp(card.at.clone());
    }
    embed
}

fn clip(text: &str, max: usize) -> String {
    notification::clip(text, max)
}

/// When it happened, for [`Card::at`].
pub fn at(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tether_plugin_sdk::discord::EmbedField;

    #[test]
    fn eve_times_get_a_countdown() {
        assert_eq!(
            countdowns("arrives 2026-11-02 04:00 EVE and fractures 2026-11-02 07:00 EVE."),
            "arrives 2026-11-02 04:00 EVE · <t:1793592000:R> and fractures \
             2026-11-02 07:00 EVE · <t:1793602800:R>."
        );
        // Not a time, or not EVE's, or cut short: as it was.
        for text in [
            "2026-13-02 04:00 EVE",
            "2026-11-02 04:00 UTC",
            "2026-11-02 04:0",
            "Å 2026",
        ] {
            assert_eq!(countdowns(text), text);
        }
    }

    #[test]
    fn a_moon_card_reads_like_a_notification_bot() {
        let card = Card {
            kind: "MoonminingExtractionStarted".into(),
            at: "2026-11-01T14:19:00Z".into(),
            corporation_id: 98000001,
            corporation: Some("Acme Holdings".into()),
            structure: Some("Jita - The_Drill".into()),
            type_id: Some(35835),
            system: Some("Jita".into()),
            moon: Some("Jita IV - Moon 4".into()),
        };
        let embed = embed(
            "Extraction started: Jita - The\\_Drill (Athanor) in Jita, Jita IV - Moon 4. \
             The chunk arrives 2026-11-02 04:00 EVE.",
            &card,
        );
        assert_eq!(embed.title, "Extraction started");
        assert_eq!(
            embed.description.as_deref(),
            Some(
                "Jita - The\\_Drill (Athanor) in Jita, Jita IV - Moon 4. The chunk arrives \
                 2026-11-02 04:00 EVE · <t:1793592000:R>."
            )
        );
        assert_eq!(embed.color, Some(GREEN));
        let author = embed.author.unwrap();
        assert_eq!(author.name, "Acme Holdings");
        assert!(matches!(author.icon, Some(Image::Corporation(98000001))));
        assert!(matches!(embed.thumbnail, Some(Image::TypeRender(35835))));
        let fields: Vec<(&str, &str)> = embed
            .fields
            .iter()
            .map(|EmbedField { name, value, .. }| (name.as_str(), value.as_str()))
            .collect();
        assert_eq!(
            fields,
            [
                ("Structure", "Jita - The\\_Drill"),
                ("System", "Jita"),
                ("Moon", "Jita IV - Moon 4"),
            ]
        );
        assert_eq!(
            embed.footer.as_deref(),
            Some("Structures · Moon extraction started")
        );
        assert_eq!(embed.timestamp.as_deref(), Some("2026-11-01T14:19:00Z"));
    }

    #[test]
    fn attacks_are_red_and_unknowns_leave_parts_out() {
        let card = Card {
            kind: "StructureUnderAttack".into(),
            at: "not a time".into(),
            ..Card::default()
        };
        let embed = embed("Under attack: A structure.", &card);
        assert_eq!(embed.color, Some(RED));
        assert!(embed.author.is_none() && embed.thumbnail.is_none());
        assert!(embed.fields.is_empty() && embed.timestamp.is_none());
    }
}
