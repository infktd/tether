//! A plugin whose pages misbehave on request, for the host's page checks.

use tether_plugin_sdk::{
    Card, Column, Field, Form, Page, PageError, Plugin, Request, Stat, Submission, SubmitResult,
    Table, Tone, badge, isk, link, log, time,
};

fn note_form() -> Form {
    Form::new("note", "Save")
        .title("A note")
        .field(Field::text("body", "Note", 20).required())
        .field(Field::number("count", "Count").range(Some(1.0), Some(10.0), true))
        .field(Field::select(
            "kind",
            "Kind",
            vec![("ore".into(), "Ore".into()), ("ice".into(), "Ice".into())],
        ))
        .field(Field::checkbox("go", "Go elsewhere afterwards", false))
}

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
            "values" => Ok(Page::new("Values")
                .stats(vec![Stat::new("Ore", isk(1_240_000_000.0))])
                .card(
                    Card::new("Moon")
                        .field("Chunk", time("2026-09-24T18:00:00Z"))
                        .field("State", badge("Ready", Tone::Accent))
                        .field("More", link("Old moons", "moons/old")),
                )
                .tab(
                    "First",
                    vec![tether_plugin_sdk::Section::Text("first tab".into())],
                )
                .tab(
                    "Second",
                    vec![tether_plugin_sdk::Section::Text("second tab".into())],
                )),
            "form" => Ok(Page::new("Form").form(note_form())),
            "admin/secret" => Ok(Page::new("Secret")),
            _ => Err(PageError::NotFound),
        }
    }

    fn submit(submission: Submission) -> Result<SubmitResult, PageError> {
        log::info(format!("submitted {}", submission.form));
        if submission.checked("go") {
            return Ok(SubmitResult::Redirect("values".into()));
        }
        Ok(SubmitResult::Page(
            Page::new("Saved").text(format!("got {:?}", submission.values)),
        ))
    }
}

tether_plugin_sdk::export!(Pages);
