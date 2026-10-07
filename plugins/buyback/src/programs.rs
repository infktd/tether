//! Programs, locations, special taxes, static prices, the watchlist and
//! the FAQ: what managers set up (aa-buybackprogram `views/programs.py`,
//! `views/special_taxes.py`), read here for every page.

use tether_plugin_sdk::storage::{self, Value as Db};

use crate::pricing::{Overrides, PriceType, ProgramItem, Rules};
use crate::{Access, Restrictions, boolean, float, int, opt_int, opt_text, text};

/// A buyback program.
#[derive(Debug, Clone)]
pub struct Program {
    pub id: i64,
    pub name: String,
    pub tracking_prefill: String,
    pub owner_character: i64,
    pub owner_corporation: i64,
    pub manager_account: i64,
    pub is_corporation: bool,
    pub expiration: String,
    pub price_type: String,
    pub tax: i64,
    pub hauling_fuel_cost: i64,
    pub density_modifier: bool,
    pub compression_density_modifier: bool,
    pub density_threshold: i64,
    pub density_tax: i64,
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
    pub restricted_groups: Vec<i64>,
    pub restricted_states: Vec<String>,
    pub is_public: bool,
    pub notify_manager: bool,
    pub discord_show_item_list: bool,
    pub discord_channel: Option<String>,
    pub wallet_division: Option<i64>,
}

const COLUMNS: &str = "id, name, tracking_prefill, owner_character, owner_corporation, \
    manager_account, is_corporation, expiration, price_type, tax, hauling_fuel_cost, \
    density_modifier, compression_density_modifier, density_threshold, density_tax, \
    allow_all_items, use_refined_value, use_compressed_value, use_raw_ore_value, \
    allow_unpacked_items, refining_rate::float8, use_t1_scrap, t1_refining_rate::float8, \
    blue_loot_npc_price, red_loot_npc_price, ope_npc_price, bonds_npc_price, \
    to_jsonb(restricted_groups)::text, to_jsonb(restricted_states)::text, is_public, \
    notify_manager, discord_show_item_list, discord_channel, wallet_division";

fn json_list<T: serde::de::DeserializeOwned>(row: &[Db], i: usize) -> Vec<T> {
    row.get(i)
        .and_then(Db::as_text)
        .and_then(|t| serde_json::from_str(t).ok())
        .unwrap_or_default()
}

fn program(r: &[Db]) -> Program {
    Program {
        id: int(r, 0),
        name: text(r, 1),
        tracking_prefill: text(r, 2),
        owner_character: int(r, 3),
        owner_corporation: int(r, 4),
        manager_account: int(r, 5),
        is_corporation: boolean(r, 6),
        expiration: text(r, 7),
        price_type: text(r, 8),
        tax: int(r, 9),
        hauling_fuel_cost: int(r, 10),
        density_modifier: boolean(r, 11),
        compression_density_modifier: boolean(r, 12),
        density_threshold: int(r, 13),
        density_tax: int(r, 14),
        allow_all_items: boolean(r, 15),
        use_refined_value: boolean(r, 16),
        use_compressed_value: boolean(r, 17),
        use_raw_ore_value: boolean(r, 18),
        allow_unpacked_items: boolean(r, 19),
        refining_rate: float(r, 20),
        use_t1_scrap: boolean(r, 21),
        t1_refining_rate: float(r, 22),
        blue_loot_npc_price: boolean(r, 23),
        red_loot_npc_price: boolean(r, 24),
        ope_npc_price: boolean(r, 25),
        bonds_npc_price: boolean(r, 26),
        restricted_groups: json_list(r, 27),
        restricted_states: json_list(r, 28),
        is_public: boolean(r, 29),
        notify_manager: boolean(r, 30),
        discord_show_item_list: boolean(r, 31),
        discord_channel: opt_text(r, 32),
        wallet_division: opt_int(r, 33),
    }
}

pub fn get(id: i64) -> Result<Option<Program>, storage::Error> {
    Ok(storage::query(
        &format!("SELECT {COLUMNS} FROM programs WHERE id = $1"),
        &[id.into()],
    )?
    .rows
    .first()
    .map(|r| program(r)))
}

pub fn all() -> Result<Vec<Program>, storage::Error> {
    Ok(storage::query(
        &format!("SELECT {COLUMNS} FROM programs ORDER BY name, id"),
        &[],
    )?
    .rows
    .iter()
    .map(|r| program(r))
    .collect())
}

impl Program {
    pub fn display_name(&self) -> String {
        if self.name.is_empty() {
            "Unnamed Program".to_owned()
        } else {
            self.name.clone()
        }
    }

    pub fn restrictions(&self) -> Restrictions {
        Restrictions {
            is_public: self.is_public,
            manager_account: self.manager_account,
            groups: self.restricted_groups.clone(),
            states: self.restricted_states.clone(),
        }
    }

    pub fn visible_to(&self, access: &Access) -> bool {
        access.may_use(&self.restrictions())
    }

    pub fn editable_by(&self, access: &Access) -> bool {
        access.manages(self.manager_account, self.owner_character)
    }

    /// The tracking prefix: the program's, else the Settings'.
    pub fn prefill(&self, global: &str) -> String {
        if self.tracking_prefill.trim().is_empty() {
            global.trim().to_owned()
        } else {
            self.tracking_prefill.trim().to_owned()
        }
    }

    pub fn rules(&self) -> Rules {
        Rules {
            tax: self.tax as f64,
            hauling_fuel_cost: self.hauling_fuel_cost as f64,
            density_modifier: self.density_modifier,
            compression_density_modifier: self.compression_density_modifier,
            density_threshold: self.density_threshold as f64,
            density_tax: self.density_tax as f64,
            allow_all_items: self.allow_all_items,
            use_refined_value: self.use_refined_value,
            use_compressed_value: self.use_compressed_value,
            use_raw_ore_value: self.use_raw_ore_value,
            allow_unpacked_items: self.allow_unpacked_items,
            refining_rate: self.refining_rate,
            use_t1_scrap: self.use_t1_scrap,
            t1_refining_rate: self.t1_refining_rate,
            blue_loot_npc_price: self.blue_loot_npc_price,
            red_loot_npc_price: self.red_loot_npc_price,
            ope_npc_price: self.ope_npc_price,
            bonds_npc_price: self.bonds_npc_price,
            price_type: PriceType::parse(&self.price_type),
        }
    }
}

/// A program's special taxes, static prices and watchlist.
pub fn overrides(program_id: i64) -> Result<Overrides, storage::Error> {
    let items = storage::query(
        "SELECT type_id, market_group_id, item_tax, disallow_item FROM program_items \
         WHERE program_id = $1",
        &[program_id.into()],
    )?
    .rows
    .iter()
    .map(|r| ProgramItem {
        type_id: opt_int(r, 0),
        market_group_id: opt_int(r, 1),
        item_tax: int(r, 2) as f64,
        disallow: boolean(r, 3),
    })
    .collect();
    let static_prices = storage::query(
        "SELECT type_id, price::float8 FROM static_prices WHERE program_id = $1",
        &[program_id.into()],
    )?
    .rows
    .iter()
    .map(|r| (int(r, 0), float(r, 1)))
    .collect();
    let watch = storage::query(
        "SELECT type_id, group_id FROM watchlist WHERE program_id = $1",
        &[program_id.into()],
    )?;
    Ok(Overrides {
        items,
        static_prices,
        watch_types: watch.rows.iter().filter_map(|r| opt_int(r, 0)).collect(),
        watch_groups: watch.rows.iter().filter_map(|r| opt_int(r, 1)).collect(),
    })
}

/// A place contracts are accepted at.
#[derive(Debug, Clone)]
pub struct Location {
    pub id: i64,
    pub owner_character: i64,
    pub name: String,
    pub system_id: Option<i64>,
    pub structure_id: Option<i64>,
    pub created_by: i64,
}

fn location(r: &[Db]) -> Location {
    Location {
        id: int(r, 0),
        owner_character: int(r, 1),
        name: text(r, 2),
        system_id: opt_int(r, 3),
        structure_id: opt_int(r, 4),
        created_by: int(r, 5),
    }
}

pub fn locations() -> Result<Vec<Location>, storage::Error> {
    Ok(storage::query(
        "SELECT id, owner_character, name, system_id, structure_id, created_by FROM locations \
         ORDER BY name, id",
        &[],
    )?
    .rows
    .iter()
    .map(|r| location(r))
    .collect())
}

/// A program's locations (`reverse`: a reverse program's).
pub fn program_locations(program_id: i64, reverse: bool) -> Result<Vec<Location>, storage::Error> {
    let table = if reverse {
        "reverse_program_locations"
    } else {
        "program_locations"
    };
    Ok(storage::query(
        &format!(
            "SELECT l.id, l.owner_character, l.name, l.system_id, l.structure_id, l.created_by \
             FROM locations l JOIN {table} p ON p.location_id = l.id \
             WHERE p.program_id = $1 ORDER BY l.name, l.id"
        ),
        &[program_id.into()],
    )?
    .rows
    .iter()
    .map(|r| location(r))
    .collect())
}

impl Location {
    /// AA's `location_display_name`: "<system>: <name>", or the name.
    pub fn display(&self, system: Option<&str>) -> String {
        match system {
            Some(s) => format!("{s}: {}", self.name),
            None => self.name.clone(),
        }
    }
}
