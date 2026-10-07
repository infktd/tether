//! aa-buybackprogram's pricing (`helpers.py` `get_item_prices`,
//! `get_item_values`, `get_item_buy_value`), as pure functions over what
//! the app read: an item's static data, market and NPC prices, and the
//! program's rules. Rules are AA's; its bugs are fixed (Jay, 2026-10-07):
//! bonds follow their own switch (B6), an NPC price counts for every price
//! type (B7), a missing compressed type never fails a calculation (B9),
//! total volume always adds up (B10), a row's base and unit values are the
//! winning variant's (B28), a static price is the price (B29), and
//! rejected rows aren't part of what the contract must hold (B30).

use std::collections::HashMap;

/// aa-buybackprogram's `ORE_EVE_GROUPS` (constants.py), as written.
const ORE_GROUPS: &[i64] = &[
    450, 451, 452, 453, 454, 455, 456, 457, 458, 459, 460, 461, 462, 465, 467, 468, 469, 1884,
    1911, 1920, 1921, 1922, 1923, 2024, 2029, 4029, 4030, 4031, 4094, 4513, 4514, 4515, 4516, 4755,
    4756, 4757, 4758, 4759, 4915,
];
const BLUE_LOOT: &[i64] = &[30744, 30745, 30746, 30747];
const RED_LOOT: &[i64] = &[48121, 60459, 91773];
const OPE_GROUPS: &[i64] = &[493];
const BOND_GROUPS: &[i64] = &[1248];
/// Categories whose meta 0-4 items may be valued as scrap.
const T1_CATEGORIES: &[&str] = &["Ship", "Module", "Charge", "Drone", "Subsystem"];

pub fn is_ore(group_id: i64) -> bool {
    ORE_GROUPS.contains(&group_id)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PriceType {
    Buy,
    Sell,
    Split,
}

impl PriceType {
    pub fn parse(s: &str) -> Self {
        match s {
            "Sell" => Self::Sell,
            "Split" => Self::Split,
            _ => Self::Buy,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Buy => "Buy",
            Self::Sell => "Sell",
            Self::Split => "Split",
        }
    }

    /// The unit price a (buy, sell) pair gives.
    pub fn pick(self, buy: f64, sell: f64) -> f64 {
        match self {
            Self::Buy => buy,
            Self::Sell => sell,
            Self::Split => (buy + sell) / 2.0,
        }
    }
}

/// A program's pricing rules.
#[derive(Debug, Clone)]
pub struct Rules {
    pub tax: f64,
    pub hauling_fuel_cost: f64,
    pub density_modifier: bool,
    pub compression_density_modifier: bool,
    pub density_threshold: f64,
    pub density_tax: f64,
    pub allow_all_items: bool,
    pub use_refined_value: bool,
    pub use_compressed_value: bool,
    pub use_raw_ore_value: bool,
    pub allow_unpacked_items: bool,
    pub refining_rate: f64,
    pub use_t1_scrap: bool,
    pub t1_refining_rate: f64,
    pub blue_loot_npc_price: bool,
    pub red_loot_npc_price: bool,
    pub ope_npc_price: bool,
    pub bonds_npc_price: bool,
    pub price_type: PriceType,
}

/// A special tax or allow-list entry, by type or market group.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProgramItem {
    pub type_id: Option<i64>,
    pub market_group_id: Option<i64>,
    pub item_tax: f64,
    pub disallow: bool,
}

/// What a program says about particular items.
#[derive(Debug, Clone, Default)]
pub struct Overrides {
    pub items: Vec<ProgramItem>,
    pub static_prices: HashMap<i64, f64>,
    pub watch_types: Vec<i64>,
    pub watch_groups: Vec<i64>,
}

impl Overrides {
    /// The entry for a type: its own, else the nearest market group
    /// above it (AA walks up to the root).
    pub fn entry(&self, item: &Item) -> Option<&ProgramItem> {
        self.items
            .iter()
            .find(|p| p.type_id == Some(item.type_id))
            .or_else(|| {
                item.market_group_chain.iter().find_map(|group| {
                    self.items
                        .iter()
                        .find(|p| p.market_group_id == Some(*group))
                })
            })
    }
}

/// An item's static data (`sde-types`, `sde-materials`).
#[derive(Debug, Clone, Default)]
pub struct Item {
    pub type_id: i64,
    pub name: String,
    pub published: bool,
    pub group_id: i64,
    pub category_name: String,
    /// Its market group and every one above; empty: none.
    pub market_group_chain: Vec<i64>,
    pub packaged_volume: f64,
    pub portion_size: i64,
    pub meta_level: Option<i64>,
    /// The compressed form, and its volume.
    pub compressed: Option<(i64, f64)>,
    /// What a portion reprocesses into: (type, quantity).
    pub materials: Vec<(i64, i64)>,
}

/// A market price pair and its age in hours.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Price {
    pub buy: f64,
    pub sell: f64,
    pub age_hours: f64,
}

/// What the pricing reads besides the item: market prices by type, ESI's
/// average prices (NPC), and the settings' words.
pub struct Market<'a> {
    pub prices: &'a HashMap<i64, Price>,
    pub npc: &'a HashMap<i64, f64>,
    pub source_name: &'a str,
    pub age_warning_hours: f64,
}

impl Market<'_> {
    fn price(&self, type_id: i64) -> Price {
        self.prices.get(&type_id).copied().unwrap_or_default()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Danger,
    Warning,
    Success,
    Info,
    Watch,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Note {
    pub tone: Tone,
    pub text: String,
}

fn note(tone: Tone, text: String) -> Note {
    Note { tone, text }
}

/// One way of valuing a row.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Variant {
    pub label: &'static str,
    /// Before taxes, the whole row.
    pub raw_value: f64,
    /// After taxes, the whole row.
    pub value: f64,
    /// After taxes, one unit.
    pub unit_value: f64,
    pub tax_percent: f64,
    /// Refined: each material's line (name filled by the caller).
    pub materials: Vec<MaterialLine>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct MaterialLine {
    pub type_id: i64,
    /// Units the row yields.
    pub quantity: f64,
    pub unit_price: f64,
    pub value: f64,
}

/// A priced row of a calculation.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Row {
    pub type_id: Option<i64>,
    pub name: String,
    pub quantity: i64,
    /// Not priced: not found, forbidden, a blueprint, unpacked or
    /// disallowed. Kept as a row with its notes; not part of the
    /// contract.
    pub rejected: bool,
    pub market: Price,
    /// "Our Price": one unit, after taxes.
    pub unit_value: f64,
    /// "Base Price": the whole row, before taxes.
    pub raw_value: f64,
    /// "Row Total": the whole row, after taxes.
    pub buy_value: f64,
    pub tax_percent: f64,
    pub variants: Vec<Variant>,
    pub winner: Option<&'static str>,
    pub notes: Vec<Note>,
    /// Volume for hauling and the invoice: compressed or packaged.
    pub hauling_volume: f64,
    pub static_price: bool,
}

impl Row {
    /// A row priced at nothing, with why.
    pub fn rejected(type_id: Option<i64>, name: &str, quantity: i64, why: Note) -> Self {
        Row {
            type_id,
            name: name.to_owned(),
            quantity,
            rejected: true,
            notes: vec![why],
            ..Row::default()
        }
    }
}

/// The density tax on a unit price (`get_price_dencity_tax`).
fn density_tax(rules: &Rules, unit_price: f64, volume: f64, ore: bool) -> f64 {
    if !rules.density_modifier || volume <= 0.0 || (ore && !rules.compression_density_modifier) {
        return 0.0;
    }
    let density = unit_price / volume;
    if density > 0.0 && density < rules.density_threshold {
        rules.density_tax
    } else {
        0.0
    }
}

fn scrap_eligible(item: &Item) -> bool {
    T1_CATEGORIES.contains(&item.category_name.as_str()) && item.meta_level.is_none_or(|m| m <= 4)
}

fn money(v: f64) -> String {
    let whole = v.round() as i64;
    let digits = whole.unsigned_abs().to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    if whole < 0 { format!("-{out}") } else { out }
}

/// Prices one accepted row (the item was found and isn't forbidden, a
/// blueprint or unpacked; the caller checked).
pub fn price_row(
    rules: &Rules,
    overrides: &Overrides,
    market: &Market<'_>,
    item: &Item,
    quantity: i64,
) -> Row {
    let name = item.name.as_str();
    let q = quantity as f64;
    let ore = is_ore(item.group_id);
    let entry = overrides.entry(item);
    let item_tax = entry.map_or(0.0, |e| e.item_tax);
    let mut notes = Vec::new();
    let mut row = Row {
        type_id: Some(item.type_id),
        name: item.name.clone(),
        quantity,
        market: market.price(item.type_id),
        hauling_volume: item.packaged_volume,
        ..Row::default()
    };

    // 1. A static price is the price, untaxed (B29: nothing beats it).
    let static_price = overrides.static_prices.get(&item.type_id).copied();
    // 4-5. Allowed?
    let allowed = if rules.allow_all_items {
        !entry.is_some_and(|e| e.disallow)
    } else {
        entry.is_some_and(|e| !e.disallow)
    };
    if !item.published {
        notes.push(note(
            Tone::Danger,
            format!("{name} is not a published item. It may be expired or an invalid paste."),
        ));
    } else if item.market_group_chain.is_empty() {
        notes.push(note(
            Tone::Danger,
            format!(
                "{name} has no market information. Likely a mission item or special commodity."
            ),
        ));
    }
    if !allowed || !item.published || item.market_group_chain.is_empty() {
        if !allowed {
            notes.push(note(
                Tone::Danger,
                format!("{name} is disallowed in this program"),
            ));
        }
        row.rejected = true;
        row.notes = notes;
        return row;
    }
    if let Some(price) = static_price {
        notes.push(note(
            Tone::Success,
            format!("Using static price of {} ISK for {name}", money(price)),
        ));
        row.static_price = true;
        row.unit_value = price;
        row.raw_value = price * q;
        row.buy_value = price * q;
        row.winner = Some("Static");
        row.variants.push(Variant {
            label: "Static",
            raw_value: price * q,
            value: price * q,
            unit_value: price,
            ..Variant::default()
        });
        add_watch(overrides, item, &mut notes);
        row.notes = notes;
        if let Some((_, volume)) = item
            .compressed
            .filter(|_| ore && (rules.use_compressed_value || rules.compression_density_modifier))
        {
            row.hauling_volume = volume;
        }
        return row;
    }

    let multiplier = |density: f64| (100.0 - (rules.tax + item_tax + density)) / 100.0;
    let total_tax = |density: f64| rules.tax + item_tax + density;

    // 3. T1 scrap replaces the market price.
    let mut base = market.price(item.type_id);
    if rules.use_t1_scrap && rules.t1_refining_rate > 0.0 && scrap_eligible(item) {
        let rate = rules.t1_refining_rate / 100.0;
        let portion = item.portion_size.max(1) as f64;
        let scrap: f64 = item
            .materials
            .iter()
            .map(|(m, qty)| {
                let yielded = ((*qty as f64 / portion) * rate).floor();
                if yielded <= 0.0 {
                    return 0.0;
                }
                let unit = overrides
                    .static_prices
                    .get(m)
                    .copied()
                    .unwrap_or_else(|| market.price(*m).buy);
                yielded * unit
            })
            .sum();
        if scrap > 0.0 {
            base = Price {
                buy: scrap,
                sell: scrap,
                age_hours: 0.0,
            };
            notes.push(note(
                Tone::Warning,
                format!(
                    "Valued at {}% scrap mineral cost (Meta {}).",
                    rules.t1_refining_rate,
                    item.meta_level.unwrap_or(0)
                ),
            ));
        }
    }
    if base.buy == 0.0 {
        notes.push(note(
            Tone::Warning,
            format!("{name} has no buy orders in {}", market.source_name),
        ));
    }
    if base.age_hours > market.age_warning_hours {
        notes.push(note(
            Tone::Warning,
            format!(
                "Price data for {name} is more than {} hours old",
                market.age_warning_hours
            ),
        ));
    }

    let mut variants: Vec<Variant> = Vec::new();
    // Raw.
    let raw_used = !ore || rules.use_raw_ore_value;
    if raw_used {
        let p = rules.price_type.pick(base.buy, base.sell);
        let density = density_tax(rules, p, item.packaged_volume, ore);
        let m = multiplier(density);
        variants.push(Variant {
            label: "Raw",
            raw_value: q * p,
            value: q * p * m,
            unit_value: p * m,
            tax_percent: total_tax(density),
            materials: Vec::new(),
        });
    }
    let compressed = if ore { item.compressed } else { None };
    // Refined (ore).
    if ore && rules.use_refined_value {
        if item.materials.is_empty() {
            notes.push(note(
                Tone::Warning,
                format!(
                    "Refined price valuation is active but TypeMaterials for {name} are missing."
                ),
            ));
        } else {
            let density = match compressed {
                Some((c, volume)) => density_tax(rules, market.price(c).buy, volume, ore),
                None => density_tax(rules, base.buy, item.packaged_volume, ore),
            };
            let m = multiplier(density);
            let rate = rules.refining_rate / 100.0;
            let portion = item.portion_size.max(1) as f64;
            let mut variant = Variant {
                label: "Refined",
                tax_percent: total_tax(density),
                ..Variant::default()
            };
            for (material, qty) in &item.materials {
                let mp = market.price(*material);
                let p = rules.price_type.pick(mp.buy, mp.sell);
                let quantity = *qty as f64 * q / portion;
                let raw = quantity * rate * p;
                variant.raw_value += raw;
                variant.value += raw * m;
                variant.unit_value += p * m * (*qty as f64 / portion) * rate;
                variant.materials.push(MaterialLine {
                    type_id: *material,
                    quantity: quantity * rate,
                    unit_price: p,
                    value: raw * m,
                });
            }
            variants.push(variant);
        }
    }
    // Compressed (ore).
    if let Some((c, volume)) = compressed {
        if rules.use_compressed_value {
            let cp = market.price(c);
            let p = rules.price_type.pick(cp.buy, cp.sell);
            let density = density_tax(rules, p, volume, ore);
            let m = multiplier(density);
            variants.push(Variant {
                label: "Compressed",
                raw_value: q * p,
                value: q * p * m,
                unit_value: p * m,
                tax_percent: total_tax(density),
                materials: Vec::new(),
            });
        }
        if rules.use_compressed_value || rules.compression_density_modifier {
            row.hauling_volume = volume;
        }
    }
    // NPC (ESI's average price), whatever the price type (B7).
    let npc = (BLUE_LOOT.contains(&item.type_id) && rules.blue_loot_npc_price)
        || (RED_LOOT.contains(&item.type_id) && rules.red_loot_npc_price)
        || (OPE_GROUPS.contains(&item.group_id) && rules.ope_npc_price)
        || (BOND_GROUPS.contains(&item.group_id) && rules.bonds_npc_price);
    let mut npc_variant = None;
    if npc {
        let average = market.npc.get(&item.type_id).copied().unwrap_or(0.0);
        if average == 0.0 {
            notes.push(note(
                Tone::Warning,
                format!("{name} is missing price data."),
            ));
        }
        let density = density_tax(rules, average, item.packaged_volume, ore);
        let m = multiplier(density);
        npc_variant = Some(Variant {
            label: "NPC",
            raw_value: q * average,
            value: q * average * m,
            unit_value: average * m,
            tax_percent: total_tax(density),
            materials: Vec::new(),
        });
    }

    // Best value: NPC wins outright; else the highest after-tax value,
    // raw first on a tie, and the row's figures are the winner's (B28).
    let winner = match &npc_variant {
        Some(v) => Some(v.clone()),
        None => variants
            .iter()
            .fold(None::<&Variant>, |best, v| match best {
                Some(b) if b.value >= v.value => Some(b),
                _ => Some(v),
            })
            .cloned(),
    };
    if let Some(v) = npc_variant {
        variants.push(v);
    }
    match &winner {
        None => {
            notes.push(note(Tone::Danger, format!("{name} has no price data")));
        }
        Some(w) => {
            row.unit_value = w.unit_value;
            row.raw_value = w.raw_value;
            row.buy_value = w.value;
            row.tax_percent = w.tax_percent;
            row.winner = Some(w.label);
            let several = variants.len() > 1;
            match w.label {
                "NPC" => notes.push(note(
                    Tone::Info,
                    format!(
                        "Using NPC buy price for {name} instead of {} prices",
                        market.source_name
                    ),
                )),
                "Refined" => notes.push(note(
                    Tone::Info,
                    format!("Best price: using refined value for {name}"),
                )),
                "Compressed" => notes.push(note(
                    Tone::Info,
                    format!("Best price: using compressed value for {name}"),
                )),
                _ if several && ore => notes.push(note(
                    Tone::Info,
                    format!("Best price: using raw ore value for {name}"),
                )),
                _ => {}
            }
            let density = w.tax_percent - rules.tax - item_tax;
            if density > 0.0 {
                notes.push(note(
                    Tone::Warning,
                    format!(
                        "{name} has low price density: {density}% low price density tax applied"
                    ),
                ));
            }
        }
    }
    if item_tax > 0.0 {
        notes.push(note(
            Tone::Warning,
            format!("{name} has an additional {item_tax}% item-specific tax applied"),
        ));
    } else if item_tax < 0.0 {
        notes.push(note(
            Tone::Success,
            format!("{name} has a {}% item-specific discount applied", -item_tax),
        ));
    }
    add_watch(overrides, item, &mut notes);
    row.variants = variants;
    row.notes = notes;
    row
}

fn add_watch(overrides: &Overrides, item: &Item, notes: &mut Vec<Note>) {
    if overrides.watch_types.contains(&item.type_id)
        || overrides.watch_groups.contains(&item.group_id)
    {
        notes.push(note(
            Tone::Watch,
            format!(
                "{} is on the watchlist and requires manual review",
                item.name
            ),
        ));
    }
}

/// A calculation's totals (`get_item_buy_value`).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Totals {
    /// "Price before expenses": every row's base value.
    pub raw: f64,
    /// After taxes.
    pub after_tax: f64,
    /// "Program taxes".
    pub taxes: f64,
    pub donation: f64,
    pub hauling: f64,
    pub volume: f64,
    /// "I will receive" (negative shows as 0).
    pub net: f64,
}

pub fn totals(rules: &Rules, rows: &[Row], donation_percent: f64) -> Totals {
    let priced = rows.iter().filter(|r| !r.rejected);
    let mut t = Totals::default();
    for r in priced {
        t.raw += r.raw_value;
        t.after_tax += r.buy_value;
        // B10: every priced row's volume, hauling or not.
        t.volume += r.hauling_volume * r.quantity as f64;
    }
    t.taxes = t.raw - t.after_tax;
    if donation_percent > 0.0 {
        t.donation = t.after_tax * donation_percent / 100.0;
    }
    if rules.hauling_fuel_cost > 0.0 {
        t.hauling = t.volume * rules.hauling_fuel_cost;
    }
    t.net = t.after_tax - t.hauling - t.donation;
    t
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::float_cmp)] // test code

    use super::*;

    fn rules() -> Rules {
        Rules {
            tax: 10.0,
            hauling_fuel_cost: 0.0,
            density_modifier: false,
            compression_density_modifier: false,
            density_threshold: 0.0,
            density_tax: 0.0,
            allow_all_items: true,
            use_refined_value: false,
            use_compressed_value: false,
            use_raw_ore_value: true,
            allow_unpacked_items: false,
            refining_rate: 0.0,
            use_t1_scrap: false,
            t1_refining_rate: 50.0,
            blue_loot_npc_price: false,
            red_loot_npc_price: false,
            ope_npc_price: false,
            bonds_npc_price: false,
            price_type: PriceType::Buy,
        }
    }

    fn tritanium() -> Item {
        Item {
            type_id: 34,
            name: "Tritanium".into(),
            published: true,
            group_id: 18,
            category_name: "Material".into(),
            market_group_chain: vec![1857, 1034, 533],
            packaged_volume: 0.01,
            portion_size: 1,
            ..Item::default()
        }
    }

    fn veldspar() -> Item {
        Item {
            type_id: 1230,
            name: "Veldspar".into(),
            published: true,
            group_id: 462,
            category_name: "Asteroid".into(),
            market_group_chain: vec![518, 54],
            packaged_volume: 0.1,
            portion_size: 100,
            compressed: Some((62516, 0.001)),
            materials: vec![(34, 400)],
            ..Item::default()
        }
    }

    fn prices() -> HashMap<i64, Price> {
        HashMap::from([
            (
                34,
                Price {
                    buy: 4.0,
                    sell: 5.0,
                    age_hours: 1.0,
                },
            ),
            (
                1230,
                Price {
                    buy: 10.0,
                    sell: 12.0,
                    age_hours: 1.0,
                },
            ),
            (
                62516,
                Price {
                    buy: 11.0,
                    sell: 13.0,
                    age_hours: 1.0,
                },
            ),
        ])
    }

    fn market<'a>(p: &'a HashMap<i64, Price>, npc: &'a HashMap<i64, f64>) -> Market<'a> {
        Market {
            prices: p,
            npc,
            source_name: "Jita",
            age_warning_hours: 48.0,
        }
    }

    #[test]
    fn a_plain_item_is_its_price_less_tax() {
        let (p, npc) = (prices(), HashMap::new());
        let row = price_row(
            &rules(),
            &Overrides::default(),
            &market(&p, &npc),
            &tritanium(),
            1000,
        );
        assert_eq!(row.raw_value, 4000.0);
        assert_eq!(row.buy_value, 3600.0);
        assert_eq!(row.unit_value, 3.6);
        assert_eq!(row.tax_percent, 10.0);
        let mut r = rules();
        r.price_type = PriceType::Split;
        let row = price_row(
            &r,
            &Overrides::default(),
            &market(&p, &npc),
            &tritanium(),
            1000,
        );
        assert_eq!(row.raw_value, 4500.0);
    }

    #[test]
    fn ore_takes_its_best_variant_and_its_figures() {
        let (p, npc) = (prices(), HashMap::new());
        let mut r = rules();
        r.use_refined_value = true;
        r.refining_rate = 90.0;
        r.use_compressed_value = true;
        // 100 Veldspar: raw 1000; compressed 1100; refined 400 × 0.9 × 4 = 1440.
        let row = price_row(
            &r,
            &Overrides::default(),
            &market(&p, &npc),
            &veldspar(),
            100,
        );
        assert_eq!(row.winner, Some("Refined"));
        assert!((row.raw_value - 1440.0).abs() < 1e-9);
        assert!((row.buy_value - 1296.0).abs() < 1e-9);
        assert_eq!(row.hauling_volume, 0.001);
        assert_eq!(row.variants.len(), 3);
    }

    #[test]
    fn taxes_follow_the_nearest_entry_and_static_prices_win() {
        let (p, npc) = (prices(), HashMap::new());
        let mut o = Overrides::default();
        o.items.push(ProgramItem {
            type_id: None,
            market_group_id: Some(1034),
            item_tax: 5.0,
            disallow: false,
        });
        let row = price_row(&rules(), &o, &market(&p, &npc), &tritanium(), 100);
        assert_eq!(row.tax_percent, 15.0);
        o.items.push(ProgramItem {
            type_id: Some(34),
            market_group_id: None,
            item_tax: -10.0,
            disallow: false,
        });
        let row = price_row(&rules(), &o, &market(&p, &npc), &tritanium(), 100);
        assert_eq!(row.tax_percent, 0.0);
        o.static_prices.insert(34, 6.0);
        let row = price_row(&rules(), &o, &market(&p, &npc), &tritanium(), 100);
        assert_eq!((row.buy_value, row.tax_percent), (600.0, 0.0));
        assert!(row.static_price);
    }

    #[test]
    fn disallowed_unpublished_and_unlisted_items_are_rejected() {
        let (p, npc) = (prices(), HashMap::new());
        let mut o = Overrides::default();
        o.items.push(ProgramItem {
            type_id: Some(34),
            market_group_id: None,
            item_tax: 0.0,
            disallow: true,
        });
        assert!(price_row(&rules(), &o, &market(&p, &npc), &tritanium(), 1).rejected);
        let mut r = rules();
        r.allow_all_items = false;
        assert!(
            price_row(
                &r,
                &Overrides::default(),
                &market(&p, &npc),
                &tritanium(),
                1
            )
            .rejected
        );
        let mut item = tritanium();
        item.published = false;
        assert!(price_row(&rules(), &Overrides::default(), &market(&p, &npc), &item, 1).rejected);
    }

    #[test]
    fn npc_items_take_the_average_for_any_price_type() {
        let p = prices();
        let npc = HashMap::from([(30744, 1_000_000.0)]);
        let item = Item {
            type_id: 30744,
            name: "Sleeper Data Library".into(),
            published: true,
            group_id: 880,
            category_name: "Commodity".into(),
            market_group_chain: vec![1],
            packaged_volume: 0.01,
            portion_size: 1,
            ..Item::default()
        };
        let mut r = rules();
        r.blue_loot_npc_price = true;
        r.price_type = PriceType::Sell;
        let row = price_row(&r, &Overrides::default(), &market(&p, &npc), &item, 2);
        assert_eq!(row.winner, Some("NPC"));
        assert_eq!(row.buy_value, 1_800_000.0);
    }

    #[test]
    fn density_tax_applies_below_the_threshold() {
        let (p, npc) = (prices(), HashMap::new());
        let mut r = rules();
        r.density_modifier = true;
        r.density_threshold = 1000.0;
        r.density_tax = 20.0;
        // Tritanium: 4 ISK / 0.01 m³ = 400 ISK/m³, below 1000.
        let row = price_row(
            &r,
            &Overrides::default(),
            &market(&p, &npc),
            &tritanium(),
            100,
        );
        assert_eq!(row.tax_percent, 30.0);
    }

    #[test]
    fn totals_take_donation_then_hauling() {
        let (p, npc) = (prices(), HashMap::new());
        let mut r = rules();
        r.hauling_fuel_cost = 100.0;
        let rows = vec![
            price_row(
                &r,
                &Overrides::default(),
                &market(&p, &npc),
                &tritanium(),
                1000,
            ),
            Row::rejected(
                None,
                "Nothing",
                5,
                Note {
                    tone: Tone::Danger,
                    text: String::new(),
                },
            ),
        ];
        let t = totals(&r, &rows, 10.0);
        assert_eq!(t.raw, 4000.0);
        assert_eq!(t.after_tax, 3600.0);
        assert_eq!(t.taxes, 400.0);
        assert_eq!(t.donation, 360.0);
        assert!((t.volume - 10.0).abs() < 1e-9);
        assert!((t.hauling - 1000.0).abs() < 1e-9);
        assert!((t.net - 2240.0).abs() < 1e-6);
    }
}
