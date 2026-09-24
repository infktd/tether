//! An example plugin: one page with stats, a table, a card and tabs, and a
//! second page linked from the first. Build it with
//! `cargo build -p hello-plugin --target wasm32-wasip2 --release`.

use tether_plugin_sdk::{
    Card, Column, Page, PageError, Plugin, Request, Stat, Table, Tone, badge, isk, link, log, time,
};

struct Hello;

impl Plugin for Hello {
    fn render(request: Request) -> Result<Page, PageError> {
        log::debug(format!("rendering {:?}", request.path));
        match request.path.as_str() {
            "" => Ok(main_page()),
            "about" => Ok(Page::new("About")
                .description("What this example shows")
                .text("A plugin describes its pages as data; the host draws them.")),
            _ => Err(PageError::NotFound),
        }
    }
}

fn main_page() -> Page {
    let moons = Table::new(vec![
        Column::text("Moon"),
        Column::text("Status"),
        Column::numeric("Value"),
        Column::numeric("Pops"),
    ])
    .title("Extractions")
    .row(vec![
        "1DQ1-A I - Moon 1".into(),
        badge("Ready", Tone::Accent).into(),
        isk(1_240_000_000.0),
        time("2026-09-24T18:00:00Z"),
    ])
    .row(vec![
        "1DQ1-A II - Moon 3".into(),
        badge("Cooling", Tone::Neutral).into(),
        isk(350_200_000.0),
        time("2026-09-26T02:30:00Z"),
    ]);

    Page::new("Hello")
        .description("An example plugin page")
        .stats(vec![
            Stat::new("Moons", 2).caption("In this example"),
            Stat::new("Value", isk(1_590_200_000.0)),
        ])
        .table(moons)
        .card(
            Card::new("Links")
                .description("Pages of the same plugin link to each other.")
                .field("More", link("About this example", "about")),
        )
        .tab(
            "Notes",
            vec![tether_plugin_sdk::Section::Text(
                "Tabs hold more sections.".into(),
            )],
        )
}

tether_plugin_sdk::export!(Hello);
