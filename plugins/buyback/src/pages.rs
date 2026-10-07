//! Which page or form is which, and the small pages: the programs list,
//! the FAQ, a pilot's own settings and the app's Settings.

use std::collections::HashMap;

use tether_plugin_sdk::storage;
use tether_plugin_sdk::{
    Card, Column, Field, Page, PageError, Request, SettingsForm, SettingsGroup, Submission,
    SubmitResult, Table, Value, action, actions, link, log,
};

use crate::programs::{self, Program};
use crate::{Access, failed, int, settings, text};

pub fn render(access: &Access, request: &Request) -> Result<Page, PageError> {
    let path = request.path.as_str();
    let parts: Vec<&str> = path.split('/').collect();
    match parts.as_slice() {
        [""] => index(access),
        ["faq"] => faq(),
        ["me"] => me(access),
        ["settings"] if access.manage_all() => app_settings(),
        ["program", id] => crate::calculator::page(access, id_of(id)?),
        ["program", id, "prices"] => crate::manage::prices_page(access, id_of(id)?, None),
        ["program", id, "leaderboard"] => crate::stats::leaderboard(access, id_of(id)?, request),
        ["program", id, "performance"] => crate::stats::performance(access, id_of(id)?, request),
        ["tracking", number] => crate::stats::details(access, number),
        ["stats"] => crate::stats::mine(access, request),
        ["program-stats"] => crate::stats::programs(access, false, request),
        ["all-stats"] => crate::stats::programs(access, true, request),
        ["reverse", ..] | ["manage", "reverse", ..] => crate::reverse::render(access, request),
        ["manage", ..] => crate::manage::render(access, request),
        _ => Err(PageError::NotFound),
    }
}

pub fn submit(access: &Access, s: &Submission) -> Result<SubmitResult, PageError> {
    let path = s.request.path.as_str();
    let parts: Vec<&str> = path.split('/').collect();
    match (parts.as_slice(), s.form.as_str()) {
        (["program", id], "calculate") => crate::calculator::submit(access, id_of(id)?, s),
        (["program", id, "prices"], _) => crate::manage::prices_submit(access, id_of(id)?, s),
        (["program", _, "performance"], _) => crate::stats::submit(access, s),
        (["me"], "me") => save_me(access, s),
        (["settings"], "settings") if access.manage_all() => save_settings(access, s),
        (["reverse", ..] | ["manage", "reverse", ..], _) => crate::reverse::submit(access, s),
        (["tracking", ..] | ["program-stats"] | ["all-stats"] | ["stats"], _) => {
            crate::stats::submit(access, s)
        }
        (["manage", ..], _) => crate::manage::submit(access, s),
        _ => Err(PageError::NotFound),
    }
}

pub(crate) fn id_of(s: &str) -> Result<i64, PageError> {
    s.parse::<i64>().map_err(|_| PageError::NotFound)
}

/// AA's index: the programs the viewer may use, each with its manager,
/// locations, tax and what the viewer may do with it.
fn index(access: &Access) -> Result<Page, PageError> {
    let settings = settings().map_err(|e| failed("reading settings", e))?;
    let programs: Vec<Program> = programs::all()
        .map_err(|e| failed("reading programs", e))?
        .into_iter()
        .filter(|p| p.visible_to(access))
        .collect();
    let owners = owner_names(&programs);
    let leaderboard = access.can("see_leaderboard");
    let mut columns = vec![
        Column::text("Program"),
        Column::text("Manager"),
        Column::text("Locations"),
        Column::numeric("Tax"),
        Column::text("Special prices"),
    ];
    if leaderboard {
        columns.push(Column::text("Leaderboard"));
    }
    columns.push(Column::text("Performance"));
    columns.push(Column::text(""));
    let mut table = Table::new(columns);
    for p in &programs {
        let locations = crate::calculator::location_names(p.id)?;
        let shown = usize::try_from(settings.show_location_count).unwrap_or(4);
        let places = if locations.len() > shown {
            format!("{} locations", locations.len())
        } else {
            locations.join(", ")
        };
        let owner_id = if p.is_corporation {
            p.owner_corporation
        } else {
            p.owner_character
        };
        let mut row: Vec<Value> = vec![
            link(p.display_name(), format!("program/{}", p.id)).into(),
            owners
                .get(&owner_id)
                .cloned()
                .unwrap_or_else(|| owner_id.to_string())
                .into(),
            places.into(),
            format!("{}%", p.tax).into(),
            link("Prices", format!("program/{}/prices", p.id)).into(),
        ];
        if leaderboard {
            row.push(link("Leaderboard", format!("program/{}/leaderboard", p.id)).into());
        }
        let performance = access.can("see_performance") || p.editable_by(access);
        row.push(if performance {
            link("Performance", format!("program/{}/performance", p.id)).into()
        } else {
            "".into()
        });
        row.push(if p.editable_by(access) {
            link("Edit", format!("manage/program/{}", p.id)).into()
        } else {
            "".into()
        });
        table = table.row(row);
    }
    Ok(Page::new("Buyback programs")
        .description(
            "Sell your items to a program: paste them from your inventory, and contract them to its manager with the tracking number you get.",
        )
        .table(table.empty("No programs you may use yet.")))
}

/// Names of programs' owners (characters and corporations).
pub(crate) fn owner_names(programs: &[Program]) -> HashMap<i64, String> {
    let mut ids: Vec<i64> = programs
        .iter()
        .map(|p| {
            if p.is_corporation {
                p.owner_corporation
            } else {
                p.owner_character
            }
        })
        .collect();
    ids.sort_unstable();
    ids.dedup();
    tether_plugin_sdk::esi::names(&ids)
        .map(|n| n.into_iter().map(|n| (n.id, n.name)).collect())
        .unwrap_or_default()
}

/// AA's FAQ: its three questions, then the admins' own.
fn faq() -> Result<Page, PageError> {
    let mut page = Page::new("FAQ")
        .card(Card::new("Where can I sell my items?").description(
            "Each program lists the locations it accepts contracts at. Pick a program on the Programs page and check its locations.",
        ))
        .card(Card::new("How can I sell my items?").description(
            "Open a program, paste your items from your inventory and calculate. Then create an Item Exchange contract to the program's manager with the price and the tracking number you got as its description.",
        ))
        .card(Card::new("How can I turn off notifications for my contracts?").description(
            "Under My settings, tick Disable notifications.",
        ));
    let rows = storage::query("SELECT header, body FROM faq ORDER BY position, id", &[])
        .map_err(|e| failed("reading the FAQ", e))?;
    for r in rows.rows.iter().take(crate::TABLE_ROWS) {
        page = page.card(Card::new(text(r, 0)).description(text(r, 1)));
    }
    Ok(page)
}

fn me(access: &Access) -> Result<Page, PageError> {
    let off = crate::notifications_off(access.account());
    Ok(Page::new("My settings").settings(
        SettingsForm::new("me").group(
            SettingsGroup::new("Notifications").field(
                Field::checkbox("disable_notifications", "Disable notifications", off)
                    .help("No notices when a program accepts or rejects your contracts."),
            ),
        ),
    ))
}

fn save_me(access: &Access, s: &Submission) -> Result<SubmitResult, PageError> {
    storage::execute(
        "INSERT INTO user_settings (account_id, disable_notifications) VALUES ($1, $2) \
         ON CONFLICT (account_id) DO UPDATE SET disable_notifications = EXCLUDED.disable_notifications",
        &[access.account().into(), s.checked("disable_notifications").into()],
    )
    .map_err(|e| failed("saving your settings", e))?;
    Ok(SubmitResult::Redirect("me".to_owned()))
}

/// aa-buybackprogram's Django settings, for app admins.
fn app_settings() -> Result<Page, PageError> {
    let s = settings().map_err(|e| failed("reading settings", e))?;
    let refreshed = s
        .prices_updated_at
        .map_or_else(|| "not yet".to_owned(), crate::rfc3339);
    let mut page = Page::new("Buyback settings");
    if let Some(problem) = &s.sync_error {
        page = page
            .card(Card::new("The last contract read had a problem").description(problem.clone()));
    }
    Ok(page.settings(
        SettingsForm::new("settings")
            .group(
                SettingsGroup::new("Prices")
                    .description(format!(
                        "Where item prices come from. Fuzzwork needs nothing; Janice needs its API key, set under Administration › Apps › Buyback. Prices last refreshed: {refreshed}."
                    ))
                    .field(
                        Field::select(
                            "price_method",
                            "Price source",
                            vec![
                                ("Fuzzwork".to_owned(), "Fuzzwork".to_owned()),
                                ("Janice".to_owned(), "Janice".to_owned()),
                            ],
                        )
                        .value(s.price_method.clone()),
                    )
                    .field(
                        Field::number("price_source_id", "Station")
                            .value(s.price_source_id.to_string())
                            .range(Some(1.0), None, true)
                            .help("Fuzzwork's market hub, by station id (Jita IV - Moon 4: 60003760). Janice always uses Jita."),
                    )
                    .field(Field::text("price_source_name", "Station name", 64).value(s.price_source_name.clone()))
                    .field(
                        Field::checkbox("instant_prices", "Instant prices", s.instant_prices)
                            .help("Highest buy and lowest sell order, instead of the top 5% average."),
                    )
                    .field(
                        Field::number("price_age_warning_hours", "Price age warning (hours)")
                            .value(s.price_age_warning_hours.to_string())
                            .range(Some(1.0), Some(8760.0), true),
                    ),
            )
            .group(
                SettingsGroup::new("Tracking")
                    .field(
                        Field::text("tracking_prefill", "Tracking prefix", 16)
                            .value(s.tracking_prefill.clone())
                            .help("Starts every tracking number, unless a program has its own. Letters, digits, dots, dashes and underscores."),
                    )
                    .field(
                        Field::checkbox("track_prefill_contracts", "Flag possible scams", s.track_prefill_contracts)
                            .help("Contracts whose description has a buyback prefix but no tracking number of ours."),
                    )
                    .field(
                        Field::number("purge_hours", "Forget unused calculations after (hours)")
                            .value(s.purge_hours.to_string())
                            .range(Some(0.0), Some(8760.0), true)
                            .help("Also how long reverse buyback reservations last. 0: never."),
                    )
                    .field(
                        Field::checkbox("disallow_any_disallowed", "Refuse pastes with anything not accepted", s.disallow_any_disallowed)
                            .help("No price at all until those items are removed."),
                    )
                    .field(
                        Field::checkbox("restrict_tracking_details", "Restrict contract details", s.restrict_tracking_details)
                            .help("Only the seller and managers open a contract's details."),
                    ),
            )
            .group(
                SettingsGroup::new("Pages")
                    .field(
                        Field::number("show_location_count", "Locations shown on the programs list")
                            .value(s.show_location_count.to_string())
                            .range(Some(1.0), Some(50.0), true),
                    )
                    .field(Field::checkbox("reverse_enabled", "Reverse buyback", s.reverse_enabled)),
            ),
    ))
}

fn save_settings(access: &Access, s: &Submission) -> Result<SubmitResult, PageError> {
    let number = |name: &str, min: i64, max: i64| -> Result<i64, PageError> {
        s.value(name)
            .parse::<i64>()
            .ok()
            .filter(|n| (min..=max).contains(n))
            .ok_or_else(|| PageError::Failed(format!("{name} is a number from {min} to {max}")))
    };
    let method = match s.value("price_method") {
        "Janice" => "Janice",
        _ => "Fuzzwork",
    };
    let prefill: String = s.value("tracking_prefill").trim().to_owned();
    if !prefill
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        || prefill.is_empty()
    {
        return Err(PageError::Failed(
            "the tracking prefix takes letters, digits, dots, dashes and underscores".to_owned(),
        ));
    }
    storage::execute(
        "UPDATE settings SET price_method = $1, price_source_id = $2, price_source_name = $3, \
             instant_prices = $4, price_age_warning_hours = $5, tracking_prefill = $6, \
             track_prefill_contracts = $7, purge_hours = $8, disallow_any_disallowed = $9, \
             restrict_tracking_details = $10, show_location_count = $11, reverse_enabled = $12 \
         WHERE id = 1",
        &[
            method.into(),
            number("price_source_id", 1, i64::MAX)?.into(),
            s.value("price_source_name").trim().to_owned().into(),
            s.checked("instant_prices").into(),
            number("price_age_warning_hours", 1, 8760)?.into(),
            prefill.into(),
            s.checked("track_prefill_contracts").into(),
            number("purge_hours", 0, 8760)?.into(),
            s.checked("disallow_any_disallowed").into(),
            s.checked("restrict_tracking_details").into(),
            number("show_location_count", 1, 50)?.into(),
            s.checked("reverse_enabled").into(),
        ],
    )
    .map_err(|e| failed("saving settings", e))?;
    log::info(format!(
        "settings changed by {} ({}): prices from {method}",
        access.viewer.main.name, access.viewer.main.id
    ));
    Ok(SubmitResult::Redirect("settings".to_owned()))
}

/// Actions a program row offers its managers (used by the Manage list).
pub(crate) fn program_actions(p: &Program) -> Value {
    actions(vec![
        action("Delete", "delete_program")
            .field("program", p.id.to_string())
            .tone(tether_plugin_sdk::Tone::Danger)
            .confirm("This program and its special prices go; its tracking history stays, no longer matched."),
    ])
}

/// The number of a program's stored rows (for the Manage list).
pub(crate) fn count(sql: &str, id: i64) -> i64 {
    storage::query(sql, &[id.into()])
        .ok()
        .and_then(|r| r.rows.first().map(|row| int(row, 0)))
        .unwrap_or(0)
}
