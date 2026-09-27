//! A plugin whose pages misbehave on request, for the host's page checks.

use tether_plugin_sdk::{
    Card, CardGrid, CodeBlock, Column, Field, Form, Page, PageError, Plugin, Profile, Request,
    Stat, Submission, SubmitResult, Table, Tone, action, actions, add_owner, alliance, badge,
    character, corporation, countdown, faction, isk, item_type, link, log, progress, share, time,
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

/// Every new block, with markup in every string a plugin gives: the host
/// must escape all of it.
fn blocks() -> Page {
    const EVIL: &str = "<script>alert(1)</script>";
    Page::new("Blocks")
        .link("Blocks", "blocks")
        .link(format!("Values {EVIL}"), "values")
        .profile(
            Profile::new(character(90_000_001, format!("Pilot {EVIL}")))
                .subtitle(format!("Main {EVIL}"))
                .corporation(corporation(98_000_001, "Corp \"quoted\" & co"))
                .alliance(alliance(0, "Alliance <b>bold</b>"))
                .badge(badge(format!("Badge {EVIL}"), Tone::Success))
                .fact("Skill points", 48_210_332)
                .fact(format!("Fact {EVIL}"), isk(1_240_000_000.0))
                .fact("Ship", item_type(587, "Rifter"))
                .fact("Faction", faction(500_001, "Faction"))
                .fact("Clone", countdown("2099-01-01T00:00:00+02:00"))
                .fact("Done", countdown("2000-01-01T00:00:00Z")),
        )
        .button(format!("Create {EVIL}"), "form")
        .table(
            Table::new(vec![
                Column::text("Who"),
                Column::text("Training"),
                Column::text("Do"),
            ])
            .row(vec![
                character(-5, "Fixture Pilot").into(),
                progress(0.0)
                    .between("2000-01-01T00:00:00Z", "2099-01-01T00:00:00Z")
                    .label(format!("Skill {EVIL}"))
                    .into(),
                actions(vec![
                    action("Approve", "decide")
                        .field("id", "7")
                        .field("verdict", "approve")
                        .tone(Tone::Accent),
                    action(format!("Reject {EVIL}"), "decide")
                        .field("id", "7")
                        .field("verdict", format!("reject \"{EVIL}\""))
                        .tone(Tone::Danger)
                        .confirm(format!("Pilot 7 is told no {EVIL}")),
                ]),
            ])
            .row(vec![
                corporation(98_000_002, "Second Corp").into(),
                progress(0.42).into(),
                action("Close", "close").field("id", "8").into(),
            ]),
        )
        .code(
            CodeBlock::new(format!("[Rifter, {EVIL}]\n  Damage Control II\n"))
                .title(format!("Fit {EVIL}"))
                .copy_label(format!("Copy {EVIL}")),
        )
        .code(CodeBlock::new("plain"))
        .card(
            Card::new("Share")
                .field("Register link", share("values"))
                // An app without data sources: drawn as nothing.
                .field("Owner", add_owner(format!("Log in {EVIL}"))),
        )
        .cards(
            // Asks for the Register Character card, which an app without
            // user scopes doesn't get.
            CardGrid::new()
                .register()
                .linked(
                    Profile::new(character(90_000_002, format!("Card {EVIL}")))
                        .corporation(corporation(98_000_001, "Corp"))
                        .alliance(alliance(99_000_001, format!("Ally {EVIL}")))
                        .fact("Wallet", isk(5.0e9)),
                    "values",
                )
                .card(Profile::new(character(0, "Unlinked Pilot"))),
        )
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
            "blocks" => Ok(blocks()),
            "live" => Ok(Page::new("Live").refresh(1).text("syncing")),
            "live-form" => Ok(Page::new("Live form").refresh(10).form(note_form())),
            "mail/1" => Ok(Page::new("Mail")
                .refresh(5)
                .text(format!("mail {:?}", request.query))),
            "bad-progress" => Ok(Page::new("Bad progress").card(Card::new("Bar").field(
                "p",
                progress(0.5).between("2026-09-24T18:00:00Z", "not a time"),
            ))),
            "bad-page-link" => Ok(Page::new("Bad page link").link("away", "//evil.example")),
            "action-clash" => Ok(Page::new("Clash")
                .form(note_form())
                .card(Card::new("c").field("a", action("Go", "note")))),
            "form" => Ok(Page::new("Form").form(note_form())),
            "groups" => {
                let names = |groups: Vec<tether_plugin_sdk::identity::Group>| {
                    groups
                        .into_iter()
                        .map(|g| format!("{}={}", g.id, g.name))
                        .collect::<Vec<_>>()
                        .join(",")
                };
                // Twice each: the second answer comes from the call's own
                // copy.
                let mine = names(tether_plugin_sdk::identity::groups());
                let _ = tether_plugin_sdk::identity::groups();
                let offered = names(tether_plugin_sdk::identity::all_groups());
                let _ = tether_plugin_sdk::identity::all_groups();
                Ok(Page::new("Groups")
                    .text(format!("mine[{mine}]"))
                    .text(format!("offered[{offered}]")))
            }
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
