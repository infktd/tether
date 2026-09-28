//! aa-freight's pricing: a route, its reward and the contract check.

use tether_plugin_sdk::storage::{self, Value as Db};

/// A pricing as stored.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Pricing {
    pub id: i64,
    pub start: i64,
    pub end: i64,
    pub active: bool,
    pub bidirectional: bool,
    pub price_base: Option<f64>,
    pub price_min: Option<f64>,
    pub price_per_volume: Option<f64>,
    pub use_modifier: bool,
    pub price_per_collateral_percent: Option<f64>,
    pub collateral_min: Option<f64>,
    pub collateral_max: Option<f64>,
    pub volume_min: Option<f64>,
    pub volume_max: Option<f64>,
    pub days_to_expire: Option<i64>,
    pub days_to_complete: Option<i64>,
    pub details: String,
}

const COLUMNS: &str = "id, start_location, end_location, active, bidirectional, price_base, \
     price_min, price_per_volume, use_modifier, price_per_collateral_percent, collateral_min, \
     collateral_max, volume_min, volume_max, days_to_expire, days_to_complete, details";

fn opt_float(row: &[Db], i: usize) -> Option<f64> {
    row.get(i).and_then(Db::as_float)
}

fn opt_int(row: &[Db], i: usize) -> Option<i64> {
    row.get(i).and_then(Db::as_integer)
}

fn flag(row: &[Db], i: usize) -> bool {
    row.get(i).and_then(Db::as_bool).unwrap_or(false)
}

/// Every pricing (active or not), by id.
pub fn all() -> Result<Vec<Pricing>, storage::Error> {
    let rows = storage::query(&format!("SELECT {COLUMNS} FROM pricings ORDER BY id"), &[])?;
    Ok(rows
        .rows
        .iter()
        .map(|r| Pricing {
            id: opt_int(r, 0).unwrap_or_default(),
            start: opt_int(r, 1).unwrap_or_default(),
            end: opt_int(r, 2).unwrap_or_default(),
            active: flag(r, 3),
            bidirectional: flag(r, 4),
            price_base: opt_float(r, 5),
            price_min: opt_float(r, 6),
            price_per_volume: opt_float(r, 7),
            use_modifier: flag(r, 8),
            price_per_collateral_percent: opt_float(r, 9),
            collateral_min: opt_float(r, 10),
            collateral_max: opt_float(r, 11),
            volume_min: opt_float(r, 12),
            volume_max: opt_float(r, 13),
            days_to_expire: opt_int(r, 14),
            days_to_complete: opt_int(r, 15),
            details: r
                .get(16)
                .and_then(Db::as_text)
                .unwrap_or_default()
                .to_owned(),
        })
        .collect())
}

impl Pricing {
    /// The price per volume with the global modifier (a percentage), as
    /// aa-freight: never below zero.
    pub fn price_per_volume_eff(&self, modifier: Option<f64>) -> Option<f64> {
        let base = self.price_per_volume.filter(|p| *p != 0.0)?;
        Some(match modifier.filter(|_| self.use_modifier) {
            Some(m) if m != 0.0 => (base + base * m / 100.0).max(0.0),
            _ => base,
        })
    }

    /// aa-freight's reward: the base, per volume and per collateral
    /// percent, at least the minimum.
    pub fn price(&self, volume: f64, collateral: f64, modifier: Option<f64>) -> f64 {
        let per_volume = self.price_per_volume_eff(modifier).unwrap_or(0.0);
        let percent = self.price_per_collateral_percent.unwrap_or(0.0);
        (self.price_base.unwrap_or(0.0) + volume * per_volume + collateral * (percent / 100.0))
            .max(self.price_min.unwrap_or(0.0))
    }

    pub fn requires_volume(&self) -> bool {
        self.price_per_volume.is_some_and(|p| p != 0.0) || self.volume_min.is_some_and(|v| v != 0.0)
    }

    pub fn requires_collateral(&self) -> bool {
        self.price_per_collateral_percent.is_some_and(|p| p != 0.0)
            || self.collateral_min.is_some_and(|c| c != 0.0)
    }

    pub fn is_fix_price(&self) -> bool {
        self.price_base.is_some()
            && self.price_min.is_none()
            && self.price_per_volume.is_none()
            && self.price_per_collateral_percent.is_none()
    }

    /// aa-freight's contract check: what's wrong with a contract (or a
    /// calculation) against this pricing, none when it passes.
    pub fn issues(
        &self,
        volume: f64,
        collateral: f64,
        reward: Option<f64>,
        modifier: Option<f64>,
    ) -> Vec<String> {
        let mut issues = Vec::new();
        if let Some(min) = self.volume_min.filter(|m| *m != 0.0 && volume < *m) {
            issues.push(format!(
                "below the minimum required volume of {} m3",
                thousands(min)
            ));
        }
        if let Some(max) = self.volume_max.filter(|m| *m != 0.0 && volume > *m) {
            issues.push(format!(
                "exceeds the maximum allowed volume of {} m3",
                thousands(max)
            ));
        }
        if let Some(max) = self.collateral_max.filter(|m| *m != 0.0 && collateral > *m) {
            issues.push(format!(
                "exceeds the maximum allowed collateral of {} ISK",
                thousands(max)
            ));
        }
        if let Some(min) = self.collateral_min.filter(|m| *m != 0.0 && collateral < *m) {
            issues.push(format!(
                "below the minimum required collateral of {} ISK",
                thousands(min)
            ));
        }
        if let Some(reward) = reward {
            let price = self.price(volume, collateral, modifier);
            if reward < price {
                issues.push(format!(
                    "reward is below the calculated price of {} ISK",
                    thousands(price)
                ));
            }
        }
        issues
    }

    /// Whether this pricing is for a route from `start` to `end` (either
    /// way when bidirectional).
    pub fn serves(&self, start: i64, end: i64) -> bool {
        (self.start == start && self.end == end)
            || (self.bidirectional && self.start == end && self.end == start)
    }
}

/// The active pricing for a route, if any.
pub fn for_route(pricings: &[Pricing], start: i64, end: i64) -> Option<&Pricing> {
    pricings
        .iter()
        .filter(|p| p.active)
        .find(|p| p.start == start && p.end == end)
        .or_else(|| {
            pricings
                .iter()
                .filter(|p| p.active)
                .find(|p| p.serves(start, end))
        })
}

/// A whole number with thousands separators, as aa-freight's `{:,.0f}`.
pub fn thousands(value: f64) -> String {
    let digits = format!("{:.0}", value.abs());
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    if value < 0.0 && out != "0" {
        out.insert(0, '-');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route() -> Pricing {
        Pricing {
            id: 1,
            start: 60003760,
            end: 1_022_734_985_679,
            active: true,
            bidirectional: true,
            price_base: Some(50_000_000.0),
            price_per_volume: Some(500.0),
            use_modifier: true,
            price_per_collateral_percent: Some(1.0),
            volume_max: Some(320_000.0),
            collateral_max: Some(5_000_000_000.0),
            ..Pricing::default()
        }
    }

    #[test]
    fn prices_as_aa_freight() {
        let p = route();
        // 50m + 100,000 m3 x 500 + 1% of 1b = 110m.
        assert_eq!(p.price(100_000.0, 1_000_000_000.0, None), 110_000_000.0);
        // A 10% modifier on the volume price: 550 per m3.
        assert_eq!(p.price(100_000.0, 0.0, Some(10.0)), 105_000_000.0);
        // Never below the minimum.
        let min = Pricing {
            price_min: Some(200_000_000.0),
            ..route()
        };
        assert_eq!(min.price(1.0, 0.0, None), 200_000_000.0);
        assert!(p.requires_volume() && p.requires_collateral() && !p.is_fix_price());
    }

    #[test]
    fn contracts_are_checked_as_aa_freight() {
        let p = route();
        assert!(
            p.issues(100_000.0, 1e9, Some(110_000_000.0), None)
                .is_empty()
        );
        let issues = p.issues(400_000.0, 6e9, Some(1.0), None);
        assert_eq!(issues.len(), 3, "{issues:?}");
        assert_eq!(
            issues[0],
            "exceeds the maximum allowed volume of 320,000 m3"
        );
        assert!(issues[2].starts_with("reward is below the calculated price of"));
        assert!(p.serves(1_022_734_985_679, 60003760));
        assert_eq!(
            for_route(std::slice::from_ref(&p), 1_022_734_985_679, 60003760),
            Some(&p)
        );
        let one_way = Pricing {
            bidirectional: false,
            ..route()
        };
        assert!(for_route(&[one_way], 1_022_734_985_679, 60003760).is_none());
        assert_eq!(thousands(1234567.4), "1,234,567");
    }
}
