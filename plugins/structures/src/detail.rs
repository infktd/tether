//! A structure's page (aa-structures' details): what it is and where, its
//! fuel, state and timers, a customs office's taxes and access, a
//! starbase's fuel bay, the fitting (with `view_structure_fit`), and its
//! tags (managers change them here).

use chrono::Utc;
use tether_plugin_sdk::identity::Viewer;
use tether_plugin_sdk::storage::{self, Value as Db};
use tether_plugin_sdk::{Card, Column, Page, PageError, Table, Value, badge, link, time};

use crate::orbitals::METENOX_GAS_PER_HOUR;
use crate::tags;
use crate::{
    VISIBLE, failed, int, kind_label, left, opt_int, rfc3339, services_text, state_badge, text,
    visibility, when, with_rows,
};

/// A structure the viewer may see, or not found.
pub fn visible_structure(viewer: &Viewer, id: i64) -> Result<bool, PageError> {
    let Some(mut params) = visibility(viewer) else {
        return Ok(false);
    };
    params.push(id.into());
    let rows = storage::query(
        &format!("SELECT 1 FROM structures s WHERE {VISIBLE} AND s.structure_id = $4"),
        &params,
    )
    .map_err(|e| failed("reading a structure", e))?;
    Ok(!rows.rows.is_empty())
}

fn slot_group(flag: &str) -> Option<(&'static str, u8)> {
    for (prefix, label) in [
        ("HiSlot", "High slots"),
        ("MedSlot", "Medium slots"),
        ("LoSlot", "Low slots"),
        ("RigSlot", "Rigs"),
        ("ServiceSlot", "Services"),
    ] {
        if let Some(n) = flag.strip_prefix(prefix) {
            return Some((label, n.parse().unwrap_or(0)));
        }
    }
    None
}

fn percent(value: &serde_json::Value) -> Value {
    value
        .as_f64()
        .map_or_else(|| "".into(), |v| format!("{:.1}%", v * 100.0).into())
}

pub fn page(viewer: &Viewer, id: i64) -> Result<Page, PageError> {
    if !visible_structure(viewer, id)? {
        return Err(PageError::NotFound);
    }
    let rows = storage::query(
        "SELECT s.name, s.kind, coalesce(t.name, 'Type ' || s.type_id::text), \
             coalesce(y.name, sn.name, 'System ' || s.system_id::text), y.security_status, \
             coalesce(r.name, ''), coalesce(o.name, 'Corporation ' || s.corporation_id::text), \
             s.fuel_expires, s.state, s.state_timer_end, s.unanchors_at, s.reinforce_hour, \
             s.services::text, coalesce(m.name, ''), coalesce(s.planet_name, p.name, ''), \
             s.details::text, s.fuel_blocks, s.strontium, s.magmatic_gas, s.gas_expires, \
             s.has_core, s.fuel_read_at, s.onlined_since, s.corporation_id \
         FROM structures s LEFT JOIN names t ON t.id = s.type_id \
         LEFT JOIN systems y ON y.system_id = s.system_id LEFT JOIN names sn ON sn.id = s.system_id \
         LEFT JOIN names r ON r.id = y.region_id LEFT JOIN names o ON o.id = s.corporation_id \
         LEFT JOIN names m ON m.id = s.moon_id LEFT JOIN names p ON p.id = s.planet_id \
         WHERE s.structure_id = $1",
        &[id.into()],
    )
    .map_err(|e| failed("reading a structure", e))?;
    let row = rows.rows.first().ok_or(PageError::NotFound)?;
    let now = Utc::now();
    let kind = text(row, 1);
    let system = match row.get(4).and_then(Db::as_float) {
        Some(sec) => format!("{} ({sec:.1})", text(row, 3)),
        None => text(row, 3),
    };
    let mut general = Card::new("General")
        .field(
            "Owner",
            link(text(row, 6), format!("owner/{}", int(row, 23))),
        )
        .field("Kind", kind_label(&kind))
        .field("Type", text(row, 2))
        .field("System", system)
        .field("Region", text(row, 5));
    match kind.as_str() {
        "starbase" if !text(row, 13).is_empty() => general = general.field("Moon", text(row, 13)),
        "customs_office" | "skyhook" if !text(row, 14).is_empty() => {
            general = general.field("Planet", text(row, 14));
        }
        _ => {}
    }
    if kind != "customs_office" && kind != "skyhook" {
        general = general.field("State", state_badge(&text(row, 8)));
    }
    if let Some(t) = when(row, 9) {
        general = general.field("State timer", time(rfc3339(t)));
    }
    if let Some(t) = when(row, 10) {
        general = general.field("Unanchors", time(rfc3339(t)));
    }
    if let Some(t) = when(row, 22) {
        general = general.field("Online since", time(rfc3339(t)));
    }
    if kind == "upwell" {
        if let Some(h) = opt_int(row, 11) {
            general = general.field("Reinforce hour", format!("{h:02}:00"));
        }
        general = general
            .field("Services", services_text(&text(row, 12)))
            .field(
                "Quantum core",
                match row.get(20).and_then(Db::as_bool) {
                    Some(true) => "Installed",
                    Some(false) => "None",
                    None => "Unknown (the owner's assets aren't read)",
                },
            );
    }
    let mut fuel = Card::new("Fuel");
    let mut has_fuel = false;
    if let Some(t) = when(row, 7) {
        fuel = fuel
            .field("Fuel expires", time(rfc3339(t)))
            .field("Fuel left", left(t - now));
        has_fuel = true;
    }
    if let Some(n) = opt_int(row, 16) {
        fuel = fuel.field("Fuel blocks", n);
        has_fuel = true;
    }
    if let Some(n) = opt_int(row, 17).filter(|_| kind == "starbase") {
        fuel = fuel.field("Strontium", n);
    }
    if let Some(gas) = opt_int(row, 18).filter(|_| when(row, 19).is_some()) {
        fuel = fuel.field("Magmatic gas", gas).field(
            "Gas lasts",
            when(row, 19).map_or_else(|| "".to_owned(), |t| left(t - now)),
        );
        fuel = fuel.field("Gas burn", format!("{METENOX_GAS_PER_HOUR:.0} an hour"));
    }
    if let Some(t) = when(row, 21).filter(|_| has_fuel) {
        fuel = fuel.field("Fuel bay read", time(rfc3339(t)));
    }
    let mut page = Page::new(text(row, 0))
        .description(format!("{} in {}", kind_label(&kind), text(row, 3)))
        .card(general);
    if has_fuel {
        page = page.card(fuel);
    }
    let details: serde_json::Value = serde_json::from_str(&text(row, 15)).unwrap_or_default();
    if kind == "customs_office" {
        let window = match (
            details["reinforce_exit_start"].as_i64(),
            details["reinforce_exit_end"].as_i64(),
        ) {
            (Some(a), Some(b)) => format!("{a:02}:00 to {b:02}:00"),
            _ => String::new(),
        };
        let yes = |v: &serde_json::Value| {
            if v.as_bool().unwrap_or(false) {
                "Yes"
            } else {
                "No"
            }
        };
        page = page.card(
            Card::new("Taxes and access")
                .field("Reinforcement exit", window)
                .field("Corporation tax", percent(&details["corporation_tax_rate"]))
                .field("Alliance access", yes(&details["allow_alliance_access"]))
                .field("Alliance tax", percent(&details["alliance_tax_rate"]))
                .field(
                    "Access by standing",
                    yes(&details["allow_access_with_standings"]),
                )
                .field(
                    "Lowest standing allowed",
                    details["standing_level"].as_str().unwrap_or("").to_owned(),
                )
                .field(
                    "Excellent standing tax",
                    percent(&details["excellent_standing_tax_rate"]),
                )
                .field(
                    "Good standing tax",
                    percent(&details["good_standing_tax_rate"]),
                )
                .field(
                    "Neutral standing tax",
                    percent(&details["neutral_standing_tax_rate"]),
                )
                .field(
                    "Bad standing tax",
                    percent(&details["bad_standing_tax_rate"]),
                )
                .field(
                    "Terrible standing tax",
                    percent(&details["terrible_standing_tax_rate"]),
                ),
        );
    }
    if kind == "starbase" {
        let fuels: Vec<(i64, i64)> = details["fuels"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|f| Some((f["type_id"].as_i64()?, f["quantity"].as_i64()?)))
                    .collect()
            })
            .unwrap_or_default();
        let ids: Vec<i64> = fuels.iter().map(|(t, _)| *t).collect();
        let names = crate::names_for(&ids).map_err(|e| failed("reading names", e))?;
        page = page.table(with_rows(
            Table::new(vec![Column::text("Fuel"), Column::numeric("Quantity")])
                .title("Fuel bay")
                .empty("Not read yet (the owner needs the Director role)."),
            fuels.iter().map(|(t, q)| {
                vec![
                    names
                        .iter()
                        .find(|(i, _)| i == t)
                        .map_or_else(|| format!("Type {t}"), |(_, n)| n.clone())
                        .into(),
                    (*q).into(),
                ]
            }),
        ));
    }
    if kind == "upwell" {
        page = fitting(page, viewer, id)?;
    }
    if kind == "skyhook" {
        page = page.text(
            "ESI tells little about Orbital Skyhooks: no name, state, reagents or timers (only \
             their notifications do). Skyhooks burn no fuel.",
        );
    }
    let all_tags = tags::all().map_err(|e| failed("reading tags", e))?;
    let on = storage::query(
        "SELECT tag_id FROM structure_tags WHERE structure_id = $1",
        &[id.into()],
    )
    .map_err(|e| failed("reading tags", e))?;
    let on: Vec<i64> = on.rows.iter().map(|r| int(r, 0)).collect();
    let mut tag_card = Card::new("Tags");
    for tag in all_tags.iter().filter(|t| on.contains(&t.id)) {
        let label = if tag.description.is_empty() {
            "Tag".to_owned()
        } else {
            tag.description.clone()
        };
        tag_card = tag_card.field(label, tag.badge());
    }
    if on.is_empty() {
        tag_card = tag_card.field("Tags", "None");
    }
    page = page.card(tag_card);
    if viewer.can("manage")
        && let Some(form) = tags::structure_form(&all_tags, &on)
    {
        page = page.form(form);
    }
    Ok(page.card(Card::new("Structures").field("Back", link("Every structure", ""))))
}

/// The fitting (aa-structures' fit view): modules by slot, fighters, the
/// fuel bay, the quantum core and the moon material bay.
fn fitting(page: Page, viewer: &Viewer, id: i64) -> Result<Page, PageError> {
    if !viewer.can("view_structure_fit") {
        return Ok(page);
    }
    let items = storage::query(
        "SELECT i.flag, coalesce(n.name, 'Type ' || i.type_id::text), i.quantity \
         FROM structure_items i LEFT JOIN names n ON n.id = i.type_id \
         WHERE i.structure_id = $1 ORDER BY i.flag, 2",
        &[id.into()],
    )
    .map_err(|e| failed("reading the fitting", e))?;
    let read = storage::query(
        "SELECT fuel_read_at FROM structures WHERE structure_id = $1",
        &[id.into()],
    )
    .map_err(|e| failed("reading the fitting", e))?;
    let read_at = read.rows.first().and_then(|r| when(r, 0));
    let Some(read_at) = read_at else {
        return Ok(page.text(
            "Fitting: not read yet. It comes from the owner's corporation assets, which need the \
             owner character's Director role.",
        ));
    };
    let mut modules: Vec<(&'static str, u8, String)> = Vec::new();
    let mut bays: Vec<(&'static str, String, i64)> = Vec::new();
    for row in &items.rows {
        let (flag, name, quantity) = (text(row, 0), text(row, 1), int(row, 2));
        if let Some((group, slot)) = slot_group(&flag) {
            modules.push((group, slot, name));
            continue;
        }
        let bay = match flag.as_str() {
            "FighterBay" => "Fighter bay",
            f if f.starts_with("FighterTube") => "Fighter tubes",
            "StructureFuel" => "Fuel bay",
            "QuantumCoreRoom" => "Quantum core",
            "MoonMaterialBay" => "Moon material bay",
            _ => continue,
        };
        bays.push((bay, name, quantity));
    }
    let order = [
        "High slots",
        "Medium slots",
        "Low slots",
        "Rigs",
        "Services",
    ];
    modules.sort_by_key(|(g, s, _)| (order.iter().position(|o| o == g), *s));
    let page = page.table(with_rows(
        Table::new(vec![
            Column::text("Slot"),
            Column::numeric("#"),
            Column::text("Module"),
        ])
        .title("Fitting")
        .empty("Nothing fitted."),
        modules
            .into_iter()
            .map(|(g, s, n)| vec![g.into(), i64::from(s).into(), n.into()]),
    ));
    let page = page.table(with_rows(
        Table::new(vec![
            Column::text("Bay"),
            Column::text("Item"),
            Column::numeric("Quantity"),
        ])
        .title("Bays")
        .empty("Nothing in its bays."),
        bays.into_iter()
            .map(|(b, n, q)| vec![b.into(), n.into(), q.into()]),
    ));
    Ok(page.card(Card::new("Assets").field("Read", time(rfc3339(read_at)))))
}

/// A value's badge, for the list's Core column.
pub fn core_badge(has_core: Option<bool>) -> Value {
    match has_core {
        Some(true) => badge("Yes", tether_plugin_sdk::Tone::Success).into(),
        Some(false) => badge("No", tether_plugin_sdk::Tone::Warning).into(),
        None => "Unknown".into(),
    }
}
