//! The calculator (aa-buybackprogram `views/calculate.py`,
//! `get_tracking_number`): a pasted inventory priced by a program's
//! rules, then a tracking number and how to make the contract. A
//! calculation is kept (with its tracking number) only when it priced
//! something (B1: not on every page load).

use std::collections::HashMap;
use std::hash::{BuildHasher, Hasher};

use tether_plugin_sdk::storage::{self, Statement, Value as Db};
use tether_plugin_sdk::{
    Card, Column, Field, Form, Page, PageError, Stat, Submission, SubmitResult, Table, Value,
    badge, item_type,
};

use crate::paste;
use crate::pricing::{self, Market, Note, Row, Tone, Totals};
use crate::programs::{self, Program};
use crate::statics;
use crate::{Access, Settings, failed, isk_text, settings};

/// Most lines a paste is priced for (a big hangar, in a few calls).
const MAX_LINES: usize = 2000;

/// The program's terms, in AA's words (`program_settings.py`).
pub fn terms(p: &Program, settings: &Settings, locations: &[String]) -> Card {
    let mut card = Card::new("This program");
    card = card.field(
        "Items",
        if p.allow_all_items {
            "This program accepts all types of items."
        } else {
            "Only the items listed under Special prices are accepted."
        },
    );
    card = card.field(
        "Prices",
        format!(
            "Based on {} {} prices{}",
            settings.price_source_name,
            p.price_type,
            if settings.instant_prices {
                " (instant prices)"
            } else {
                " (top 5% average)"
            }
        ),
    );
    card = card.field("Tax", format!("{}%", p.tax));
    if p.hauling_fuel_cost > 0 {
        card = card.field(
            "Freight",
            format!(
                "Items have an added freight cost of {} ISK per m³",
                p.hauling_fuel_cost
            ),
        );
    }
    if p.density_modifier {
        card = card.field(
            "Price density",
            format!(
                "Items with price density below {} ISK/m³ have an additional {}% tax on them.",
                p.density_threshold, p.density_tax
            ),
        );
    }
    let mut ore = Vec::new();
    if p.use_raw_ore_value {
        ore.push("raw".to_owned());
    }
    if p.use_refined_value {
        ore.push(format!("refined at {}%", p.refining_rate));
    }
    if p.use_compressed_value {
        ore.push("compressed".to_owned());
    }
    if !ore.is_empty() {
        card = card.field(
            "Ore",
            format!("Valued {}: the best price is used.", ore.join(", ")),
        );
    }
    if p.blue_loot_npc_price || p.red_loot_npc_price || p.ope_npc_price || p.bonds_npc_price {
        card = card.field(
            "NPC prices",
            format!(
                "Some items use ESI's average price instead of {} prices.",
                settings.price_source_name
            ),
        );
    }
    if p.use_t1_scrap {
        card = card.field(
            "T1 scrap",
            format!(
                "Uses T1 scrap reprocessing value (meta 0-4) at {}% yield",
                p.t1_refining_rate
            ),
        );
    }
    if p.allow_unpacked_items {
        card = card.field("Unpacked items", "Accepted.");
    }
    if p.is_public {
        card = card.field("Open to", "Every pilot who can log in.");
    }
    card.field("Locations", locations.join(", "))
}

/// The calculator page for a program.
pub fn page(access: &Access, program_id: i64) -> Result<Page, PageError> {
    let program = visible_program(access, program_id)?;
    let settings = settings().map_err(|e| failed("reading settings", e))?;
    let locations = location_names(program.id)?;
    let owner = owner_name(&program);
    Ok(Page::new(format!("Sell to {}", program.display_name()))
        .description(format!("Managed by {owner}."))
        .card(terms(&program, &settings, &locations))
        .form(form()))
}

fn form() -> Form {
    Form::new("calculate", "Calculate")
        .title("Your items")
        .field(
            Field::textarea("items", "Items", 200_000)
                .required()
                .help("Copy and paste the item data from your inventory. Item types not in this buyback program will be ignored."),
        )
        .field(
            Field::number("donation", "Donation %")
                .value("0")
                .range(Some(0.0), Some(100.0), true),
        )
        .field(Field::text("notes", "Additional notes", 500))
}

pub(crate) fn visible_program(access: &Access, program_id: i64) -> Result<Program, PageError> {
    let program = programs::get(program_id)
        .map_err(|e| failed("reading the program", e))?
        .ok_or(PageError::NotFound)?;
    if !program.visible_to(access) {
        return Err(PageError::NotFound);
    }
    Ok(program)
}

pub(crate) fn location_names(program_id: i64) -> Result<Vec<String>, PageError> {
    let locations = programs::program_locations(program_id, false)
        .map_err(|e| failed("reading locations", e))?;
    let systems: Vec<i64> = locations.iter().filter_map(|l| l.system_id).collect();
    let names: HashMap<i64, String> = statics::systems(&systems)
        .unwrap_or_default()
        .into_iter()
        .map(|s| (s.id, s.name))
        .collect();
    Ok(locations
        .iter()
        .map(|l| l.display(l.system_id.and_then(|s| names.get(&s)).map(String::as_str)))
        .collect())
}

/// Who contracts go to: the manager's corporation or character.
pub(crate) fn owner_name(program: &Program) -> String {
    let id = if program.is_corporation {
        program.owner_corporation
    } else {
        program.owner_character
    };
    tether_plugin_sdk::esi::names(&[id])
        .ok()
        .and_then(|n| n.into_iter().find(|n| n.id == id))
        .map_or_else(|| id.to_string(), |n| n.name)
}

/// A priced paste.
pub struct Calculation {
    pub rows: Vec<Row>,
    pub totals: Totals,
}

/// Prices a paste by the program's rules (AA's view checks, then
/// `get_item_prices` and `get_item_values` per line).
pub fn calculate(
    program: &Program,
    settings: &Settings,
    lines: &[paste::Line],
    donation: f64,
) -> Result<Calculation, String> {
    let names: Vec<String> = lines.iter().map(|l| l.name.clone()).collect();
    let types = statics::by_names(&names).map_err(|e| format!("reading item data: {e:?}"))?;
    let overrides =
        programs::overrides(program.id).map_err(|e| format!("reading the program: {e:?}"))?;
    let rules = program.rules();
    let found: Vec<&statics::TypeInfo> = types.values().collect();
    let ids: Vec<i64> = found.iter().map(|t| t.id).collect();
    let compressed_ids: Vec<i64> = found.iter().filter_map(|t| t.compressed_type_id).collect();
    let compressed =
        statics::by_ids(&compressed_ids).map_err(|e| format!("reading item data: {e:?}"))?;
    let materials = statics::materials(&ids).map_err(|e| format!("reading item data: {e:?}"))?;
    let mut wanted: Vec<i64> = ids.clone();
    wanted.extend(&compressed_ids);
    wanted.extend(materials.values().flatten().map(|(m, _)| *m));
    let prices = crate::prices::get(&wanted, true)?;
    // ESI's averages only for programs using NPC prices; without them,
    // those items say their price is missing rather than failing all.
    let wants_npc = rules.blue_loot_npc_price
        || rules.red_loot_npc_price
        || rules.ope_npc_price
        || rules.bonds_npc_price;
    let npc = if wants_npc {
        crate::prices::npc(&ids).unwrap_or_else(|why| {
            tether_plugin_sdk::log::warn(format!("NPC prices weren't read: {why}"));
            HashMap::new()
        })
    } else {
        HashMap::new()
    };
    let market = Market {
        prices: &prices,
        npc: &npc,
        source_name: &settings.price_source_name,
        age_warning_hours: settings.price_age_warning_hours as f64,
    };
    let rows: Vec<Row> = lines
        .iter()
        .map(|line| {
            let reject = |tone, text: String| Row::rejected(None, &line.name, line.quantity, Note { tone, text });
            let Some(info) = types.get(&line.name) else {
                return reject(
                    Tone::Danger,
                    format!(
                        "{} not found from database. It is most likely a new item still not added to database or a renamed unpacked item.",
                        line.name
                    ),
                );
            };
            let item = statics::item(info, &materials, &compressed);
            if overrides.entry(&item).is_some_and(|e| e.disallow) {
                return Row::rejected(
                    Some(info.id),
                    &info.name,
                    line.quantity,
                    Note {
                        tone: Tone::Danger,
                        text: format!("{} is explicitly forbidden in this buyback program.", info.name),
                    },
                );
            }
            if info.category_name == "Blueprint" {
                return Row::rejected(
                    Some(info.id),
                    &info.name,
                    line.quantity,
                    Note {
                        tone: Tone::Warning,
                        text: format!("{} belongs to category Blueprint. Blueprints are not accepted.", info.name),
                    },
                );
            }
            if line.unpacked && !rules.allow_unpacked_items {
                return Row::rejected(
                    Some(info.id),
                    &info.name,
                    line.quantity,
                    Note {
                        tone: Tone::Danger,
                        text: format!(
                            "Unpacked items are not allowed at this location. Repack {} to get a price for it",
                            info.name
                        ),
                    },
                );
            }
            pricing::price_row(&rules, &overrides, &market, &item, line.quantity)
        })
        .collect();
    let totals = pricing::totals(&rules, &rows, donation);
    Ok(Calculation { rows, totals })
}

/// Six random hex digits (AA's uuid4 slice), from WASI's random source.
fn hex6() -> String {
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u64(chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default() as u64);
    format!("{:06X}", h.finish() & 0x00FF_FFFF)
}

/// `<prefill>-<the latest tracking's id, or 0>-<6 hex>` (AA's).
pub(crate) fn tracking_number(prefill: &str, table: &str, reverse: bool) -> Result<String, String> {
    let latest = storage::query(&format!("SELECT coalesce(max(id), 0) FROM {table}"), &[])
        .map_err(|e| format!("numbering: {e:?}"))?
        .rows
        .first()
        .map_or(0, |r| crate::int(r, 0));
    let middle = if reverse {
        format!("R-{latest}")
    } else {
        latest.to_string()
    };
    let mut number = format!("{prefill}-{middle}-{}", hex6());
    number.truncate(32);
    Ok(number)
}

/// Posts a calculation: priced, kept with a tracking number unless the
/// Settings refuse pastes with anything not accepted, and shown.
pub fn submit(access: &Access, program_id: i64, s: &Submission) -> Result<SubmitResult, PageError> {
    let program = visible_program(access, program_id)?;
    let settings = settings().map_err(|e| failed("reading settings", e))?;
    let items = s.value("items");
    let donation = s
        .value("donation")
        .parse::<f64>()
        .unwrap_or(0.0)
        .clamp(0.0, 100.0);
    let notes = s.value("notes").trim().to_owned();
    let locations = location_names(program.id)?;
    let owner = owner_name(&program);
    let base = Page::new(format!("Sell to {}", program.display_name()))
        .description(format!("Managed by {owner}."));
    if !paste::is_inventory_paste(items) {
        return Ok(SubmitResult::Page(
            base.card(
                Card::new("Copy items from your inventory")
                    .description("Buyback calculator only accepts copy pasted item formats from ingame. To calculate a price copy the items from your inventory."),
            )
            .card(terms(&program, &settings, &locations))
            .form(form()),
        ));
    }
    let mut lines = paste::lines(items);
    lines.truncate(MAX_LINES);
    let calc =
        calculate(&program, &settings, &lines, donation).map_err(|e| failed("pricing", e))?;
    let priced = calc.rows.iter().any(|r| !r.rejected);
    let blocked = settings.disallow_any_disallowed && calc.rows.iter().any(|r| r.rejected);
    let mut page = base;
    if blocked {
        page = page.card(
            Card::new("Calculation Failed")
                .description("Prohibited or unrecognized items detected. Please remove the highlighted items and try again."),
        );
    } else if priced {
        let number = keep(access, &program, &calc, donation, &notes, &settings)
            .map_err(|e| failed("keeping the calculation", e))?;
        page = page
            .stats(summary(&owner, &calc.totals, &number))
            .card(instructions(
                &owner,
                &calc.totals,
                &number,
                &program.expiration,
            ));
    }
    if !blocked {
        page = page.card(invoice(
            &calc.totals,
            &settings,
            &program,
            &locations,
            donation,
        ));
    }
    Ok(SubmitResult::Page(
        page.table(rows_table(&calc.rows, &settings.price_source_name))
            .form(form()),
    ))
}

/// Keeps the calculation: its tracking number and the priced rows (the
/// contract must hold exactly those, B30).
fn keep(
    access: &Access,
    program: &Program,
    calc: &Calculation,
    donation: f64,
    notes: &str,
    settings: &Settings,
) -> Result<String, String> {
    let number = tracking_number(
        &program.prefill(&settings.tracking_prefill),
        "trackings",
        false,
    )?;
    let t = &calc.totals;
    let items: Vec<serde_json::Value> = calc
        .rows
        .iter()
        .filter(|r| !r.rejected)
        .filter_map(|r| {
            Some(serde_json::json!({
                "type_id": r.type_id?, "quantity": r.quantity, "buy_value": r.unit_value,
            }))
        })
        .collect();
    let character = tether_plugin_sdk::identity::acting().map_or(access.viewer.main.id, |c| c.id);
    storage::transaction(&[
        Statement::new(
            "INSERT INTO trackings (program_id, issuer_account, issuer_character, value, taxes, \
                 hauling_cost, donation, net_price, total_volume, tracking_number, notes) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, nullif($11, ''))",
            vec![
                program.id.into(),
                access.account().into(),
                character.into(),
                t.raw.into(),
                t.taxes.into(),
                t.hauling.into(),
                (if donation > 0.0 { t.donation } else { 0.0 }).into(),
                t.net.into(),
                t.volume.into(),
                number.clone().into(),
                notes.to_owned().into(),
            ],
        ),
        Statement::new(
            "INSERT INTO tracking_items (tracking_id, type_id, quantity, buy_value) \
             SELECT (SELECT id FROM trackings WHERE tracking_number = $2), type_id, quantity, buy_value \
             FROM jsonb_to_recordset($1::jsonb) AS x(type_id bigint, quantity bigint, buy_value numeric)",
            vec![
                Db::json(serde_json::Value::Array(items).to_string()),
                number.clone().into(),
            ],
        ),
    ])
    .map_err(|e| format!("{e:?}"))?;
    Ok(number)
}

pub(crate) fn summary(owner: &str, t: &Totals, number: &str) -> Vec<Stat> {
    vec![
        Stat::new("Availability", owner.to_owned()),
        Stat::new("I will receive", Value::Isk(t.net.max(0.0))),
        Stat::new("Tracking number", number.to_owned()),
    ]
}

/// AA's "How to create the contract".
pub(crate) fn instructions(owner: &str, t: &Totals, number: &str, expiration: &str) -> Card {
    let receive = if t.net < 0.0 {
        format!("0 ISK (the calculation came to {} ISK)", isk_text(t.net))
    } else {
        format!("{} ISK", isk_text(t.net))
    };
    Card::new("How to create the contract")
        .field("1", "Open contracts in game and click Create Contract")
        .field("2", format!("Select Item Exchange and search for {owner}"))
        .field("3", "Add all items from your inventory to the contract")
        .field(
            "4",
            format!("On the price page set I will receive to {receive}"),
        )
        .field(
            "5",
            format!("Set the contract description to your tracking number: {number}"),
        )
        .field("6", format!("Set expiration to {expiration} and submit"))
}

pub(crate) fn invoice(
    t: &Totals,
    settings: &Settings,
    program: &Program,
    locations: &[String],
    donation: f64,
) -> Card {
    let mut card = Card::new("Invoice")
        .field(
            "Prices based on",
            format!(
                "{} {} price",
                settings.price_source_name, program.price_type
            ),
        )
        .field("Price before expenses", Value::Isk(t.raw))
        .field("Program taxes", Value::Isk(t.taxes))
        .field("Total volume", format!("{} m³", isk_text(t.volume)));
    if program.hauling_fuel_cost > 0 {
        card = card.field(
            "Hauling cost",
            format!(
                "{} ISK @ {} ISK / m³",
                isk_text(t.hauling),
                program.hauling_fuel_cost
            ),
        );
    }
    if donation > 0.0 {
        card = card.field(
            "Donation",
            format!("{} ISK @ {donation} %", isk_text(t.donation)),
        );
    }
    card.field("Net price", Value::Isk(t.net.max(0.0)))
        .field("Accepted at", locations.join(", "))
}

fn tone(t: Tone) -> tether_plugin_sdk::Tone {
    match t {
        Tone::Danger => tether_plugin_sdk::Tone::Danger,
        Tone::Warning => tether_plugin_sdk::Tone::Warning,
        Tone::Success => tether_plugin_sdk::Tone::Success,
        Tone::Info | Tone::Watch => tether_plugin_sdk::Tone::Neutral,
    }
}

/// The item details: each row with its prices and notes.
pub(crate) fn rows_table(rows: &[Row], source: &str) -> Table {
    let mut table = Table::new(vec![
        Column::text("Item"),
        Column::numeric("Quantity"),
        Column::numeric(format!("{source} buy / sell")),
        Column::numeric("Base price"),
        Column::numeric("Our taxes"),
        Column::numeric("Our price"),
        Column::numeric("Row total"),
        Column::text("Notes"),
    ])
    .title("Item details");
    for r in rows {
        let name: Value = match r.type_id {
            Some(id) => item_type(id, r.name.clone()).into(),
            None => r.name.clone().into(),
        };
        let notes = r
            .notes
            .iter()
            .map(|n| n.text.as_str())
            .collect::<Vec<_>>()
            .join(" · ");
        let first = r
            .notes
            .first()
            .map_or(tether_plugin_sdk::Tone::Neutral, |n| tone(n.tone));
        table = table.row(vec![
            name,
            Value::Number(r.quantity),
            format!("{} / {}", isk_text(r.market.buy), isk_text(r.market.sell)).into(),
            Value::Isk(r.raw_value),
            format!("{}%", r.tax_percent).into(),
            Value::Isk(r.unit_value),
            Value::Isk(r.buy_value),
            if notes.is_empty() {
                "".into()
            } else if r.rejected {
                badge(notes, tether_plugin_sdk::Tone::Danger).into()
            } else {
                badge(notes, first).into()
            },
        ]);
        if r.variants.len() > 1 {
            for v in &r.variants {
                table = table.row(vec![
                    format!("  {} value", v.label).into(),
                    "".into(),
                    "".into(),
                    Value::Isk(v.raw_value),
                    format!("{}%", v.tax_percent).into(),
                    Value::Isk(v.unit_value),
                    Value::Isk(v.value),
                    if Some(v.label) == r.winner {
                        badge("Best price", tether_plugin_sdk::Tone::Success).into()
                    } else {
                        "".into()
                    },
                ]);
            }
        }
    }
    table.empty("No items.")
}
