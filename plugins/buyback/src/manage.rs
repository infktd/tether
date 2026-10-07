//! What managers set up (aa-buybackprogram's Manage menu): programs,
//! locations, a program's special taxes, static prices and watchlist,
//! and the FAQ; Refresh contracts and Force price update.

use std::collections::HashMap;

use tether_plugin_sdk::esi::{self, Character};
use tether_plugin_sdk::jobs::{self, NewJob};
use tether_plugin_sdk::storage::{self, Statement, Value as Db};
use tether_plugin_sdk::{
    Card, Column, Field, Form, Page, PageError, Request, SettingsForm, SettingsGroup, Submission,
    SubmitResult, Table, Tone, Value, action, actions, add_owner, discord, identity, item_type,
    link, log,
};

use crate::pages::id_of;
use crate::programs::{self, Location, Program};
use crate::{Access, failed, int, opt_int, statics, text};

pub fn render(access: &Access, request: &Request) -> Result<Page, PageError> {
    let parts: Vec<&str> = request.path.split('/').collect();
    match parts.as_slice() {
        ["manage"] => list(access),
        ["manage", "program", "new"] => editor(access, None, request),
        ["manage", "program", id] => editor(access, Some(id_of(id)?), request),
        ["manage", "locations"] => locations_page(access),
        ["manage", "faq"] => faq_page(access),
        _ => Err(PageError::NotFound),
    }
}

pub fn submit(access: &Access, s: &Submission) -> Result<SubmitResult, PageError> {
    let parts: Vec<&str> = s.request.path.split('/').collect();
    match (parts.as_slice(), s.form.as_str()) {
        (["manage"], "delete_program") => delete_program(access, s),
        (["manage"], "refresh_contracts") => queue(access, "contracts", "manage"),
        (["manage"], "update_prices") => queue(access, "prices", "manage"),
        (["manage", "program", "new"], "program") => save_program(access, None, s),
        (["manage", "program", id], "program") => save_program(access, Some(id_of(id)?), s),
        (["manage", "locations"], "location") => add_location(access, s),
        (["manage", "locations"], "remove_location") => remove_location(access, s),
        (["manage", "faq"], "faq") => add_faq(access, s),
        (["manage", "faq"], "remove_faq") => remove_faq(access, s),
        _ => Err(PageError::NotFound),
    }
}

/// The data sources the viewer may make a program's manager: their own
/// characters among them (all of them for manage_all).
fn owners(access: &Access) -> Vec<Character> {
    let mine = access.character_ids();
    esi::data_sources()
        .into_iter()
        .filter(|c| access.manage_all() || mine.contains(&c.id))
        .collect()
}

fn list(access: &Access) -> Result<Page, PageError> {
    let programs: Vec<Program> = programs::all()
        .map_err(|e| failed("reading programs", e))?
        .into_iter()
        .filter(|p| p.editable_by(access))
        .collect();
    let names = crate::pages::owner_names(&programs);
    let mut table = Table::new(vec![
        Column::text("Program"),
        Column::text("Manager"),
        Column::numeric("Tax"),
        Column::numeric("Contracts"),
        Column::text(""),
    ]);
    for p in &programs {
        let owner = if p.is_corporation {
            p.owner_corporation
        } else {
            p.owner_character
        };
        table = table.row(vec![
            link(p.display_name(), format!("manage/program/{}", p.id)).into(),
            names
                .get(&owner)
                .cloned()
                .unwrap_or_else(|| owner.to_string())
                .into(),
            format!("{}%", p.tax).into(),
            Value::Number(crate::pages::count(
                "SELECT count(*) FROM trackings WHERE program_id = $1 AND contract_id IS NOT NULL",
                p.id,
            )),
            crate::pages::program_actions(p),
        ]);
    }
    let mut page = Page::new("Manage programs").description(
        "Each program's manager is a character of yours added as a data source (aa-buybackprogram's Setup Manager). Then add the locations contracts are accepted at, and create programs.",
    );
    if owners(access).is_empty() {
        page = page.card(
            Card::new("1. Add yourself as a manager")
                .description("Log in with the character whose contracts (or whose corporation's) the programs take. It needs the in-game roles for what you use: Accountant for wallets, Director for hangars and stock.")
                .field("", add_owner("Add a manager")),
        );
    }
    Ok(page
        .table(
            table
                .title("Your programs")
                .empty("No programs yet: create one."),
        )
        .card(Card::new("Background updates").field(
            "",
            actions(vec![
                action("Refresh contracts", "refresh_contracts"),
                action("Force price update", "update_prices"),
            ]),
        )))
}

fn queue(access: &Access, job: &str, back: &str) -> Result<SubmitResult, PageError> {
    if !access.manager() {
        return Err(PageError::Forbidden);
    }
    jobs::enqueue(NewJob::new(job).key(job)).map_err(|e| failed("queuing", e))?;
    log::info(format!(
        "{job} queued by {} ({})",
        access.viewer.main.name, access.viewer.main.id
    ));
    Ok(SubmitResult::Redirect(back.to_owned()))
}

fn delete_program(access: &Access, s: &Submission) -> Result<SubmitResult, PageError> {
    let program = programs::get(id_of(s.value("program"))?)
        .map_err(|e| failed("reading the program", e))?
        .ok_or(PageError::NotFound)?;
    if !program.editable_by(access) {
        return Err(PageError::Forbidden);
    }
    storage::execute("DELETE FROM programs WHERE id = $1", &[program.id.into()])
        .map_err(|e| failed("deleting the program", e))?;
    log::info(format!(
        "program {} ({}) deleted by {} ({})",
        program.display_name(),
        program.id,
        access.viewer.main.name,
        access.viewer.main.id
    ));
    Ok(SubmitResult::Redirect("manage".to_owned()))
}

// ---- the program editor ----------------------------------------------------

const EXPIRATIONS: &[&str] = &["1 Day", "3 Days", "1 Week", "2 Weeks", "4 Weeks"];

fn choices(list: &[&str]) -> Vec<(String, String)> {
    list.iter()
        .map(|s| ((*s).to_owned(), (*s).to_owned()))
        .collect()
}

fn editor(access: &Access, id: Option<i64>, request: &Request) -> Result<Page, PageError> {
    let program = match id {
        Some(id) => {
            let p = programs::get(id)
                .map_err(|e| failed("reading the program", e))?
                .ok_or(PageError::NotFound)?;
            if !p.editable_by(access) {
                return Err(PageError::NotFound);
            }
            Some(p)
        }
        None => None,
    };
    let mut owners = owners(access);
    // A manager_all editor keeps the program's own manager on the list.
    if let Some(p) = &program
        && !owners.iter().any(|c| c.id == p.owner_character)
    {
        owners.extend(
            esi::data_sources()
                .into_iter()
                .filter(|c| c.id == p.owner_character),
        );
    }
    let title = program.as_ref().map_or_else(
        || "New program".to_owned(),
        |p| format!("Edit {}", p.display_name()),
    );
    if owners.is_empty() {
        return Ok(Page::new(title).card(
            Card::new("Add yourself as a manager first")
                .description("A program's manager is a character of yours added as a data source.")
                .field("", add_owner("Add a manager")),
        ));
    }
    let chosen_owner = program
        .as_ref()
        .map(|p| p.owner_character)
        .or_else(|| request.param("owner").parse().ok())
        .filter(|o| owners.iter().any(|c| c.id == *o))
        .unwrap_or(owners[0].id);
    let owner_corp = owners
        .iter()
        .find(|c| c.id == chosen_owner)
        .map_or(0, |c| c.corporation_id);
    let locations: Vec<Location> = programs::locations()
        .map_err(|e| failed("reading locations", e))?
        .into_iter()
        .filter(|l| {
            access.manage_all()
                || l.created_by == access.account()
                || l.owner_character == chosen_owner
        })
        .collect();
    let selected: Vec<i64> = match &program {
        Some(p) => programs::program_locations(p.id, false)
            .map_err(|e| failed("reading locations", e))?
            .iter()
            .map(|l| l.id)
            .collect(),
        None => Vec::new(),
    };
    let wallets = storage::query(
        "SELECT division, name FROM wallets WHERE corporation_id = $1 ORDER BY division",
        &[owner_corp.into()],
    )
    .map_err(|e| failed("reading wallets", e))?;
    let groups = identity::all_groups();
    let channels = discord::channels();
    let p = program.clone();
    let v = |f: &dyn Fn(&Program) -> String, default: &str| {
        p.as_ref().map_or_else(|| default.to_owned(), f)
    };
    let b = |f: &dyn Fn(&Program) -> bool, default: bool| p.as_ref().map_or(default, f);

    let mut general = SettingsGroup::new("Program")
        .field(Field::text("name", "Name", 64).value(v(&|p| p.name.clone(), "")))
        .field(
            Field::select(
                "owner",
                "Manager",
                owners
                    .iter()
                    .map(|c| (c.id.to_string(), c.name.clone()))
                    .collect(),
            )
            .value(chosen_owner.to_string())
            .help("Contracts go to this character, or its corporation."),
        )
        .field(Field::checkbox(
            "is_corporation",
            "Contracts go to the manager's corporation",
            b(&|p| p.is_corporation, false),
        ))
        .field(
            Field::text("tracking_prefill", "Tracking prefix", 16)
                .value(v(&|p| p.tracking_prefill.clone(), ""))
                .help("Empty: the Settings' prefix."),
        )
        .field(
            Field::select("expiration", "Contract expiration", choices(EXPIRATIONS))
                .value(v(&|p| p.expiration.clone(), "2 Weeks")),
        )
        .field(
            Field::select(
                "price_type",
                "Price type",
                choices(&["Buy", "Sell", "Split"]),
            )
            .value(v(&|p| p.price_type.clone(), "Buy")),
        )
        .field(number(
            "tax",
            "Default tax %",
            v(&|p| p.tax.to_string(), "0"),
            0.0,
            100.0,
            true,
        ))
        .field(number(
            "hauling_fuel_cost",
            "Freight cost (ISK per m³)",
            v(&|p| p.hauling_fuel_cost.to_string(), "0"),
            -1e9,
            1e9,
            true,
        ));
    let mut wallet_choices = vec![(String::new(), "None".to_owned())];
    wallet_choices.extend(
        wallets
            .rows
            .iter()
            .map(|r| (int(r, 0).to_string(), text(r, 1))),
    );
    general = general.field(
        Field::select("wallet_division", "Funding wallet", wallet_choices).value(v(
            &|p| p.wallet_division.map(|w| w.to_string()).unwrap_or_default(),
            "",
        )),
    );
    let pricing = SettingsGroup::new("Items and pricing")
        .field(
            Field::checkbox(
                "allow_all_items",
                "Accept all items",
                b(&|p| p.allow_all_items, true),
            )
            .help("Off: only the items listed under Special prices."),
        )
        .field(Field::checkbox(
            "allow_unpacked_items",
            "Accept unpacked items",
            b(&|p| p.allow_unpacked_items, false),
        ))
        .field(Field::checkbox(
            "density_modifier",
            "Price density modifier",
            b(&|p| p.density_modifier, false),
        ))
        .field(number(
            "density_threshold",
            "Price density threshold (ISK/m³)",
            v(&|p| p.density_threshold.to_string(), "0"),
            0.0,
            1e12,
            true,
        ))
        .field(number(
            "density_tax",
            "Price density tax %",
            v(&|p| p.density_tax.to_string(), "0"),
            0.0,
            100.0,
            true,
        ))
        .field(Field::checkbox(
            "use_t1_scrap",
            "T1 scrap value (meta 0-4)",
            b(&|p| p.use_t1_scrap, false),
        ))
        .field(number(
            "t1_refining_rate",
            "T1 scrap yield %",
            v(&|p| p.t1_refining_rate.to_string(), "50"),
            0.0,
            100.0,
            false,
        ));
    let ore = SettingsGroup::new("Ore")
        .field(Field::checkbox(
            "use_raw_ore_value",
            "Use raw value",
            b(&|p| p.use_raw_ore_value, true),
        ))
        .field(Field::checkbox(
            "use_refined_value",
            "Use refined value",
            b(&|p| p.use_refined_value, false),
        ))
        .field(number(
            "refining_rate",
            "Refining rate %",
            v(&|p| p.refining_rate.to_string(), "0"),
            0.0,
            100.0,
            false,
        ))
        .field(Field::checkbox(
            "use_compressed_value",
            "Use compressed value",
            b(&|p| p.use_compressed_value, false),
        ))
        .field(Field::checkbox(
            "compression_density_modifier",
            "Ore volume based on compressed volume",
            b(&|p| p.compression_density_modifier, false),
        ));
    let npc = SettingsGroup::new("NPC prices")
        .description("ESI's average price instead of the market's, for loot NPCs buy.")
        .field(Field::checkbox(
            "blue_loot_npc_price",
            "Blue loot",
            b(&|p| p.blue_loot_npc_price, false),
        ))
        .field(Field::checkbox(
            "red_loot_npc_price",
            "Red loot",
            b(&|p| p.red_loot_npc_price, false),
        ))
        .field(Field::checkbox(
            "ope_npc_price",
            "Overseer's personal effects",
            b(&|p| p.ope_npc_price, false),
        ))
        .field(Field::checkbox(
            "bonds_npc_price",
            "Bonds",
            b(&|p| p.bonds_npc_price, false),
        ));
    let mut places =
        SettingsGroup::new("Locations").description("Where contracts are accepted: at least one.");
    for l in locations.iter().take(40) {
        places = places.field(Field::checkbox(
            format!("loc_{}", l.id),
            l.name.clone(),
            selected.contains(&l.id),
        ));
    }
    if locations.is_empty() {
        places = places.field(Field::checkbox(
            "no_locations",
            "Add locations under Manage › Locations first",
            false,
        ));
    }
    let mut who = SettingsGroup::new("Who may use it")
        .field(Field::checkbox(
            "is_public",
            "Every pilot who can log in",
            b(&|p| p.is_public, false),
        ))
        .field(
            Field::text("restricted_states", "Only these states", 500)
                .value(v(&|p| p.restricted_states.join(", "), ""))
                .help("State names, comma-separated. Empty: any state."),
        );
    let restricted = p
        .as_ref()
        .map(|p| p.restricted_groups.clone())
        .unwrap_or_default();
    for g in groups.iter().take(60) {
        who = who.field(Field::checkbox(
            format!("group_{}", g.id),
            format!("Group: {}", g.name),
            restricted.contains(&g.id),
        ));
    }
    let mut channel_choices = vec![(String::new(), "Not posted".to_owned())];
    channel_choices.extend(
        channels
            .iter()
            .map(|c| (c.id.clone(), format!("#{}", c.name))),
    );
    let notices = SettingsGroup::new("Notices")
        .field(Field::checkbox(
            "notify_manager",
            "Tell the manager of new contracts",
            b(&|p| p.notify_manager, false),
        ))
        .field(
            Field::select("discord_channel", "Post new contracts to", channel_choices)
                .value(v(&|p| p.discord_channel.clone().unwrap_or_default(), ""))
                .help("One of the app's channels (Administration › Apps › Buyback)."),
        )
        .field(Field::checkbox(
            "discord_show_item_list",
            "List the items in the post",
            b(&|p| p.discord_show_item_list, false),
        ));
    Ok(Page::new(title).settings(
        SettingsForm::new("program")
            .group(general)
            .group(pricing)
            .group(ore)
            .group(npc)
            .group(places)
            .group(who)
            .group(notices),
    ))
}

fn number(name: &str, label: &str, value: String, min: f64, max: f64, integer: bool) -> Field {
    Field::number(name, label)
        .value(value)
        .range(Some(min), Some(max), integer)
}

fn save_program(
    access: &Access,
    id: Option<i64>,
    s: &Submission,
) -> Result<SubmitResult, PageError> {
    let existing = match id {
        Some(id) => {
            let p = programs::get(id)
                .map_err(|e| failed("reading the program", e))?
                .ok_or(PageError::NotFound)?;
            if !p.editable_by(access) {
                return Err(PageError::Forbidden);
            }
            Some(p)
        }
        None => None,
    };
    let owner_id: i64 = s
        .value("owner")
        .parse()
        .map_err(|_| PageError::Failed("pick a manager".into()))?;
    let owner = esi::data_sources()
        .into_iter()
        .find(|c| c.id == owner_id)
        .filter(|c| {
            access.manage_all()
                || access.character_ids().contains(&c.id)
                || existing.as_ref().is_some_and(|p| p.owner_character == c.id)
        })
        .ok_or_else(|| PageError::Failed("the manager must be one of your data sources".into()))?;
    let num = |name: &str| s.value(name).parse::<f64>().unwrap_or(0.0);
    let flag = |name: &str| s.checked(name);
    // AA's Program.clean().
    let problem = if flag("allow_all_items")
        && !(flag("use_refined_value") || flag("use_compressed_value") || flag("use_raw_ore_value"))
    {
        Some(
            "All items are allowed but not a single pricing method for ores is selected. Please use at least one pricing method for ores if all items is allowed.",
        )
    } else if flag("density_modifier") && num("density_tax") <= 0.0 {
        Some("Price density is used but value for price density tax is missing")
    } else if flag("density_modifier") && num("density_threshold") <= 0.0 {
        Some("Price density is used but value for price density threshold is missing")
    } else if flag("use_refined_value") && num("refining_rate") <= 0.0 {
        Some(
            "Refined value is used for ore pricing method but no refining rate is provided. Provide a refining rate to used with this pricing model.",
        )
    } else {
        None
    };
    if let Some(why) = problem {
        return Err(PageError::Failed(why.to_owned()));
    }
    let prefill = s.value("tracking_prefill").trim().to_owned();
    if prefill.len() > 16
        || !prefill
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    {
        return Err(PageError::Failed(
            "Only letters, numbers, dots, dashes and underscores are allowed.".into(),
        ));
    }
    let locations: Vec<i64> = s
        .values
        .iter()
        .filter(|(n, v)| n.starts_with("loc_") && v == "true")
        .filter_map(|(n, _)| n.trim_start_matches("loc_").parse().ok())
        .collect();
    if locations.is_empty() {
        return Err(PageError::Failed("pick at least one location".into()));
    }
    let groups: Vec<i64> = s
        .values
        .iter()
        .filter(|(n, v)| n.starts_with("group_") && v == "true")
        .filter_map(|(n, _)| n.trim_start_matches("group_").parse().ok())
        .collect();
    let states: Vec<String> = s
        .value("restricted_states")
        .split(',')
        .map(|x| x.trim().to_owned())
        .filter(|x| !x.is_empty())
        .collect();
    let channel = Some(s.value("discord_channel").to_owned())
        .filter(|c| discord::channels().iter().any(|a| &a.id == c));
    let wallet = s.value("wallet_division").parse::<i64>().ok();
    let manager_account = existing
        .as_ref()
        .map_or(access.account(), |p| p.manager_account);
    let pick = |name: &str, list: &[&str], default: &str| -> String {
        let v = s.value(name);
        if list.contains(&v) {
            v.to_owned()
        } else {
            default.to_owned()
        }
    };
    let values: Vec<Db> = vec![
        s.value("name")
            .trim()
            .chars()
            .take(64)
            .collect::<String>()
            .into(),
        prefill.into(),
        owner.id.into(),
        owner.corporation_id.into(),
        manager_account.into(),
        flag("is_corporation").into(),
        pick("expiration", EXPIRATIONS, "2 Weeks").into(),
        pick("price_type", &["Buy", "Sell", "Split"], "Buy").into(),
        (num("tax").clamp(0.0, 100.0) as i64).into(),
        (num("hauling_fuel_cost") as i64).into(),
        flag("density_modifier").into(),
        flag("compression_density_modifier").into(),
        (num("density_threshold").max(0.0) as i64).into(),
        (num("density_tax").clamp(0.0, 100.0) as i64).into(),
        flag("allow_all_items").into(),
        flag("use_refined_value").into(),
        flag("use_compressed_value").into(),
        flag("use_raw_ore_value").into(),
        flag("allow_unpacked_items").into(),
        num("refining_rate").clamp(0.0, 100.0).into(),
        flag("use_t1_scrap").into(),
        num("t1_refining_rate").clamp(0.0, 100.0).into(),
        flag("blue_loot_npc_price").into(),
        flag("red_loot_npc_price").into(),
        flag("ope_npc_price").into(),
        flag("bonds_npc_price").into(),
        Db::json(serde_json::to_string(&groups).unwrap_or_else(|_| "[]".into())),
        Db::json(serde_json::to_string(&states).unwrap_or_else(|_| "[]".into())),
        flag("is_public").into(),
        flag("notify_manager").into(),
        flag("discord_show_item_list").into(),
        channel.into(),
        wallet.into(),
    ];
    let columns = "name, tracking_prefill, owner_character, owner_corporation, manager_account, \
        is_corporation, expiration, price_type, tax, hauling_fuel_cost, density_modifier, \
        compression_density_modifier, density_threshold, density_tax, allow_all_items, \
        use_refined_value, use_compressed_value, use_raw_ore_value, allow_unpacked_items, \
        refining_rate, use_t1_scrap, t1_refining_rate, blue_loot_npc_price, red_loot_npc_price, \
        ope_npc_price, bonds_npc_price, restricted_groups, restricted_states, is_public, \
        notify_manager, discord_show_item_list, discord_channel, wallet_division";
    let placeholders: Vec<String> = (1..=values.len())
        .map(|i| match i {
            27 => format!("ARRAY(SELECT jsonb_array_elements_text(${i}::jsonb)::bigint)"),
            28 => format!("ARRAY(SELECT jsonb_array_elements_text(${i}::jsonb))"),
            _ => format!("${i}"),
        })
        .collect();
    let program_id = match id {
        Some(id) => {
            let sets: Vec<String> = columns
                .split(',')
                .map(str::trim)
                .zip(&placeholders)
                .map(|(c, p)| format!("{c} = {p}"))
                .collect();
            let mut params = values;
            params.push(id.into());
            storage::execute(
                &format!(
                    "UPDATE programs SET {} WHERE id = ${}",
                    sets.join(", "),
                    params.len()
                ),
                &params,
            )
            .map_err(|e| failed("saving the program", e))?;
            id
        }
        None => {
            let rows = storage::query(
                &format!(
                    "INSERT INTO programs ({columns}) VALUES ({}) RETURNING id",
                    placeholders.join(", ")
                ),
                &values,
            )
            .map_err(|e| failed("creating the program", e))?;
            rows.rows.first().map_or(0, |r| int(r, 0))
        }
    };
    storage::transaction(&[
        Statement::new(
            "DELETE FROM program_locations WHERE program_id = $1",
            vec![program_id.into()],
        ),
        Statement::new(
            "INSERT INTO program_locations (program_id, location_id) \
             SELECT $1, l.id FROM locations l \
             WHERE l.id IN (SELECT jsonb_array_elements_text($2::jsonb)::bigint)",
            vec![program_id.into(), crate::json_ids(&locations)],
        ),
    ])
    .map_err(|e| failed("saving locations", e))?;
    log::info(format!(
        "program {program_id} {} by {} ({})",
        if id.is_some() { "updated" } else { "created" },
        access.viewer.main.name,
        access.viewer.main.id
    ));
    Ok(SubmitResult::Redirect("manage".to_owned()))
}

// ---- locations -------------------------------------------------------------

fn locations_page(access: &Access) -> Result<Page, PageError> {
    let locations: Vec<Location> = programs::locations()
        .map_err(|e| failed("reading locations", e))?
        .into_iter()
        .filter(|l| access.manage_all() || l.created_by == access.account())
        .collect();
    let systems: Vec<i64> = locations.iter().filter_map(|l| l.system_id).collect();
    let names: HashMap<i64, String> = statics::systems(&systems)
        .unwrap_or_default()
        .into_iter()
        .map(|s| (s.id, s.name))
        .collect();
    let mut table = Table::new(vec![
        Column::text("Name"),
        Column::text("System"),
        Column::numeric("Station or structure id"),
        Column::text(""),
    ]);
    for l in &locations {
        table = table.row(vec![
            l.name.clone().into(),
            l.system_id
                .and_then(|s| names.get(&s).cloned())
                .unwrap_or_else(|| "Custom".to_owned())
                .into(),
            l.structure_id.map_or_else(|| "".into(), Value::Number),
            action("Delete", "remove_location")
                .field("location", l.id.to_string())
                .tone(Tone::Danger)
                .confirm("The location goes from every program that accepts contracts there.")
                .into(),
        ]);
    }
    let owners = owners(access);
    Ok(Page::new("Locations")
        .description("Where contracts are accepted. The station or structure id lets Tether check a contract was made there.")
        .table(table.empty("No locations yet."))
        .form(
            Form::new("location", "Add location")
                .title("Add a location")
                .field(
                    Field::select(
                        "owner",
                        "Manager",
                        owners.iter().map(|c| (c.id.to_string(), c.name.clone())).collect(),
                    )
                    .required(),
                )
                .field(
                    Field::text("name", "Name", 32)
                        .required()
                        .help("Station or structure name. It doesn't have to match the in-game name: \"Region: Forge\" is fine."),
                )
                .field(Field::text("system", "Solar system", 64).help("Optional, as written in game."))
                .field(Field::number("structure_id", "Station or structure id").range(Some(1.0), None, true)),
        ))
}

fn add_location(access: &Access, s: &Submission) -> Result<SubmitResult, PageError> {
    let owner: i64 = s
        .value("owner")
        .parse()
        .map_err(|_| PageError::Failed("pick a manager".into()))?;
    if !owners(access).iter().any(|c| c.id == owner) {
        return Err(PageError::Forbidden);
    }
    let name = s.value("name").trim().to_owned();
    if name.is_empty() || name.chars().count() > 32 {
        return Err(PageError::Failed("a name is 1 to 32 characters".into()));
    }
    let system = match s.value("system").trim() {
        "" => None,
        q => Some(
            statics::search_systems(q)
                .map_err(|e| failed("reading systems", e))?
                .into_iter()
                .find(|x| x.name.eq_ignore_ascii_case(q))
                .ok_or_else(|| PageError::Failed(format!("no solar system is called {q}")))?
                .id,
        ),
    };
    let structure = s
        .value("structure_id")
        .parse::<i64>()
        .ok()
        .filter(|n| *n > 0);
    storage::execute(
        "INSERT INTO locations (owner_character, name, system_id, structure_id, created_by) \
         VALUES ($1, $2, $3, $4, $5) ON CONFLICT DO NOTHING",
        &[
            owner.into(),
            name.into(),
            system.into(),
            structure.into(),
            access.account().into(),
        ],
    )
    .map_err(|e| failed("adding the location", e))?;
    Ok(SubmitResult::Redirect("manage/locations".to_owned()))
}

fn remove_location(access: &Access, s: &Submission) -> Result<SubmitResult, PageError> {
    let id = id_of(s.value("location"))?;
    let rows = storage::query(
        "SELECT created_by FROM locations WHERE id = $1",
        &[id.into()],
    )
    .map_err(|e| failed("reading the location", e))?;
    let creator = rows
        .rows
        .first()
        .map(|r| int(r, 0))
        .ok_or(PageError::NotFound)?;
    if !(access.manage_all() || creator == access.account()) {
        return Err(PageError::Failed(
            "You did not create this location and thus you can't delete it.".into(),
        ));
    }
    storage::execute("DELETE FROM locations WHERE id = $1", &[id.into()])
        .map_err(|e| failed("deleting the location", e))?;
    Ok(SubmitResult::Redirect("manage/locations".to_owned()))
}

// ---- the FAQ ---------------------------------------------------------------

fn faq_page(access: &Access) -> Result<Page, PageError> {
    if !access.manage_all() {
        return Err(PageError::NotFound);
    }
    let rows = storage::query(
        "SELECT id, header, body FROM faq ORDER BY position, id",
        &[],
    )
    .map_err(|e| failed("reading the FAQ", e))?;
    let mut table = Table::new(vec![
        Column::text("Question"),
        Column::text("Answer"),
        Column::text(""),
    ]);
    for r in &rows.rows {
        table = table.row(vec![
            text(r, 1).into(),
            text(r, 2).into(),
            action("Delete", "remove_faq")
                .field("faq", int(r, 0).to_string())
                .tone(Tone::Danger)
                .confirm("This question goes from the FAQ.")
                .into(),
        ]);
    }
    Ok(Page::new("FAQ")
        .description("Questions shown after the three every program's FAQ has (aa-buybackprogram's admin-only FAQ).")
        .table(table.empty("No questions of your own yet."))
        .form(
            Form::new("faq", "Add question")
                .field(Field::text("header", "Question", 1024).required())
                .field(Field::textarea("body", "Answer", 10_000).required()),
        ))
}

fn add_faq(access: &Access, s: &Submission) -> Result<SubmitResult, PageError> {
    if !access.manage_all() {
        return Err(PageError::Forbidden);
    }
    storage::execute(
        "INSERT INTO faq (header, body, position) VALUES ($1, $2, \
         (SELECT coalesce(max(position), 0) + 1 FROM faq))",
        &[
            s.value("header").trim().to_owned().into(),
            s.value("body").trim().to_owned().into(),
        ],
    )
    .map_err(|e| failed("adding the question", e))?;
    Ok(SubmitResult::Redirect("manage/faq".to_owned()))
}

fn remove_faq(access: &Access, s: &Submission) -> Result<SubmitResult, PageError> {
    if !access.manage_all() {
        return Err(PageError::Forbidden);
    }
    storage::execute(
        "DELETE FROM faq WHERE id = $1",
        &[id_of(s.value("faq"))?.into()],
    )
    .map_err(|e| failed("deleting the question", e))?;
    Ok(SubmitResult::Redirect("manage/faq".to_owned()))
}

// ---- special prices --------------------------------------------------------

/// AA's special taxes page: everyone who may use the program reads it;
/// its managers add and remove entries.
pub fn prices_page(access: &Access, program_id: i64, request: &Request) -> Result<Page, PageError> {
    let program = crate::calculator::visible_program(access, program_id)?;
    let edit = program.editable_by(access);
    let items = storage::query(
        "SELECT id, type_id, market_group_id, item_tax, disallow_item FROM program_items \
         WHERE program_id = $1 ORDER BY id",
        &[program.id.into()],
    )
    .map_err(|e| failed("reading special taxes", e))?;
    let statics_rows = storage::query(
        "SELECT type_id, price::float8 FROM static_prices WHERE program_id = $1 ORDER BY type_id",
        &[program.id.into()],
    )
    .map_err(|e| failed("reading static prices", e))?;
    let watch = storage::query(
        "SELECT id, type_id, group_id FROM watchlist WHERE program_id = $1 ORDER BY id",
        &[program.id.into()],
    )
    .map_err(|e| failed("reading the watchlist", e))?;
    let mut type_ids: Vec<i64> = items.rows.iter().filter_map(|r| opt_int(r, 1)).collect();
    type_ids.extend(statics_rows.rows.iter().map(|r| int(r, 0)));
    type_ids.extend(watch.rows.iter().filter_map(|r| opt_int(r, 1)));
    let types = statics::by_ids(&type_ids).map_err(|e| failed("reading item data", e))?;
    let market_ids: Vec<i64> = items.rows.iter().filter_map(|r| opt_int(r, 2)).collect();
    let markets: HashMap<i64, String> = statics::market_groups(&market_ids)
        .unwrap_or_default()
        .into_iter()
        .map(|g| (g.id, g.name))
        .collect();
    let group_ids: Vec<i64> = watch.rows.iter().filter_map(|r| opt_int(r, 2)).collect();
    let groups: HashMap<i64, String> = statics::groups(&group_ids)
        .unwrap_or_default()
        .into_iter()
        .map(|g| (g.id, g.name))
        .collect();
    let type_name = |id: i64| {
        types
            .get(&id)
            .map_or_else(|| id.to_string(), |t| t.name.clone())
    };
    let remove = |form: &str, id: i64| -> Value {
        if edit {
            action("Delete", form)
                .field("entry", id.to_string())
                .tone(Tone::Danger)
                .into()
        } else {
            "".into()
        }
    };

    let mut taxes = Table::new(vec![
        Column::text("Item or market group"),
        Column::numeric("Tax adjustment"),
        Column::numeric("Total tax"),
        Column::text("Allowed"),
        Column::text(""),
    ])
    .title("Special item taxes");
    for r in &items.rows {
        let what: Value = match (opt_int(r, 1), opt_int(r, 2)) {
            (Some(t), _) => item_type(t, type_name(t)).into(),
            (_, Some(m)) => format!(
                "Market group: {}",
                markets.get(&m).cloned().unwrap_or_else(|| m.to_string())
            )
            .into(),
            _ => "".into(),
        };
        let tax = int(r, 3);
        taxes = taxes.row(vec![
            what,
            format!("{}{tax}%", if tax > 0 { "+" } else { "" }).into(),
            format!("{}%", program.tax + tax).into(),
            if crate::boolean(r, 4) { "No" } else { "Yes" }.into(),
            remove("remove_item", int(r, 0)),
        ]);
    }
    let mut static_table = Table::new(vec![
        Column::text("Item"),
        Column::numeric("Price"),
        Column::text(""),
    ])
    .title("Static prices");
    for r in &statics_rows.rows {
        static_table = static_table.row(vec![
            item_type(int(r, 0), type_name(int(r, 0))).into(),
            Value::Isk(crate::float(r, 1)),
            remove("remove_static", int(r, 0)),
        ]);
    }
    let mut watch_table = Table::new(vec![Column::text("Item or group"), Column::text("")])
        .title("Manual review watchlist");
    for r in &watch.rows {
        let what: Value = match (opt_int(r, 1), opt_int(r, 2)) {
            (Some(t), _) => item_type(t, type_name(t)).into(),
            (_, Some(g)) => format!(
                "Group: {}",
                groups.get(&g).cloned().unwrap_or_else(|| g.to_string())
            )
            .into(),
            _ => "".into(),
        };
        watch_table = watch_table.row(vec![what, remove("remove_watch", int(r, 0))]);
    }
    let mut page = Page::new(format!("{}: special prices", program.display_name()))
        .table(taxes.empty("No special taxes."))
        .table(static_table.empty("No static prices."))
        .table(watch_table.empty("Nothing on the watchlist."));
    if edit {
        if let Some(problem) = request
            .query
            .iter()
            .find(|(k, _)| k == "problem")
            .map(|(_, v)| v.clone())
        {
            page = page.card(Card::new("Not added").description(problem));
        }
        page = page
            .form(
                Form::new("add_item", "Add item")
                    .title("Item tax or ban")
                    .field(Field::text("item", "Item (its exact name)", 200).required())
                    .field(
                        Field::number("item_tax", "Tax adjustment %")
                            .value("0")
                            .range(Some(-100.0), Some(100.0), true),
                    )
                    .field(Field::checkbox("disallow_item", "Not accepted", false)),
            )
            .form(
                Form::new("add_market_group", "Add market group")
                    .title("Market group tax or ban")
                    .field(Field::text("market_group", "Market group (its name)", 200).required())
                    .field(
                        Field::number("item_tax", "Tax adjustment %")
                            .value("0")
                            .range(Some(-100.0), Some(100.0), true),
                    )
                    .field(Field::checkbox("disallow_item", "Not accepted", false)),
            )
            .form(
                Form::new("add_static", "Set static price")
                    .title("Static price")
                    .field(Field::text("item", "Item (its exact name)", 200).required())
                    .field(
                        Field::number("price", "Static buy price (ISK)")
                            .range(Some(0.0), None, false)
                            .required(),
                    ),
            )
            .form(
                Form::new("add_watch", "Add to watchlist")
                    .title("Manual review watchlist")
                    .field(Field::text("item", "Item (its exact name)", 200))
                    .field(Field::text(
                        "group",
                        "Or an inventory group (its name)",
                        200,
                    )),
            )
            .form(Form::new("remove_all", "Delete all special taxes").title("Start over"));
    }
    Ok(page)
}

fn find_type(name: &str) -> Result<statics::TypeInfo, String> {
    let name = name.trim();
    let found = statics::by_names(&[name.to_owned()]).map_err(|e| format!("{e:?}"))?;
    if let Some(t) = found
        .into_values()
        .next()
        .filter(|t| t.published && t.category_id != 9)
    {
        return Ok(t);
    }
    let close: Vec<String> = statics::search_types(name)
        .map_err(|e| format!("{e:?}"))?
        .into_iter()
        .take(8)
        .map(|t| t.name)
        .collect();
    Err(if close.is_empty() {
        format!("No item is called {name}.")
    } else {
        format!(
            "No item is called exactly {name}. Did you mean: {}?",
            close.join(", ")
        )
    })
}

fn find_named(found: Vec<statics::Named>, name: &str, what: &str) -> Result<i64, String> {
    let name = name.trim();
    if let Some(exact) = found.iter().find(|g| {
        g.name.eq_ignore_ascii_case(name)
            || g.name
                .rsplit(" -> ")
                .next()
                .is_some_and(|n| n.eq_ignore_ascii_case(name))
    }) {
        return Ok(exact.id);
    }
    if found.len() == 1 {
        return Ok(found[0].id);
    }
    let close: Vec<&str> = found.iter().take(8).map(|g| g.name.as_str()).collect();
    Err(if close.is_empty() {
        format!("No {what} is called {name}.")
    } else {
        format!("Several {what}s match {name}: {}.", close.join("; "))
    })
}

pub fn prices_submit(
    access: &Access,
    program_id: i64,
    s: &Submission,
) -> Result<SubmitResult, PageError> {
    let program = programs::get(program_id)
        .map_err(|e| failed("reading the program", e))?
        .ok_or(PageError::NotFound)?;
    if !program.editable_by(access) {
        return Err(PageError::Forbidden);
    }
    let back = format!("program/{}/prices", program.id);
    let problem = |why: String| -> Result<SubmitResult, PageError> {
        let q: String = why.chars().filter(|c| !c.is_control()).take(300).collect();
        Ok(SubmitResult::Redirect(format!(
            "{back}?problem={}",
            urlencode(&q)
        )))
    };
    let tax = s
        .value("item_tax")
        .parse::<i64>()
        .unwrap_or(0)
        .clamp(-100, 100);
    let result = match s.form.as_str() {
        "add_item" => match find_type(s.value("item")) {
            Ok(t) => storage::execute(
                "INSERT INTO program_items (program_id, type_id, item_tax, disallow_item) VALUES ($1, $2, $3, $4) \
                 ON CONFLICT (program_id, type_id) WHERE type_id IS NOT NULL \
                 DO UPDATE SET item_tax = EXCLUDED.item_tax, disallow_item = EXCLUDED.disallow_item",
                &[
                    program.id.into(),
                    t.id.into(),
                    tax.into(),
                    s.checked("disallow_item").into(),
                ],
            ),
            Err(why) => return problem(why),
        },
        "add_market_group" => {
            let found = statics::search_market_groups(s.value("market_group"))
                .map_err(|e| failed("reading market groups", e))?;
            match find_named(found, s.value("market_group"), "market group") {
                Ok(id) => storage::execute(
                    "INSERT INTO program_items (program_id, market_group_id, item_tax, disallow_item) VALUES ($1, $2, $3, $4) \
                     ON CONFLICT (program_id, market_group_id) WHERE market_group_id IS NOT NULL \
                     DO UPDATE SET item_tax = EXCLUDED.item_tax, disallow_item = EXCLUDED.disallow_item",
                    &[
                        program.id.into(),
                        id.into(),
                        tax.into(),
                        s.checked("disallow_item").into(),
                    ],
                ),
                Err(why) => return problem(why),
            }
        }
        "add_static" => {
            let price = s
                .value("price")
                .parse::<f64>()
                .ok()
                .filter(|p| *p >= 0.0 && p.is_finite());
            match (find_type(s.value("item")), price) {
                (Ok(t), Some(price)) => storage::execute(
                    "INSERT INTO static_prices (program_id, type_id, price) VALUES ($1, $2, $3) \
                     ON CONFLICT (program_id, type_id) DO UPDATE SET price = EXCLUDED.price",
                    &[program.id.into(), t.id.into(), price.into()],
                ),
                (Err(why), _) => return problem(why),
                (_, None) => {
                    return problem("A static price is a number of ISK, 0 or more.".into());
                }
            }
        }
        "add_watch" => {
            let (item, group) = (s.value("item").trim(), s.value("group").trim());
            match (item.is_empty(), group.is_empty()) {
                (true, true) => {
                    return problem("You must specify either an Item Type or a Group.".into());
                }
                (false, false) => return problem(
                    "Please specify either an Item Type OR a Group, not both in a single entry."
                        .into(),
                ),
                (false, true) => match find_type(item) {
                    Ok(t) => storage::execute(
                        "INSERT INTO watchlist (program_id, type_id) VALUES ($1, $2) ON CONFLICT DO NOTHING",
                        &[program.id.into(), t.id.into()],
                    ),
                    Err(why) => return problem(why),
                },
                (true, false) => {
                    let found =
                        statics::search_groups(group).map_err(|e| failed("reading groups", e))?;
                    match find_named(found, group, "group") {
                        Ok(id) => storage::execute(
                            "INSERT INTO watchlist (program_id, group_id) VALUES ($1, $2) ON CONFLICT DO NOTHING",
                            &[program.id.into(), id.into()],
                        ),
                        Err(why) => return problem(why),
                    }
                }
            }
        }
        "remove_item" => storage::execute(
            "DELETE FROM program_items WHERE id = $1 AND program_id = $2",
            &[id_of(s.value("entry"))?.into(), program.id.into()],
        ),
        "remove_static" => storage::execute(
            "DELETE FROM static_prices WHERE type_id = $1 AND program_id = $2",
            &[id_of(s.value("entry"))?.into(), program.id.into()],
        ),
        "remove_watch" => storage::execute(
            "DELETE FROM watchlist WHERE id = $1 AND program_id = $2",
            &[id_of(s.value("entry"))?.into(), program.id.into()],
        ),
        "remove_all" => storage::execute(
            "DELETE FROM program_items WHERE program_id = $1",
            &[program.id.into()],
        ),
        _ => return Err(PageError::NotFound),
    };
    result.map_err(|e| failed("saving", e))?;
    Ok(SubmitResult::Redirect(back))
}

fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            b' ' => "+".to_owned(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}
