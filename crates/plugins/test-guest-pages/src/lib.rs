//! A plugin whose pages misbehave on request, for the host's page checks.

use tether_plugin_sdk::{
    Card, CardGrid, CodeBlock, Column, Field, Form, Lane, LaneItem, Page, PageError, Plugin,
    Profile, RecordPanel, Request, Stat, Submission, SubmitResult, Table, Timeline, Tone, Toolbar,
    action, actions, add_owner, alliance, badge, character, composition, composition_large,
    corporation, countdown, defenses, faction, isk, item_type, levels, link, log, part, progress,
    share, time,
};

/// A long list with no search of its own: Tether finds rows among those
/// it shows.
fn list() -> Page {
    let mut table = Table::new(vec![Column::text("Name"), Column::numeric("n")]);
    for n in 1..=30 {
        let name = if n % 2 == 0 { "Beta" } else { "Alpha" };
        table = table.row(vec![format!("{name} {n}").into(), n.into()]);
    }
    Page::new("List")
        .stats(vec![Stat::new("Rows", 30)])
        .table(table)
}

/// A page searching its own data, with a filter: it says what it was
/// asked, and its rows don't match (Tether mustn't hide them).
fn searched(request: &Request) -> Page {
    let mut table = Table::new(vec![Column::text("Name")]);
    for name in ["x", "y", "z", "w", "v", "u", "t", "s"] {
        table = table.row(vec![name.into()]);
    }
    Page::new("Searched")
        .toolbar(Toolbar::new().search("Search things").filter(
            "kind",
            "Kind",
            vec![("ore".into(), "Ore".into()), ("ice".into(), "Ice".into())],
        ))
        .text(format!(
            "asked q={:?} kind={:?}",
            request.search(),
            request.param("kind")
        ))
        .table(table)
}

/// Rows whose names select their record panel.
fn panel(request: &Request) -> Page {
    let mut table = Table::new(vec![Column::text("Item"), Column::numeric("n")]);
    for n in 1..=3 {
        table = table.row(vec![
            link(format!("Item {n}"), format!("panel?item={n}")).into(),
            n.into(),
        ]);
    }
    let mut page = Page::new("Panel").table(table);
    if let Ok(n) = request.param("item").parse::<i64>() {
        page = page.panel(
            RecordPanel::new("item", "Item · test", format!("Item {n}"))
                .context("<b>context</b>")
                .figure(vec![part("Ore", 2.0, 1), part("Ice", 1.0, 3)], "1.2b")
                .fact("Number", n)
                .open("Open item", "values")
                .action(action("Pin", "pin").field("item", n.to_string())),
        );
    }
    page
}

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

/// The instruments: skill levels, composition rings, defenses and a
/// timeline, with markup in every string the plugin gives.
fn instruments() -> Page {
    const EVIL: &str = "<script>alert(1)</script>";
    Page::new("Instruments")
        .table(
            Table::new(vec![
                Column::text("Skill"),
                Column::text("Level"),
                Column::text("Moon"),
                Column::text("Hull"),
            ])
            .row(vec![
                "Mining Foreman".into(),
                levels(4, Some(5)),
                composition(vec![
                    part(format!("Xenotime {EVIL}"), 0.31, 4),
                    part("Sylvite", 0.69, 0),
                ]),
                defenses(0.0, 0.62, 1.0, true),
            ]),
        )
        .card(Card::new("Moon").field(
            "Composition",
            composition_large(
                vec![part("Chromite", 0.34, 2), part("Bitumens", 0.66, 0)],
                format!("1.84B {EVIL}"),
            ),
        ))
        .timeline(
            Timeline::new("2026-09-27T00:00:00Z", "2026-10-01T00:00:00Z")
                .title(format!("Next days {EVIL}"))
                .window("2026-09-27T18:00:00Z", "2026-09-27T21:00:00Z")
                .lane(
                    Lane::new(format!("Fleets {EVIL}"))
                        .caption("02")
                        .item(
                            LaneItem::new(format!("Stratop {EVIL}"), "2026-09-28T07:00:00Z")
                                .until("2026-09-28T10:00:00Z")
                                .tone(Tone::Warning)
                                .link("values"),
                        )
                        .item(LaneItem::new("Next chunk", "2026-09-29T19:00:00Z").planned()),
                ),
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
            "instruments" => Ok(instruments()),
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
            // A popup's action may not carry a field the form has.
            "action-clash" => Ok(Page::new("Clash")
                .form(note_form())
                .card(Card::new("c").field("a", action("Go", "note").field("body", "x")))),
            // Nor open a form on another tab.
            "popup-tab" => Ok(Page::new("Tabs")
                .card(Card::new("c").field("a", action("Go", "note").field("item", "1")))
                .tab("Other", vec![tether_plugin_sdk::Section::Form(note_form())])),
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
            // A form under a tab, whose post comes back to its own page.
            "tabbed-form" => Ok(Page::new("Tabbed form")
                .tab(
                    "Text",
                    vec![tether_plugin_sdk::Section::Text("text tab".into())],
                )
                .tab("Form", vec![tether_plugin_sdk::Section::Form(note_form())])),
            "admin/secret" => Ok(Page::new("Secret")),
            "list" => Ok(list()),
            "searched" => Ok(searched(&request)),
            "panel" => Ok(panel(&request)),
            // The search's parameter is Tether's.
            "bad-toolbar" => Ok(Page::new("Bad toolbar").toolbar(Toolbar::new().filter(
                "q",
                "Q",
                vec![("a".into(), "A".into())],
            ))),
            // A panel opens its record's page, not a download.
            "panel-download" => Ok(Page::new("Panel download")
                .panel(RecordPanel::new("item", "Item", "One").open("Export", "downloads/rows"))),
            // A panel's parameter can't be a filter's too.
            "bad-panel" => Ok(Page::new("Bad panel")
                .toolbar(Toolbar::new().filter("item", "Item", vec![("1".into(), "1".into())]))
                .panel(RecordPanel::new("item", "Item", "One"))),
            _ => Err(PageError::NotFound),
        }
    }

    fn submit(submission: Submission) -> Result<SubmitResult, PageError> {
        log::info(format!("submitted {}", submission.form));
        if submission.form == "pin" {
            return Ok(SubmitResult::Page(
                Page::new("Pinned").text(format!("pinned {}", submission.value("item"))),
            ));
        }
        if submission.request.path == "tabbed-form" {
            return Ok(SubmitResult::Redirect("tabbed-form".into()));
        }
        if submission.checked("go") {
            return Ok(SubmitResult::Redirect("values".into()));
        }
        Ok(SubmitResult::Page(
            Page::new("Saved").text(format!("got {:?}", submission.values)),
        ))
    }
}

tether_plugin_sdk::export!(Pages);
