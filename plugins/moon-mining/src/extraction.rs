//! One extraction's details, as aa-moonmining's modal: where and when, what
//! the chunk holds (estimated from the moon's survey) and, for holders of
//! `view_moon_ledgers`, who mined it.

use chrono::{DateTime, Utc};
use tether_plugin_sdk::identity::Viewer;
use tether_plugin_sdk::storage::{self, Value as Db};
use tether_plugin_sdk::{
    Card, Column, Page, PageError, Section, Table, Value, character, corporation, isk, item_type,
    link, time,
};

use crate::{
    LEDGER_WINDOW, PRICE, Which, count, extractions, failed, float, int, isk_or_blank, planner,
    refinery, rfc3339, text, value, with_rows,
};

pub fn page(viewer: &Viewer, structure_id: i64, at: i64) -> Result<Page, PageError> {
    let arrival = DateTime::<Utc>::from_timestamp(at, 0).ok_or(PageError::NotFound)?;
    let now = Utc::now();
    let pop = extractions(
        Which::One,
        &[structure_id.into(), Db::timestamp(rfc3339(arrival))],
    )?
    .into_iter()
    .next()
    .ok_or(PageError::NotFound)?;
    let owner = storage::query(
        "SELECT e.corporation_id, coalesce(n.name, 'Corporation ' || e.corporation_id::text) \
         FROM extractions e LEFT JOIN names n ON n.id = e.corporation_id \
         WHERE e.structure_id = $1 AND e.chunk_arrival = $2",
        &[structure_id.into(), Db::timestamp(rfc3339(arrival))],
    )
    .map_err(|e| failed("reading the extraction", e))?;
    let owner = owner.rows.first();
    let volume = pop.volume();
    let mut card = Card::new("Extraction")
        .field(
            "Moon",
            link(pop.moon.clone(), format!("moon/{}", pop.moon_id)),
        )
        .field("System", pop.system.clone())
        .field("Refinery", refinery(&pop.structure, pop.structure_type));
    if let Some(r) = owner {
        card = card.field("Owner", corporation(int(r, 0), text(r, 1)));
    }
    card = card
        .field("Status", pop.status(now))
        .field("Started", time(rfc3339(pop.start)))
        .field("Chunk arrival", time(rfc3339(pop.arrival)))
        .field("Auto-fracture", time(rfc3339(pop.decay)));
    if let Some(cancelled) = pop.cancelled {
        card = card.field("Cancelled", time(rfc3339(cancelled)));
    }
    card = card
        .field("Duration", planner::duration_text(pop.arrival - pop.start))
        .field("Volume (est.)", format!("{} m³", thousands(volume)))
        .field("Value (est.)", isk_or_blank(pop.value()))
        .field("Mined", isk_or_blank(pop.mined));
    let products = storage::query(
        &format!(
            "SELECT p.type_id, coalesce(n.name, 'Type ' || p.type_id::text), coalesce(t.rarity, 0), \
                    p.amount, {PRICE}::float8, pr.type_id IS NOT NULL \
             FROM survey_products p LEFT JOIN names n ON n.id = p.type_id \
             LEFT JOIN ore_types t ON t.type_id = p.type_id \
             LEFT JOIN prices pr ON pr.type_id = p.type_id \
             WHERE p.moon_id = $1 ORDER BY p.amount DESC"
        ),
        &[pop.moon_id.into()],
    )
    .map_err(|e| failed("reading the moon's survey", e))?;
    let products_table = with_rows(
        Table::new(vec![
            Column::text("Ore"),
            Column::text("Rarity"),
            Column::numeric("Share"),
            Column::numeric("Unit price (est.)"),
            Column::numeric("Volume (m³)"),
            Column::numeric("Units"),
            Column::numeric("Total (est.)"),
        ])
        .title("Products (est.)")
        .empty("No survey of this moon: upload one to see what the chunk holds."),
        products.rows.iter().map(|r| {
            let share = float(r, 3).unwrap_or_default();
            let price = float(r, 4).unwrap_or_default();
            let priced = r.get(5).and_then(Db::as_bool).unwrap_or(false);
            let (ore, units, worth) = value::ore_in_chunk(volume, share, price);
            vec![
                item_type(int(r, 0), text(r, 1)).into(),
                value::rarity(int(r, 2)).into(),
                value::percent(share).into(),
                isk_or_blank(priced.then_some(price)),
                count(ore),
                count(units),
                isk_or_blank(priced.then_some(worth)),
            ]
        }),
    );
    let mut page = Page::new(format!("Extraction at {}", pop.moon))
        .description(
            "The chunk's ores are estimated from the moon's survey and the drill's hourly yield, \
             at CCP's average ore prices.",
        )
        .card(card)
        .table(products_table);
    if viewer.can("view_moon_ledgers") {
        let (ledger, totals) = ledger_tables(structure_id, arrival)?;
        page = page
            .tab("Ledger", vec![Section::Table(ledger)])
            .tab("Totals by character", vec![Section::Table(totals)]);
    }
    Ok(page)
}

/// 12,345,678.
fn thousands(n: f64) -> String {
    let n = if n.is_finite() { n.round() as i64 } else { 0 };
    let digits = n.abs().to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    if n < 0 { format!("-{out}") } else { out }
}

/// The extraction's mining ledger: every day's row, and per character.
fn ledger_tables(structure_id: i64, arrival: DateTime<Utc>) -> Result<(Table, Table), PageError> {
    let params = [structure_id.into(), Db::timestamp(rfc3339(arrival))];
    let rows = storage::query(
        &format!(
            "SELECT l.day, l.character_id, coalesce(c.name, 'Character ' || l.character_id::text), \
                    l.corporation_id, coalesce(o.name, 'Corporation ' || l.corporation_id::text), \
                    l.type_id, coalesce(t.name, 'Type ' || l.type_id::text), l.quantity, {PRICE}::float8 \
             FROM extractions e JOIN ledger l ON {LEDGER_WINDOW} \
             LEFT JOIN names c ON c.id = l.character_id \
             LEFT JOIN names o ON o.id = l.corporation_id \
             LEFT JOIN names t ON t.id = l.type_id \
             LEFT JOIN prices pr ON pr.type_id = l.type_id \
             WHERE e.structure_id = $1 AND e.chunk_arrival = $2 \
             ORDER BY l.day DESC, l.quantity DESC LIMIT 500"
        ),
        &params,
    )
    .map_err(|e| failed("reading the ledger", e))?;
    let ledger = with_rows(
        Table::new(vec![
            Column::numeric("Day"),
            Column::text("Character"),
            Column::text("Corporation"),
            Column::text("Ore"),
            Column::numeric("Quantity"),
            Column::numeric("Volume (m³)"),
            Column::numeric("Unit price"),
            Column::numeric("Total"),
        ])
        .title("Mining ledger")
        .empty("Nothing mined from this chunk yet, or the observer hasn't been read."),
        rows.rows.iter().map(|r| {
            let quantity = int(r, 7);
            let price = float(r, 8).unwrap_or_default();
            vec![
                text(r, 0).into(),
                character(int(r, 1), text(r, 2)).into(),
                corporation(int(r, 3), text(r, 4)).into(),
                item_type(int(r, 5), text(r, 6)).into(),
                quantity.into(),
                count(quantity as f64 * value::ORE_VOLUME),
                isk(value::finite(price)),
                isk(value::finite(quantity as f64 * price)),
            ]
        }),
    );
    let per = storage::query(
        &format!(
            "SELECT x.character_id, coalesce(c.name, 'Character ' || x.character_id::text), \
                    x.corp, coalesce(o.name, 'Corporation ' || x.corp::text), x.units, x.isk \
             FROM (SELECT l.character_id, (array_agg(l.corporation_id ORDER BY l.day DESC))[1] AS corp, \
                          sum(l.quantity)::bigint AS units, sum(l.quantity * {PRICE})::float8 AS isk \
                   FROM extractions e JOIN ledger l ON {LEDGER_WINDOW} \
                   LEFT JOIN prices pr ON pr.type_id = l.type_id \
                   WHERE e.structure_id = $1 AND e.chunk_arrival = $2 \
                   GROUP BY l.character_id) x \
             LEFT JOIN names c ON c.id = x.character_id \
             LEFT JOIN names o ON o.id = x.corp \
             ORDER BY x.isk DESC NULLS LAST, x.units DESC LIMIT 500"
        ),
        &params,
    )
    .map_err(|e| failed("reading the ledger", e))?;
    let all_units: i64 = per.rows.iter().map(|r| int(r, 4)).sum();
    let all_isk: f64 = per.rows.iter().filter_map(|r| float(r, 5)).sum();
    let totals = with_rows(
        Table::new(vec![
            Column::text("Character"),
            Column::text("Corporation"),
            Column::numeric("Volume (m³)"),
            Column::numeric("Value"),
            Column::numeric("% volume"),
            Column::numeric("% value"),
        ])
        .title("Totals by character")
        .empty("Nothing mined from this chunk yet."),
        per.rows.iter().map(|r| {
            let (units, worth) = (int(r, 4), float(r, 5).unwrap_or_default());
            let row: Vec<Value> = vec![
                character(int(r, 0), text(r, 1)).into(),
                corporation(int(r, 2), text(r, 3)).into(),
                count(units as f64 * value::ORE_VOLUME),
                isk(value::finite(worth)),
                value::percent(units as f64 / all_units.max(1) as f64).into(),
                value::percent(if all_isk > 0.0 { worth / all_isk } else { 0.0 }).into(),
            ];
            row
        }),
    );
    Ok((ledger, totals))
}

#[cfg(test)]
mod tests {
    use super::thousands;

    #[test]
    fn thousands_are_grouped() {
        assert_eq!(thousands(6_242_600.4), "6,242,600");
        assert_eq!(thousands(999.0), "999");
        assert_eq!(thousands(-1234.0), "-1,234");
        assert_eq!(thousands(f64::NAN), "0");
    }
}
