//! aa-moonmining's Reports (`reports_access`): owned moons' potential
//! monthly income, members' mining over the last four months, who
//! uploaded surveys, and ore prices.
//!
//! Member mining is per character: apps never learn which characters
//! share an account, so there are no mains here.

use chrono::{Datelike, NaiveDate, Utc};
use tether_plugin_sdk::identity::Viewer;
use tether_plugin_sdk::storage;
use tether_plugin_sdk::{
    Column, Page, PageError, Section, Stat, Table, Value, character, corporation, isk, item_type,
    link, time,
};

use crate::{
    PRICE, count, failed, float, int, isk_or_blank, rfc3339, text, value, when, with_rows,
};

/// Months in Member mining, this one included.
const MONTHS: usize = 4;

/// The first day of this month and the months before it, newest first.
pub fn month_starts(today: NaiveDate, months: usize) -> Vec<NaiveDate> {
    let mut starts = Vec::with_capacity(months);
    let (mut year, mut month) = (today.year(), today.month());
    for _ in 0..months {
        if let Some(start) = NaiveDate::from_ymd_opt(year, month, 1) {
            starts.push(start);
        }
        if month == 1 {
            year -= 1;
            month = 12;
        } else {
            month -= 1;
        }
    }
    starts
}

pub fn page(viewer: &Viewer) -> Result<Page, PageError> {
    if !viewer.can("reports_access") {
        return Err(PageError::Forbidden);
    }
    let (income, total, moons) = income_table()?;
    Ok(Page::new("Reports")
        .description(
            "Values at CCP's average price of each ore, read daily from ESI; mining from the \
             refineries' mining ledgers.",
        )
        .stats(vec![
            Stat::new("Owned moons", moons),
            Stat::new("Potential income / month", isk(value::finite(total)))
                .caption("of the surveyed ones"),
        ])
        .tab("Potential monthly income", vec![Section::Table(income)])
        .tab("Member mining", vec![Section::Table(mining_table()?)])
        .tab("Member uploads", vec![Section::Table(uploads_table()?)])
        .tab("Ore prices", vec![Section::Table(prices_table()?)]))
}

/// Owned moons ranked by what a month of mining them is worth.
fn income_table() -> Result<(Table, f64, i64), PageError> {
    let rates = crate::rates()?;
    let rows = storage::query(
        &format!(
            "WITH owned AS (SELECT DISTINCT ON (e.moon_id) e.moon_id, e.corporation_id \
                 FROM extractions e ORDER BY e.moon_id, e.chunk_arrival DESC) \
             SELECT o.moon_id, coalesce(mn.name, 'Moon ' || o.moon_id::text), o.corporation_id, \
                    coalesce(co.name, 'Corporation ' || o.corporation_id::text), coalesce(rn.name, ''), \
                    coalesce(v.rarity, 0), v.worth \
             FROM owned o \
             LEFT JOIN (SELECT p.moon_id, sum(p.amount * {PRICE})::float8 AS worth, max(t.rarity) AS rarity \
                   FROM survey_products p LEFT JOIN prices pr ON pr.type_id = p.type_id \
                   LEFT JOIN ore_types t ON t.type_id = p.type_id GROUP BY p.moon_id) v ON v.moon_id = o.moon_id \
             LEFT JOIN moons m ON m.moon_id = o.moon_id \
             LEFT JOIN systems y ON y.system_id = m.system_id \
             LEFT JOIN names mn ON mn.id = o.moon_id \
             LEFT JOIN names co ON co.id = o.corporation_id \
             LEFT JOIN names rn ON rn.id = y.region_id \
             ORDER BY v.worth DESC NULLS LAST, 2 LIMIT 500"
        ),
        &[],
    )
    .map_err(|e| failed("reading moons", e))?;
    let values: Vec<Option<f64>> = rows
        .rows
        .iter()
        .map(|r| float(r, 6).map(|w| rates.monthly(w)))
        .collect();
    let total: f64 = values.iter().flatten().sum();
    let table = with_rows(
        Table::new(vec![
            Column::numeric("Rank"),
            Column::text("Moon"),
            Column::text("Corporation"),
            Column::text("Region"),
            Column::text("Rarity"),
            Column::numeric("Value / month (est.)"),
            Column::numeric("% of total"),
        ])
        .title("Potential monthly income")
        .empty("No owned moons yet: they come from the refineries' extractions."),
        rows.rows
            .iter()
            .zip(&values)
            .enumerate()
            .map(|(i, (r, v))| {
                vec![
                    if v.is_some() {
                        Value::Number(i64::try_from(i + 1).unwrap_or(i64::MAX))
                    } else {
                        "".into()
                    },
                    link(text(r, 1), format!("moon/{}", int(r, 0))).into(),
                    corporation(int(r, 2), text(r, 3)).into(),
                    text(r, 4).into(),
                    value::rarity(int(r, 5)).into(),
                    isk_or_blank(*v),
                    v.map_or_else(
                        || "not surveyed".to_owned(),
                        |v| value::percent(if total > 0.0 { v / total } else { 0.0 }),
                    )
                    .into(),
                ]
            }),
    );
    Ok((table, total, i64::try_from(rows.rows.len()).unwrap_or(0)))
}

/// Each character's mining, in m³ and ISK, month by month.
fn mining_table() -> Result<Table, PageError> {
    let starts = month_starts(Utc::now().date_naive(), MONTHS);
    let params: Vec<storage::Value> = starts.iter().map(|d| d.to_string().into()).collect();
    if params.len() != MONTHS {
        return Err(PageError::Failed("working out the months".into()));
    }
    // $1 is this month's first day, $4 the oldest month's.
    let windows = [
        "l.day >= $1::date",
        "l.day >= $2::date AND l.day < $1::date",
        "l.day >= $3::date AND l.day < $2::date",
        "l.day >= $4::date AND l.day < $3::date",
    ];
    let units: Vec<String> = windows
        .iter()
        .map(|w| format!("coalesce(sum(l.quantity) FILTER (WHERE {w}), 0)::bigint"))
        .collect();
    let isks: Vec<String> = windows
        .iter()
        .map(|w| format!("coalesce(sum(l.quantity * {PRICE}) FILTER (WHERE {w}), 0)::float8"))
        .collect();
    let rows = storage::query(
        &format!(
            "SELECT x.character_id, coalesce(n.name, 'Character ' || x.character_id::text), x.corp, \
                    coalesce(c.name, 'Corporation ' || x.corp::text), \
                    x.u0, x.v0, x.u1, x.v1, x.u2, x.v2, x.u3, x.v3 \
             FROM (SELECT l.character_id, (array_agg(l.corporation_id ORDER BY l.day DESC))[1] AS corp, \
                          {} AS u0, {} AS v0, {} AS u1, {} AS v1, {} AS u2, {} AS v2, {} AS u3, {} AS v3 \
                   FROM ledger l LEFT JOIN prices pr ON pr.type_id = l.type_id \
                   WHERE l.day >= $4::date GROUP BY l.character_id) x \
             LEFT JOIN names n ON n.id = x.character_id \
             LEFT JOIN names c ON c.id = x.corp \
             ORDER BY x.v0 + x.v1 + x.v2 + x.v3 DESC, 2 LIMIT 500",
            units[0], isks[0], units[1], isks[1], units[2], isks[2], units[3], isks[3]
        ),
        &params,
    )
    .map_err(|e| failed("reading the ledger", e))?;
    let mut columns = vec![Column::text("Character"), Column::text("Corporation")];
    for start in &starts {
        let month = start.format("%b %Y");
        columns.push(Column::numeric(format!("{month} m³")));
        columns.push(Column::numeric(format!("{month} ISK")));
    }
    Ok(with_rows(
        Table::new(columns)
            .title("Member mining, last four months")
            .empty("Nothing mined in the last four months, or no observers readable."),
        rows.rows.iter().map(|r| {
            let mut row: Vec<Value> = vec![
                character(int(r, 0), text(r, 1)).into(),
                corporation(int(r, 2), text(r, 3)).into(),
            ];
            for m in 0..MONTHS {
                row.push(count(int(r, 4 + 2 * m) as f64 * value::ORE_VOLUME));
                row.push(isk(value::finite(float(r, 5 + 2 * m).unwrap_or_default())));
            }
            row
        }),
    ))
}

/// Who uploaded surveys: the moons whose latest survey is theirs.
fn uploads_table() -> Result<Table, PageError> {
    let rows = storage::query(
        "SELECT (array_agg(character_id ORDER BY uploaded_at DESC))[1], \
                (array_agg(character_name ORDER BY uploaded_at DESC))[1], count(*), max(uploaded_at) \
         FROM surveys GROUP BY account_id ORDER BY 3 DESC, 4 DESC LIMIT 500",
        &[],
    )
    .map_err(|e| failed("reading surveys", e))?;
    Ok(with_rows(
        Table::new(vec![
            Column::text("Uploaded by"),
            Column::numeric("Moons"),
            Column::numeric("Last upload"),
        ])
        .title("Member uploads")
        .empty("No moon surveys uploaded yet."),
        rows.rows.iter().map(|r| {
            vec![
                character(int(r, 0), text(r, 1)).into(),
                int(r, 2).into(),
                when(r, 3).map_or_else(|| "".into(), |t| time(rfc3339(t))),
            ]
        }),
    ))
}

/// Moon ores' prices, rarest first.
fn prices_table() -> Result<Table, PageError> {
    let rows = storage::query(
        "SELECT t.type_id, coalesce(n.name, 'Type ' || t.type_id::text), t.rarity, \
                p.average_price, p.adjusted_price, p.updated_at \
         FROM ore_types t LEFT JOIN prices p ON p.type_id = t.type_id \
         LEFT JOIN names n ON n.id = t.type_id \
         WHERE p.type_id IS NOT NULL \
         ORDER BY t.rarity DESC, 2 LIMIT 500",
        &[],
    )
    .map_err(|e| failed("reading prices", e))?;
    Ok(with_rows(
        Table::new(vec![
            Column::text("Ore"),
            Column::text("Rarity"),
            Column::numeric("Average price"),
            Column::numeric("Adjusted price"),
            Column::numeric("Updated"),
        ])
        .title("Ore prices")
        .empty("No prices yet: they're read from ESI once a day."),
        rows.rows.iter().map(|r| {
            vec![
                item_type(int(r, 0), text(r, 1)).into(),
                value::rarity(int(r, 2)).into(),
                isk_or_blank(float(r, 3)),
                isk_or_blank(float(r, 4)),
                when(r, 5).map_or_else(|| "".into(), |t| time(rfc3339(t))),
            ]
        }),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn months_step_back_across_the_year() {
        let d = |y, m, day| NaiveDate::from_ymd_opt(y, m, day).unwrap();
        assert_eq!(
            month_starts(d(2026, 2, 17), 4),
            vec![d(2026, 2, 1), d(2026, 1, 1), d(2025, 12, 1), d(2025, 11, 1)]
        );
    }
}
