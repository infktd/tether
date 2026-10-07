//! What moons and chunks are worth, as aa-moonmining works it out.
//!
//! - A moon's monthly value is, over its ores, price × share × the ore a
//!   drill can pull in a month ÷ the ore's volume per unit. The month is
//!   aa-moonmining's settings: `MOONMINING_VOLUME_PER_DAY` (960,400 m³)
//!   times `MOONMINING_DAYS_PER_MONTH` (30.4), both editable on Settings.
//! - A chunk holds the drill's hourly volume for every hour it was
//!   extracted, split between the moon's ores by the survey's shares.
//! - Prices are CCP's average price of the ore itself (ESI's
//!   `/markets/prices/`), as aa-moonmining's default
//!   (`MOONMINING_USE_REPROCESS_PRICING` off). Where an ore has no
//!   average, Tether uses its adjusted price; aa-moonmining counts it as
//!   0. With aa-moonmining's opt-in reprocess pricing on, an ore is worth
//!   its refined materials (from the static data Tether bundles) at their
//!   prices × `MOONMINING_REPROCESSING_YIELD` (0.85), per unit; SQL works
//!   it out (`REPRICE`).

use chrono::{DateTime, Utc};
use serde::Deserialize;

/// aa-moonmining's defaults.
pub const VOLUME_PER_DAY: f64 = 960_400.0;
pub const DAYS_PER_MONTH: f64 = 30.4;
/// Every moon ore (R4 to R64, uncompressed) is 10 m³ a unit.
pub const ORE_VOLUME: f64 = 10.0;
/// aa-moonmining's MOONMINING_REPROCESSING_YIELD.
pub const REPROCESSING_YIELD: f64 = 0.85;

/// One type's refined materials, as the static data's `sde-materials`
/// answers: per portion of `portion_size` units.
#[derive(Debug, Deserialize)]
pub struct Materials {
    pub type_id: i64,
    #[serde(default = "one")]
    pub portion_size: i64,
    #[serde(default)]
    pub materials: Vec<Material>,
}

#[derive(Debug, Deserialize)]
pub struct Material {
    pub type_id: i64,
    pub quantity: i64,
}

fn one() -> i64 {
    1
}

/// How much ore a drill pulls: the settings values are worked out with.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rates {
    /// m³ a day.
    pub per_day: f64,
    pub days_per_month: f64,
}

impl Default for Rates {
    fn default() -> Self {
        Rates {
            per_day: VOLUME_PER_DAY,
            days_per_month: DAYS_PER_MONTH,
        }
    }
}

impl Rates {
    /// The ore a drill pulls in a month, m³.
    pub fn per_month(&self) -> f64 {
        self.per_day * self.days_per_month
    }

    /// A moon's value per month from Σ share × unit price over its ores
    /// (which SQL sums).
    pub fn monthly(&self, share_times_price: f64) -> f64 {
        share_times_price * self.per_month() / ORE_VOLUME
    }

    /// The volume of a chunk extracted from `start` to `arrival`, m³.
    pub fn chunk_volume(&self, start: DateTime<Utc>, arrival: DateTime<Utc>) -> f64 {
        let hours = (arrival - start).num_seconds().max(0) as f64 / 3600.0;
        hours * self.per_day / 24.0
    }
}

/// A chunk's value from its volume and Σ share × unit price.
pub fn chunk(volume: f64, share_times_price: f64) -> f64 {
    volume / ORE_VOLUME * share_times_price
}

/// One ore of a chunk: its volume (m³), units and value.
pub fn ore_in_chunk(volume: f64, share: f64, price: f64) -> (f64, f64, f64) {
    let ore = volume * share;
    let units = ore / ORE_VOLUME;
    (ore, units, units * price)
}

/// "12.3%".
pub fn percent(fraction: f64) -> String {
    if fraction.is_finite() {
        format!("{:.1}%", fraction * 100.0)
    } else {
        "0.0%".into()
    }
}

/// "R64" for a rarity class, "" when unknown.
pub fn rarity(class: i64) -> String {
    if class > 0 {
        format!("R{class}")
    } else {
        String::new()
    }
}

/// A rarity class as the host's grade, darker to brighter by value: R4 is
/// 0, R64 is 4 (unknown: 0).
pub fn grade(class: i64) -> u8 {
    match class {
        8 => 1,
        16 => 2,
        32 => 3,
        64 => 4,
        _ => 0,
    }
}

/// A moon's ores as parsed from `rarity:share,rarity:share` (the list's
/// query aggregates them so): the ring's parts.
pub fn parts(aggregated: &str) -> Vec<(i64, f64)> {
    aggregated
        .split(',')
        .filter_map(|part| {
            let (class, share) = part.split_once(':')?;
            let share: f64 = share.parse().ok()?;
            (share.is_finite() && share > 0.0).then_some((class.parse().unwrap_or(0), share))
        })
        .take(8)
        .collect()
}

/// ISK in a few characters, for the middle of a ring: `1.84B`, `620M`.
pub fn short_isk(isk: f64) -> String {
    let isk = finite(isk);
    let (scaled, unit) = if isk >= 1e12 {
        (isk / 1e12, "T")
    } else if isk >= 1e9 {
        (isk / 1e9, "B")
    } else if isk >= 1e6 {
        (isk / 1e6, "M")
    } else if isk >= 1e3 {
        (isk / 1e3, "K")
    } else {
        return format!("{isk:.0}");
    };
    if scaled >= 100.0 {
        format!("{scaled:.0}{unit}")
    } else {
        format!("{scaled:.2}{unit}")
    }
}

/// An ISK amount the host accepts (finite).
pub fn finite(isk: f64) -> f64 {
    if isk.is_finite() { isk } else { 0.0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn materials_read_as_the_static_data_answers() {
        let read: Vec<Materials> = serde_json::from_str(
            r#"[{"type_id":45490,"portion_size":100,"materials":[{"type_id":35,"quantity":8000}]},
                {"type_id":1}]"#,
        )
        .unwrap();
        assert_eq!(read[0].portion_size, 100);
        assert_eq!(read[0].materials[0].type_id, 35);
        assert_eq!(read[0].materials[0].quantity, 8000);
        // A type it doesn't know the materials of: none, a unit a portion.
        assert_eq!(read[1].portion_size, 1);
        assert!(read[1].materials.is_empty());
    }

    #[test]
    fn a_moons_monthly_value_is_aa_moonminings() {
        let rates = Rates::default();
        // 30% Zeolites at 8,000 ISK and 20% Xenotime at 150,000 ISK a unit:
        // Σ share × price = 2,400 + 30,000 = 32,400.
        let sum = 0.3 * 8_000.0 + 0.2 * 150_000.0;
        // 960,400 m³ × 30.4 days ÷ 10 m³ = 2,919,616 units a month.
        assert_eq!(rates.per_month(), 29_196_160.0);
        let value = rates.monthly(sum);
        assert!((value - 32_400.0 * 2_919_616.0).abs() < 1.0, "{value}");
        assert_eq!(rates.monthly(0.0), 0.0);
        // The settings change it in proportion.
        let half = Rates {
            per_day: VOLUME_PER_DAY / 2.0,
            days_per_month: DAYS_PER_MONTH,
        };
        assert!((half.monthly(sum) * 2.0 - value).abs() < 1.0);
    }

    #[test]
    fn a_chunk_holds_the_drills_hourly_volume() {
        let rates = Rates::default();
        // Six and a half days: 156 hours at 40,016.67 m³ an hour.
        let volume = rates.chunk_volume(at("2026-09-20T00:00:00Z"), at("2026-09-26T12:00:00Z"));
        assert!((volume - 6_242_600.0).abs() < 0.01, "{volume}");
        // A backwards pair is empty, not negative.
        assert_eq!(
            rates.chunk_volume(at("2026-09-26T00:00:00Z"), at("2026-09-20T00:00:00Z")),
            0.0
        );
        // A quarter of it Zeolites at 8,000 ISK: 156,065 units.
        let (ore, units, isk) = ore_in_chunk(volume, 0.25, 8_000.0);
        assert!((ore - 1_560_650.0).abs() < 0.01);
        assert!((units - 156_065.0).abs() < 0.01);
        assert!((isk - 1_248_520_000.0).abs() < 1.0);
        // The whole chunk, from Σ share × price over two ores.
        let whole = chunk(volume, 0.25 * 8_000.0 + 0.75 * 1_000.0);
        assert!((whole - 624_260.0 * 2_750.0).abs() < 1.0, "{whole}");
    }

    #[test]
    fn labels() {
        assert_eq!(percent(0.1234), "12.3%");
        assert_eq!(percent(f64::NAN), "0.0%");
        assert_eq!(rarity(64), "R64");
        assert_eq!(rarity(0), "");
        assert_eq!(finite(f64::INFINITY), 0.0);
    }
}

#[cfg(test)]
mod ring_tests {
    use super::*;

    #[test]
    fn rarity_grades_and_parts() {
        assert_eq!(grade(4), 0);
        assert_eq!(grade(64), 4);
        assert_eq!(grade(0), 0);
        assert_eq!(parts("64:0.31,4:0.69"), vec![(64, 0.31), (4, 0.69)]);
        assert_eq!(parts(""), vec![]);
        assert_eq!(parts("64:0,x:y,8:0.5"), vec![(8, 0.5)]);
        assert_eq!(short_isk(1_840_000_000.0), "1.84B");
        assert_eq!(short_isk(620_000_000.0), "620M");
        assert_eq!(short_isk(5.0), "5");
    }
}
