//! A contract's Discord card, as Bastion posted them: the corporation
//! above, what kind of contract and where as the title, who did what and
//! the price check below, then Location, Details, Expires and the items.
//! Kept as JSON in the outbox (`outbox.card`) until sent.

use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use tether_plugin_sdk::discord::{Embed, Image};

use crate::janice;

pub const ORANGE: u32 = 0xf3_9c12;
pub const GREEN: u32 = 0x2e_cc71;
pub const RED: u32 = 0xe7_4c3c;
pub const GREY: u32 = 0x95_a5a6;

/// Included items listed before "…and N more".
const ITEMS_SHOWN: usize = 10;

/// What a notice is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    New,
    Completed,
    Ended,
}

impl Event {
    pub fn key(self) -> &'static str {
        match self {
            Event::New => "new",
            Event::Completed => "completed",
            Event::Ended => "ended",
        }
    }

    /// Bastion's name for it, in the footer.
    fn footer(self) -> &'static str {
        match self {
            Event::New => "contract_assigned",
            Event::Completed => "contract_delivered",
            Event::Ended => "contract_ended",
        }
    }
}

/// The price check, as far as it went.
#[derive(Debug, Clone, PartialEq)]
pub enum Check {
    /// No appraisal in the description.
    NoLink,
    /// Linked but not read yet, or not readable: why.
    NotChecked(String),
    /// Read: the appraisal's buy total.
    Buy(f64),
    /// Read, but it doesn't vouch for this contract: its buy total, and
    /// why.
    Differs(f64, String),
}

/// A contract, named, ready to be a card. Names are escaped already.
#[derive(Debug, Clone)]
pub struct Notice {
    pub event: Event,
    pub kind: String,
    pub status: String,
    pub corporation: (i64, String),
    /// Its name unescaped, for the author line (which Discord shows as
    /// plain text).
    pub corporation_plain: String,
    pub issuer: String,
    pub acceptor: Option<String>,
    pub location: String,
    pub end_location: Option<String>,
    pub price: f64,
    pub reward: f64,
    pub collateral: f64,
    pub volume: f64,
    pub expires: Option<DateTime<Utc>>,
    pub at: DateTime<Utc>,
    /// (type id, name, quantity, included).
    pub items: Vec<(i64, String, i64, bool)>,
    pub appraisal: Option<String>,
    pub check: Check,
    pub tolerance: f64,
}

pub fn kind_label(kind: &str) -> &'static str {
    match kind {
        "item_exchange" => "Item exchange",
        "courier" => "Courier",
        "auction" => "Auction",
        "loan" => "Loan",
        _ => "Unknown",
    }
}

pub fn status_label(status: &str) -> &'static str {
    match status {
        "outstanding" => "Outstanding",
        "in_progress" => "In progress",
        "finished" | "finished_issuer" | "finished_contractor" => "Finished",
        "cancelled" => "Cancelled",
        "rejected" => "Rejected",
        "failed" => "Failed",
        "deleted" => "Deleted",
        "reversed" => "Reversed",
        "expired" => "Expired",
        _ => "Unknown",
    }
}

/// `1.06B ISK`, `850.2M ISK`, `12,500 ISK`.
pub fn isk(value: f64) -> String {
    let abs = value.abs();
    if abs >= 1e9 {
        format!("{:.2}B ISK", value / 1e9)
    } else if abs >= 1e6 {
        format!("{:.1}M ISK", value / 1e6)
    } else {
        format!("{} ISK", thousands(value.round() as i64))
    }
}

pub fn thousands(n: i64) -> String {
    let digits = n.unsigned_abs().to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    if n < 0 {
        out.insert(0, '-');
    }
    out
}

/// `3,198.3 m³`.
pub fn m3(volume: f64) -> String {
    let tenths = (volume * 10.0).round() as i64;
    format!("{}.{} m³", thousands(tenths / 10), (tenths % 10).abs())
}

/// The price check as one line, and whether it's a problem (a price off
/// the appraisal).
pub fn check_line(n: &Notice) -> (String, bool) {
    let linked = |text: &str| match &n.appraisal {
        Some(code) => format!("[{text}]({})", janice::link(code)),
        None => text.to_owned(),
    };
    if n.kind == "courier" {
        return (
            format!(
                "Reward {} · collateral {}",
                isk(n.reward),
                isk(n.collateral)
            ),
            false,
        );
    }
    let asked = if n.price > 0.0 {
        format!("{} asked", isk(n.price))
    } else {
        "No ISK asked".to_owned()
    };
    match &n.check {
        Check::NoLink => (
            format!("{asked} · no appraisal linked in the description"),
            false,
        ),
        Check::NotChecked(why) => (
            format!("{asked} · {} not checked: {why}", linked("appraisal")),
            false,
        ),
        Check::Differs(buy, why) => (
            format!("{asked} · {} buy {}: ❌ {why}", linked("Janice"), isk(*buy)),
            true,
        ),
        Check::Buy(buy) if n.price <= 0.0 => (
            format!("{asked} · {} buy {}", linked("Janice"), isk(*buy)),
            false,
        ),
        Check::Buy(buy) => {
            let (off, ok) = janice::compare(n.price, *buy, n.tolerance);
            if ok {
                (
                    format!(
                        "{asked} · {} buy {}: ✅ matches",
                        linked("Janice"),
                        isk(*buy)
                    ),
                    false,
                )
            } else {
                (
                    format!(
                        "{asked} · {} buy {}: ❌ {:.1}% {}",
                        linked("Janice"),
                        isk(*buy),
                        off.abs(),
                        if off > 0.0 { "over" } else { "under" }
                    ),
                    true,
                )
            }
        }
    }
}

/// The card's JSON, and the plain text kept with it (the outbox's record).
pub fn build(n: &Notice) -> (String, Value) {
    let kind = kind_label(&n.kind);
    let place = match &n.end_location {
        Some(end) if n.kind == "courier" => format!("{} → {end}", n.location),
        _ => n.location.clone(),
    };
    let title = match n.kind.as_str() {
        "courier" => format!("Courier contract {place}"),
        _ => format!("{kind} contract to {place}"),
    };
    let headline = match n.event {
        Event::New => format!(
            "{} assigned your corporation a contract at {}.",
            n.issuer, n.location
        ),
        Event::Completed => format!(
            "{} completed the contract.",
            n.acceptor.as_deref().unwrap_or(&n.corporation.1)
        ),
        Event::Ended => format!(
            "The contract is {}.",
            status_label(&n.status).to_lowercase()
        ),
    };
    let (check, off) = check_line(n);
    let mut description = vec![headline, check];
    if let (Event::New, Some(expires)) = (n.event, n.expires) {
        description.push(format!(
            "Accept before {} UTC or it lapses.",
            expires.format("%Y-%m-%d %H:%M")
        ));
    }
    let mut details = vec![
        format!("**Type:** {kind}"),
        format!("**Issued by:** {}", n.issuer),
        format!("**Issued to:** {}", n.corporation.1),
    ];
    if let Some(acceptor) = &n.acceptor {
        details.push(format!("**Contractor:** {acceptor}"));
    }
    details.push(format!("**Status:** {}", status_label(&n.status)));
    if n.price > 0.0 {
        details.push(format!("**Price:** {}", isk(n.price)));
    }
    if n.kind == "courier" {
        details.push(format!("**Reward:** {}", isk(n.reward)));
        details.push(format!("**Collateral:** {}", isk(n.collateral)));
    }
    details.push(format!("**Volume:** {}", m3(n.volume)));
    let mut fields = vec![
        json!(["Location", place, false]),
        json!(["Details", details.join("\n"), false]),
    ];
    if let Some(expires) = n.expires {
        let t = expires.timestamp();
        fields.push(json!(["Expires", format!("<t:{t}:F> · <t:{t}:R>"), false]));
    }
    for (label, included) in [("Included items", true), ("Asked for", false)] {
        let list: Vec<&(i64, String, i64, bool)> =
            n.items.iter().filter(|i| i.3 == included).collect();
        if list.is_empty() {
            continue;
        }
        let mut lines: Vec<String> = list
            .iter()
            .take(ITEMS_SHOWN)
            .map(|(_, name, quantity, _)| format!("{name} x{}", thousands(*quantity)))
            .collect();
        if list.len() > ITEMS_SHOWN {
            lines.push(format!("…and {} more", list.len() - ITEMS_SHOWN));
        }
        fields.push(json!([label, lines.join("\n"), false]));
    }
    let color = match n.event {
        _ if off => RED,
        Event::New => ORANGE,
        Event::Completed => GREEN,
        Event::Ended => GREY,
    };
    let thumbnail = n
        .items
        .iter()
        .find(|i| i.3)
        .or(n.items.first())
        .map(|i| i.0);
    let card = json!({
        "title": clip(&title, 256),
        "description": clip(&description.join("\n"), 2000),
        "color": color,
        "author": { "name": clip(&n.corporation_plain, 256), "corporation": n.corporation.0 },
        "thumbnail": thumbnail,
        "fields": fields.iter().map(|f| json!([f[0], clip(f[1].as_str().unwrap_or_default(), 1024), f[2]])).collect::<Vec<_>>(),
        "footer": format!("Contracts · {}", n.event.footer()),
        "timestamp": n.at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    });
    let text = format!("{title}: {}", description.join(" "));
    (clip(&text, 1500), card)
}

/// The card a queued notice posts as.
pub fn embed(card: &Value) -> Option<Embed> {
    let mut embed = Embed::new(card["title"].as_str()?);
    if let Some(footer) = card["footer"].as_str() {
        embed = embed.footer(footer);
    }
    if let Some(description) = card["description"].as_str().filter(|d| !d.is_empty()) {
        embed = embed.description(description);
    }
    if let Some(color) = card["color"].as_u64().and_then(|c| u32::try_from(c).ok()) {
        embed = embed.color(color);
    }
    if let Some(name) = card["author"]["name"].as_str().filter(|n| !n.is_empty()) {
        let logo = card["author"]["corporation"]
            .as_i64()
            .filter(|id| *id > 0)
            .map(Image::Corporation);
        embed = embed.author(name, logo);
    }
    if let Some(type_id) = card["thumbnail"].as_i64().filter(|id| *id > 0) {
        embed = embed.thumbnail(Image::TypeIcon(type_id));
    }
    for field in card["fields"].as_array().into_iter().flatten() {
        let (Some(name), Some(value)) = (field[0].as_str(), field[1].as_str()) else {
            continue;
        };
        if value.trim().is_empty() {
            continue;
        }
        embed = if field[2].as_bool().unwrap_or(false) {
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

pub fn clip(text: &str, max: usize) -> String {
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

    fn notice() -> Notice {
        Notice {
            event: Event::New,
            kind: "item_exchange".into(),
            status: "outstanding".into(),
            corporation: (98000001, "Acme Holdings".into()),
            corporation_plain: "Acme Holdings".into(),
            issuer: "Some Pilot".into(),
            acceptor: None,
            location: "Jita - Union Terminal".into(),
            end_location: None,
            price: 0.0,
            reward: 0.0,
            collateral: 0.0,
            volume: 3198.3,
            expires: DateTime::parse_from_rfc3339("2026-10-27T10:54:00Z")
                .ok()
                .map(|t| t.to_utc()),
            at: Utc::now(),
            items: vec![
                (62516, "Compressed Bitumens".into(), 13803, true),
                (62517, "Compressed Zeolites".into(), 6359, true),
            ],
            appraisal: None,
            check: Check::NoLink,
            tolerance: 1.0,
        }
    }

    #[test]
    fn a_new_contract_reads_like_bastions() {
        let (text, card) = build(&notice());
        assert_eq!(
            card["title"],
            "Item exchange contract to Jita - Union Terminal"
        );
        let description = card["description"].as_str().unwrap();
        assert_eq!(
            description,
            "Some Pilot assigned your corporation a contract at Jita - Union Terminal.\n\
             No ISK asked · no appraisal linked in the description\n\
             Accept before 2026-10-27 10:54 UTC or it lapses."
        );
        assert_eq!(card["color"], ORANGE);
        assert_eq!(card["footer"], "Contracts · contract_assigned");
        let fields = card["fields"].as_array().unwrap();
        assert_eq!(
            fields[0],
            json!(["Location", "Jita - Union Terminal", false])
        );
        assert!(
            fields[1][1]
                .as_str()
                .unwrap()
                .contains("**Volume:** 3,198.3 m³")
        );
        assert_eq!(fields[2][1], "<t:1793098440:F> · <t:1793098440:R>");
        assert_eq!(
            fields[3],
            json!([
                "Included items",
                "Compressed Bitumens x13,803\nCompressed Zeolites x6,359",
                false
            ])
        );
        assert_eq!(card["thumbnail"], 62516);
        assert!(text.starts_with("Item exchange contract to Jita - Union Terminal: Some Pilot"));
        let embed = embed(&card).unwrap();
        assert_eq!(embed.fields.len(), 4);
        assert!(matches!(
            embed.author.unwrap().icon,
            Some(Image::Corporation(98000001))
        ));
    }

    #[test]
    fn the_price_is_checked_against_the_appraisal() {
        let mut n = notice();
        n.price = 1_000_000_000.0;
        n.appraisal = Some("Ab12Cd".into());
        n.check = Check::Buy(1_005_000_000.0);
        let (line, off) = check_line(&n);
        assert!(!off, "{line}");
        assert_eq!(
            line,
            "1.00B ISK asked · [Janice](https://janice.e-351.com/a/Ab12Cd) buy 1.00B ISK: ✅ matches"
        );
        n.check = Check::Buy(1_200_000_000.0);
        let (line, off) = check_line(&n);
        assert!(off && line.ends_with("❌ 16.7% under"), "{line}");
        assert_eq!(build(&n).1["color"], RED);
        n.check = Check::Differs(
            1_000_000_000.0,
            "the appraisal isn't of the contract's items".into(),
        );
        let (line, off) = check_line(&n);
        assert!(
            off && line.ends_with("❌ the appraisal isn't of the contract's items"),
            "{line}"
        );
        n.check = Check::NotChecked("no Janice API key entered".into());
        assert!(
            check_line(&n)
                .0
                .ends_with("not checked: no Janice API key entered")
        );
    }

    #[test]
    fn long_item_lists_are_cut() {
        let mut n = notice();
        n.event = Event::Completed;
        n.status = "finished".into();
        n.acceptor = Some("Acme Holdings".into());
        n.items = (0..12)
            .map(|i| (i + 1, format!("Item {i}"), 1, true))
            .collect();
        let (_, card) = build(&n);
        let items = card["fields"].as_array().unwrap().last().unwrap()[1]
            .as_str()
            .unwrap()
            .to_owned();
        assert!(items.ends_with("Item 9 x1\n…and 2 more"), "{items}");
        assert!(
            card["description"]
                .as_str()
                .unwrap()
                .starts_with("Acme Holdings completed the contract.")
        );
        assert_eq!(card["color"], GREEN);
    }

    #[test]
    fn numbers_read_well() {
        assert_eq!(isk(1_060_000_000.0), "1.06B ISK");
        assert_eq!(isk(850_200_000.0), "850.2M ISK");
        assert_eq!(isk(12_500.0), "12,500 ISK");
        assert_eq!(m3(96_986.8), "96,986.8 m³");
    }
}
