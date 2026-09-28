//! Notices as Discord cards: who issued the contract above, the news as
//! the title, the route below, and its terms as fields. Kept as JSON in
//! the outbox (`outbox.card`) until sent.

use serde_json::{Value, json};
use tether_plugin_sdk::discord::{Embed, Image};

pub const RED: u32 = 0xe7_4c3c;
pub const ORANGE: u32 = 0xf3_9c12;
pub const GREEN: u32 = 0x2e_cc71;
pub const BLUE: u32 = 0x34_98db;

/// A card to queue: `fields` are (name, value, inline).
pub struct Card<'a> {
    pub title: &'a str,
    pub description: String,
    pub color: u32,
    /// The issuer: a name, and the character whose portrait goes beside it.
    pub author: (String, i64),
    pub fields: Vec<(&'a str, String, bool)>,
    /// RFC 3339.
    pub timestamp: Option<String>,
}

impl Card<'_> {
    pub fn json(&self) -> Value {
        json!({
            "title": clip(self.title, 256),
            "description": clip(&self.description, 2000),
            "color": self.color,
            "author": { "name": clip(&self.author.0, 256), "character": self.author.1 },
            "fields": self.fields.iter()
                .filter(|(_, value, _)| !value.trim().is_empty())
                .take(10)
                .map(|(name, value, inline)| json!([name, clip(value, 1024), inline]))
                .collect::<Vec<_>>(),
            "timestamp": self.timestamp,
        })
    }
}

/// The card a queued notice posts as; `None` for a notice queued without
/// one.
pub fn embed(card: &Value) -> Option<Embed> {
    let mut embed = Embed::new(card["title"].as_str()?).footer("Freight");
    if let Some(description) = card["description"].as_str().filter(|d| !d.is_empty()) {
        embed = embed.description(description);
    }
    if let Some(color) = card["color"].as_u64().and_then(|c| u32::try_from(c).ok()) {
        embed = embed.color(color);
    }
    if let Some(name) = card["author"]["name"].as_str().filter(|n| !n.is_empty()) {
        let portrait = card["author"]["character"]
            .as_i64()
            .filter(|id| *id > 0)
            .map(Image::Character);
        embed = embed.author(name, portrait);
    }
    for field in card["fields"].as_array().into_iter().flatten() {
        let (Some(name), Some(value)) = (field[0].as_str(), field[1].as_str()) else {
            continue;
        };
        embed = if field[2].as_bool().unwrap_or(true) {
            embed.field(name, value)
        } else {
            embed.wide_field(name, value)
        };
    }
    if let Some(at) = card["timestamp"].as_str() {
        embed = embed.timestamp(at);
    }
    Some(embed)
}

fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(max - 1).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_queued_card_comes_back_as_it_was() {
        let card = Card {
            title: "New courier contract",
            description: "Jita → Amarr".into(),
            color: GREEN,
            author: ("Pilot A (Acme)".into(), 90000001),
            fields: vec![
                ("Reward", "1,000 ISK".into(), true),
                ("Note", String::new(), false),
                ("Contract check", "OK".into(), false),
            ],
            timestamp: Some("2026-11-02T04:00:00Z".into()),
        };
        let embed = embed(&card.json()).unwrap();
        assert_eq!(embed.title, "New courier contract");
        assert_eq!(embed.description.as_deref(), Some("Jita → Amarr"));
        assert_eq!(embed.color, Some(GREEN));
        let author = embed.author.unwrap();
        assert_eq!(author.name, "Pilot A (Acme)");
        assert!(matches!(author.icon, Some(Image::Character(90000001))));
        // Empty fields are left out.
        let fields: Vec<(&str, bool)> = embed
            .fields
            .iter()
            .map(|f| (f.name.as_str(), f.inline))
            .collect();
        assert_eq!(fields, [("Reward", true), ("Contract check", false)]);
        assert_eq!(embed.footer.as_deref(), Some("Freight"));
        assert_eq!(embed.timestamp.as_deref(), Some("2026-11-02T04:00:00Z"));
        assert!(super::embed(&Value::Null).is_none());
    }
}
