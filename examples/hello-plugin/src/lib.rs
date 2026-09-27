//! An example plugin: a page with every kind of block (a profile, stats,
//! tables with entities, countdowns and progress bars, a card, text to
//! copy, a grid of character cards and tabs), page links beside the title, and a live page that
//! reloads itself while it "syncs". Build it with
//! `cargo build -p hello-plugin --target wasm32-wasip2 --release`.

use std::time::{SystemTime, UNIX_EPOCH};

use tether_plugin_sdk::{
    Card, CardGrid, CodeBlock, Column, Page, PageError, Plugin, Profile, Request, Section, Stat,
    Submission, SubmitResult, Table, Tone, Value, action, actions, alliance, badge, character,
    corporation, countdown, isk, item_type, link, log, progress, share, time,
};

struct Hello;

impl Plugin for Hello {
    fn render(request: Request) -> Result<Page, PageError> {
        log::debug(format!("rendering {:?}", request.path));
        let page = match request.path.as_str() {
            "" => main_page(),
            "about" => Page::new("About")
                .description("What this example shows")
                .text("A plugin describes its pages as data; the host draws them."),
            "live" => live_page(),
            _ => return Err(PageError::NotFound),
        };
        // The same links on every page: the host marks the one shown.
        Ok(page
            .link("Overview", "")
            .link("Live", "live")
            .link("About", "about"))
    }

    /// The row buttons: the host checked this page offered exactly these
    /// values to this person before calling.
    fn submit(submission: Submission) -> Result<SubmitResult, PageError> {
        let moon = submission.value("moon");
        let done = match submission.value("do") {
            "fracture" => "fractured",
            "stop" => "stopped",
            _ => return Err(PageError::NotFound),
        };
        Ok(SubmitResult::Page(
            Page::new("Hello").link("Overview", "").text(format!(
                "Moon {moon}: {done} (not really; this is an example)."
            )),
        ))
    }
}

/// Seconds since the epoch.
fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// An instant as RFC 3339 in UTC (`2026-09-24T18:00:00Z`).
fn rfc3339(unix: i64) -> String {
    let (days, secs) = (unix.div_euclid(86_400), unix.rem_euclid(86_400));
    // Days to a civil date (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        secs / 3_600,
        secs % 3_600 / 60,
        secs % 60
    )
}

const FIT: &str = "[Rifter, Example Rifter]
Damage Control II
Gyrostabilizer II

1MN Afterburner II

200mm AutoCannon II, Republic Fleet EMP S
200mm AutoCannon II, Republic Fleet EMP S
200mm AutoCannon II, Republic Fleet EMP S
";

/// A row's buttons: they post to `submit` as the `moon` form, with the
/// moon's id.
fn moon_actions(moon: &str) -> Value {
    actions(vec![
        action("Fracture", "moon")
            .field("moon", moon)
            .field("do", "fracture"),
        action("Stop", "moon")
            .field("moon", moon)
            .field("do", "stop")
            .tone(Tone::Danger)
            .confirm("The extraction stops and its ore is lost."),
    ])
}

fn main_page() -> Page {
    let now = now();
    let pilot = Profile::new(character(0, "Aura Example"))
        .subtitle("Main of 3 characters")
        .corporation(corporation(1_000_044, "School of Applied Knowledge"))
        .alliance(alliance(0, "Example Alliance"))
        .badge(badge("Main", Tone::Neutral))
        .badge(badge("Registered", Tone::Success))
        .fact("Skill points", 48_210_332)
        .fact("Wallet", isk(1_240_000_000.0))
        .fact("Security status", "5.0")
        .fact("Location", "Jita IV - Moon 4")
        .fact("Ship", item_type(587, "Rifter"))
        .fact(
            "Next jump clone",
            countdown(rfc3339(now + 7 * 3_600 + 13 * 60)),
        )
        .fact(
            "Training",
            progress(0.0)
                .between(rfc3339(now - 3_600), rfc3339(now + 2 * 3_600))
                .label("Gunnery V"),
        );

    let moons = Table::new(vec![
        Column::text("Moon"),
        Column::text("Owner"),
        Column::text("Status"),
        Column::numeric("Value"),
        Column::numeric("Pops in"),
        Column::text(""),
    ])
    .title("Extractions")
    .row(vec![
        "Example I - Moon 1".into(),
        corporation(1_000_044, "School of Applied Knowledge").into(),
        badge("Ready", Tone::Accent).into(),
        isk(1_240_000_000.0),
        countdown(rfc3339(now + 2 * 86_400 + 4 * 3_600 + 13 * 60)),
        moon_actions("1"),
    ])
    .row(vec![
        "Example II - Moon 3".into(),
        corporation(0, "Example Mining Corp").into(),
        badge("Cooling", Tone::Neutral).into(),
        isk(350_200_000.0),
        countdown(rfc3339(now - 600)),
        moon_actions("3"),
    ]);

    let queue = Table::new(vec![
        Column::text("Skill"),
        Column::text("Progress"),
        Column::numeric("Finishes in"),
        Column::numeric("Started"),
    ])
    .title("Skill queue")
    .row(vec![
        item_type(3_300, "Gunnery V").into(),
        progress(0.0)
            .between(rfc3339(now - 3_600), rfc3339(now + 2 * 3_600))
            .into(),
        countdown(rfc3339(now + 2 * 3_600)),
        time(rfc3339(now - 3_600)),
    ])
    .row(vec![
        item_type(3_301, "Small Projectile Turret V").into(),
        progress(0.25).label("Paused").into(),
        "Queued".into(),
        "".into(),
    ]);

    Page::new("Hello")
        .description("An example plugin page")
        .button("Plan extraction", "about")
        .profile(pilot)
        .stats(vec![
            Stat::new("Moons", 2).caption("In this example"),
            Stat::new("Value", isk(1_590_200_000.0)),
        ])
        .table(moons)
        .table(queue)
        .code(
            CodeBlock::new(FIT)
                .title("Doctrine fit")
                .copy_label("Copy fit"),
        )
        .card(
            Card::new("Links")
                .description("Pages of the same plugin link to each other.")
                .field("More", link("About this example", "about"))
                .field("Share", share("about")),
        )
        .tab(
            "Notes",
            vec![Section::Text("Tabs hold more sections.".into())],
        )
        .tab(
            "Characters",
            vec![Section::Cards(
                // `register()` starts it with Tether's Register Character
                // card, for apps with user scopes (this one has none).
                CardGrid::new()
                    .register()
                    .linked(
                        Profile::new(character(0, "Aura Example"))
                            .corporation(corporation(1_000_044, "School of Applied Knowledge"))
                            .badge(badge("Main", Tone::Neutral))
                            .fact("Wallet", isk(1_240_000_000.0))
                            .fact("Ship", item_type(587, "Rifter")),
                        "about",
                    )
                    .card(
                        Profile::new(character(0, "Example Alt"))
                            .corporation(corporation(0, "Example Mining Corp"))
                            .fact(
                                "Training",
                                progress(0.0)
                                    .between(rfc3339(now - 600), rfc3339(now + 3_600))
                                    .label("Mining V"),
                            ),
                    ),
            )],
        )
}

/// A page that "syncs" for the first 20 seconds of every half minute: while
/// it does, it asks to be reloaded every 5 seconds; then it stops asking.
fn live_page() -> Page {
    let now = now();
    let second = now.rem_euclid(30);
    let page = Page::new("Live").description("Reloads itself while it syncs");
    if second < 20 {
        page.refresh(5)
            .stats(vec![
                Stat::new("Synced", format!("{}/4", second / 5)).caption("pages"),
                Stat::new("Started", time(rfc3339(now - second))),
            ])
            .text("Syncing: this page reloads every 5 seconds until it's done.")
    } else {
        page.stats(vec![
            Stat::new("Synced", "4/4").caption("pages"),
            Stat::new("Next sync in", countdown(rfc3339(now - second + 30))),
        ])
        .text("Done. The page no longer reloads; open it again to watch another sync.")
    }
}

tether_plugin_sdk::export!(Hello);

#[cfg(test)]
mod tests {
    use super::rfc3339;

    #[test]
    fn instants_are_written_in_rfc3339() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(1_790_272_800), "2026-09-24T18:00:00Z");
        assert_eq!(rfc3339(951_782_400), "2000-02-29T00:00:00Z");
    }
}
