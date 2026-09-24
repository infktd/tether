//! A plugin whose pages misbehave on request, for the host's page checks.

use tether_plugin_sdk::{Column, Page, PageError, Plugin, Request, Table, link, log};

struct Pages;

impl Plugin for Pages {
    fn render(request: Request) -> Result<Page, PageError> {
        match request.path.as_str() {
            "" => Ok(Page::new("Fine").text("A well-formed page.")),
            "too-big" => {
                let mut table = Table::new(vec![Column::numeric("n")]);
                for n in 0..600 {
                    table = table.row(vec![n.into()]);
                }
                Ok(Page::new("Too big").table(table))
            }
            "bad-link" => Ok(Page::new("Bad link").card(
                tether_plugin_sdk::Card::new("Away")
                    .field("go", link("elsewhere", "https://evil.example")),
            )),
            "forbidden" => Err(PageError::Forbidden),
            "failed" => Err(PageError::Failed("the database is on fire".into())),
            "chatty" => {
                for n in 0..250 {
                    log::info(format!("line {n}"));
                }
                log::warn("bell\u{7} and\u{1b}[31m escape");
                Ok(Page::new("Chatty"))
            }
            "crash" => {
                let empty: Vec<u8> = Vec::new();
                // Out of bounds on purpose.
                let byte = empty[std::hint::black_box(3)];
                Ok(Page::new(format!("unreachable {byte}")))
            }
            "query" => Ok(Page::new(format!("{:?}", request.query))),
            "huge-log" => {
                // 20 MiB in one log line: more than the host copies per call.
                log::info("x".repeat(20 * 1024 * 1024));
                Ok(Page::new("Never shown"))
            }
            "many-cells" => {
                let mut page = Page::new("Many cells");
                for _ in 0..21 {
                    let mut table = Table::new(vec![Column::numeric("n")]);
                    for n in 0..500 {
                        table = table.row(vec![n.into()]);
                    }
                    page = page.table(table);
                }
                Ok(page)
            }
            "long-failure" => Err(PageError::Failed(format!("\u{202E}{}", "e".repeat(10_000)))),
            _ => Err(PageError::NotFound),
        }
    }
}

tether_plugin_sdk::export!(Pages);
