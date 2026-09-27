//! aa-moonmining's Moons: owned moons, every moon and the moons one
//! uploaded, each moon's details, and Upload Moon Surveys.
//!
//! - Owned: moons one of our refineries drilled (its latest extraction
//!   says which refinery and corporation), for `extractions_access` or
//!   `view_all_moons`.
//! - All: every moon surveyed or owned, for `view_all_moons`.
//! - My Uploaded: the moons whose latest survey the viewer's account
//!   uploaded, for `upload_moon_scan`.

use tether_plugin_sdk::identity::Viewer;
use tether_plugin_sdk::jobs::{self, NewJob};
use tether_plugin_sdk::storage::{self, Statement, Value as Db};
use tether_plugin_sdk::{
    Card, Column, Field, Form, Page, PageError, Section, Submission, SubmitResult, Table, Tone,
    Value, badge, character, composition, composition_large, corporation, item_type, link, log,
    part, time,
};

use crate::survey::{self, Survey};
use crate::{
    Which, extractions, failed, float, int, isk_or_blank, refinery, rfc3339, system_label, text,
    value, when, with_links, with_rows,
};

/// The latest extraction at each moon: its refinery and corporation.
const OWNED: &str = "WITH owned AS (SELECT DISTINCT ON (e.moon_id) e.moon_id, e.structure_id, e.corporation_id \
     FROM extractions e ORDER BY e.moon_id, e.chunk_arrival DESC)";

/// Σ share × unit price and the rarest class, per surveyed moon.
const WORTH: &str = "(SELECT p.moon_id, \
         sum(p.amount * coalesce(pr.average_price, pr.adjusted_price, 0))::float8 AS worth, \
         max(t.rarity) AS rarity \
     FROM survey_products p LEFT JOIN prices pr ON pr.type_id = p.type_id \
     LEFT JOIN ore_types t ON t.type_id = p.type_id GROUP BY p.moon_id)";

/// The Moons page's search.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Filter {
    /// Words in the moon's, system's, constellation's, region's, refinery's
    /// or owner's name.
    pub q: String,
    /// R4 to R64 (4 to 64), or 0 for any.
    pub rarity: i64,
}

impl From<&Submission> for Filter {
    fn from(submission: &Submission) -> Self {
        Filter {
            q: submission.value("q").trim().to_owned(),
            rarity: submission.value("rarity").parse().unwrap_or(0),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Owned,
    All,
    Mine,
}

/// Whether the viewer sees which moons are ours: their refinery and owner.
/// An uploader alone doesn't, or uploading a moon would tell them.
fn sees_owners(viewer: &Viewer) -> bool {
    viewer.can("extractions_access") || viewer.can("view_all_moons")
}

/// What the viewer may see of a moon: AA's rules for the three tabs.
fn may_see(viewer: &Viewer, owned: bool, uploader: Option<i64>) -> bool {
    viewer.can("view_all_moons")
        || (viewer.can("extractions_access") && owned)
        || (viewer.can("upload_moon_scan") && uploader == Some(viewer.account_id))
}

fn rarity_choices() -> Vec<(String, String)> {
    let mut choices = vec![(String::new(), "Any rarity".to_owned())];
    choices.extend([4, 8, 16, 32, 64].map(|r| (r.to_string(), value::rarity(r))));
    choices
}

fn moons_table(viewer: &Viewer, tab: Tab, filter: &Filter) -> Result<Table, PageError> {
    let rates = crate::rates()?;
    let owners = sees_owners(viewer);
    let (which, empty) = match tab {
        Tab::Owned => (
            "o.moon_id IS NOT NULL",
            "No owned moons match: they come from the refineries' extractions.",
        ),
        Tab::All => ("true", "No moons match."),
        Tab::Mine => (
            "sv.account_id = $3",
            "You haven't uploaded surveys of moons that match.",
        ),
    };
    let mut params: Vec<Db> = vec![filter.q.clone().into(), filter.rarity.into()];
    if tab == Tab::Mine {
        params.push(viewer.account_id.into());
    }
    let rows = storage::query(
        &format!(
            "{OWNED} \
             SELECT m.moon_id, coalesce(mn.name, 'Moon ' || m.moon_id::text), coalesce(yn.name, ''), y.security, \
                    coalesce(cn.name, ''), coalesce(rn.name, ''), coalesce(st.name, ''), coalesce(st.type_id, 0), \
                    v.worth, coalesce(v.rarity, 0), o.structure_id, \
                    (SELECT string_agg(coalesce(t.rarity, 0)::text || ':' || p.amount::text, ',' ORDER BY p.amount DESC) \
                     FROM survey_products p LEFT JOIN ore_types t ON t.type_id = p.type_id \
                     WHERE p.moon_id = m.moon_id) \
             FROM moons m \
             LEFT JOIN owned o ON o.moon_id = m.moon_id \
             LEFT JOIN surveys sv ON sv.moon_id = m.moon_id \
             LEFT JOIN {WORTH} v ON v.moon_id = m.moon_id \
             LEFT JOIN names mn ON mn.id = m.moon_id \
             LEFT JOIN systems y ON y.system_id = m.system_id \
             LEFT JOIN names yn ON yn.id = m.system_id \
             LEFT JOIN names cn ON cn.id = y.constellation_id \
             LEFT JOIN names rn ON rn.id = y.region_id \
             LEFT JOIN structures st ON st.structure_id = o.structure_id \
             LEFT JOIN names co ON co.id = o.corporation_id \
             WHERE (o.moon_id IS NOT NULL OR sv.moon_id IS NOT NULL) AND {which} \
               AND ($1::text = '' OR strpos(lower(concat_ws(' ', mn.name, yn.name, cn.name, rn.name, st.name, co.name)), \
                                            lower($1::text)) > 0) \
               AND ($2::bigint = 0 OR v.rarity = $2::bigint) \
             ORDER BY v.worth DESC NULLS LAST, 2 LIMIT 500"
        ),
        &params,
    )
    .map_err(|e| failed("reading moons", e))?;
    Ok(with_rows(
        Table::new(vec![
            Column::text(""),
            Column::text("Moon"),
            Column::text("System"),
            Column::text("Location"),
            Column::text("Refinery"),
            Column::text("Rarity"),
            Column::numeric("Value / month (est.)"),
            Column::numeric(""),
        ])
        .empty(empty),
        rows.rows.iter().map(|r| {
            let place = [text(r, 4), text(r, 5)]
                .into_iter()
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(", ");
            let drill: Value = if owners && r.get(10).is_some_and(|v| !v.is_null()) {
                refinery(&text(r, 6), int(r, 7))
            } else {
                "".into()
            };
            // The moon's ores as a ring, darker to brighter by rarity.
            let parts = value::parts(&text(r, 11));
            let ring: Value = if parts.is_empty() {
                "".into()
            } else {
                composition(
                    parts
                        .iter()
                        .map(|(class, share)| {
                            part(value::rarity(*class), *share, value::grade(*class))
                        })
                        .collect(),
                )
            };
            vec![
                ring,
                text(r, 1).into(),
                system_label(&text(r, 2), float(r, 3)).into(),
                place.into(),
                drill,
                value::rarity(int(r, 9)).into(),
                isk_or_blank(float(r, 8).map(|w| rates.monthly(w))),
                link("Details", format!("moon/{}", int(r, 0))).into(),
            ]
        }),
    ))
}

pub fn moons_page(viewer: &Viewer, filter: &Filter) -> Result<Page, PageError> {
    let sees_any = viewer.can("extractions_access")
        || viewer.can("view_all_moons")
        || viewer.can("upload_moon_scan");
    if !sees_any {
        // aa-moonmining shows Moons to everyone, with the tabs their
        // permissions allow: here, none.
        return Ok(Page::new("Moons").text(
            "No moons to show you: owned moons need Extractions access, every moon needs \
             View all moons, and uploading surveys needs Upload moon scan.",
        ));
    }
    let mut page = Page::new("Moons")
        .description(
            "Moons with their ores from surveys, and what a month of mining them is worth at CCP's \
             average ore prices (the unrefined ore's, as ESI has no reprocessing yields).",
        )
        .form(
            Form::new("filter", "Search")
                .field(
                    Field::text("q", "Moon, system, region, refinery or owner", 100)
                        .value(filter.q.clone()),
                )
                .field(Field::select("rarity", "Rarity", rarity_choices()).value(
                    if filter.rarity > 0 {
                        filter.rarity.to_string()
                    } else {
                        String::new()
                    },
                )),
        );
    if viewer.can("extractions_access") || viewer.can("view_all_moons") {
        page = page.tab(
            "Owned Moons",
            vec![Section::Table(moons_table(viewer, Tab::Owned, filter)?)],
        );
    }
    if viewer.can("view_all_moons") {
        page = page.tab(
            "All Moons",
            vec![Section::Table(moons_table(viewer, Tab::All, filter)?)],
        );
    }
    if viewer.can("upload_moon_scan") {
        page = page.tab(
            "My Uploaded Moons",
            vec![Section::Table(moons_table(viewer, Tab::Mine, filter)?)],
        );
    }
    Ok(page)
}

pub fn moon_page(viewer: &Viewer, moon_id: i64) -> Result<Page, PageError> {
    let rates = crate::rates()?;
    let rows = storage::query(
        &format!(
            "{OWNED} \
             SELECT coalesce(mn.name, 'Moon ' || m.moon_id::text), coalesce(yn.name, ''), y.security, \
                    coalesce(cn.name, ''), coalesce(rn.name, ''), o.structure_id, coalesce(st.name, ''), \
                    coalesce(st.type_id, 0), o.corporation_id, coalesce(co.name, ''), \
                    sv.account_id, sv.character_id, sv.character_name, sv.uploaded_at \
             FROM moons m \
             LEFT JOIN owned o ON o.moon_id = m.moon_id \
             LEFT JOIN surveys sv ON sv.moon_id = m.moon_id \
             LEFT JOIN names mn ON mn.id = m.moon_id \
             LEFT JOIN systems y ON y.system_id = m.system_id \
             LEFT JOIN names yn ON yn.id = m.system_id \
             LEFT JOIN names cn ON cn.id = y.constellation_id \
             LEFT JOIN names rn ON rn.id = y.region_id \
             LEFT JOIN structures st ON st.structure_id = o.structure_id \
             LEFT JOIN names co ON co.id = o.corporation_id \
             WHERE m.moon_id = $1"
        ),
        &[moon_id.into()],
    )
    .map_err(|e| failed("reading the moon", e))?;
    let r = rows.rows.first().ok_or(PageError::NotFound)?;
    let owned = r.get(5).is_some_and(|v| !v.is_null());
    let uploader = r.get(10).and_then(Db::as_integer);
    // Someone who may not see it gets the same as a moon that isn't there.
    if !may_see(viewer, owned, uploader) {
        return Err(PageError::NotFound);
    }
    let name = text(r, 0);
    let products = storage::query(
        "SELECT p.type_id, coalesce(n.name, 'Type ' || p.type_id::text), coalesce(t.rarity, 0), p.amount, \
                coalesce(pr.average_price, pr.adjusted_price)::float8 \
         FROM survey_products p LEFT JOIN names n ON n.id = p.type_id \
         LEFT JOIN ore_types t ON t.type_id = p.type_id \
         LEFT JOIN prices pr ON pr.type_id = p.type_id \
         WHERE p.moon_id = $1 ORDER BY p.amount DESC",
        &[moon_id.into()],
    )
    .map_err(|e| failed("reading the survey", e))?;
    let monthly: Vec<Option<f64>> = products
        .rows
        .iter()
        .map(|p| float(p, 4).map(|price| rates.monthly(float(p, 3).unwrap_or_default() * price)))
        .collect();
    let total: f64 = monthly.iter().flatten().sum();
    let rarest = products.rows.iter().map(|p| int(p, 2)).max().unwrap_or(0);
    let place = [text(r, 3), text(r, 4)]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(", ");
    let mut card = Card::new(name.clone())
        .field("System", system_label(&text(r, 1), float(r, 2)))
        .field("Location", place)
        .field("Rarity", value::rarity(rarest));
    if owned && sees_owners(viewer) {
        card = card
            .field("Refinery", refinery(&text(r, 6), int(r, 7)))
            .field("Owner", corporation(int(r, 8), text(r, 9)));
    }
    card = card.field(
        "Value / month (est.)",
        isk_or_blank((!products.rows.is_empty()).then_some(total)),
    );
    match when(r, 13) {
        Some(at) => {
            card = card
                .field("Last survey", time(rfc3339(at)))
                .field("Surveyed by", character(int(r, 11), text(r, 12)));
        }
        None => card = card.field("Last survey", "Not surveyed yet"),
    }
    let composition = with_rows(
        Table::new(vec![
            Column::text("Ore"),
            Column::text("Rarity"),
            Column::numeric("Share"),
            Column::numeric("Unit price"),
            Column::numeric("Value / month (est.)"),
        ])
        .title("Ore composition")
        .empty("No survey of this moon yet: upload one to see its ores."),
        products.rows.iter().zip(&monthly).map(|(p, month)| {
            vec![
                item_type(int(p, 0), text(p, 1)).into(),
                value::rarity(int(p, 2)).into(),
                value::percent(float(p, 3).unwrap_or_default()).into(),
                isk_or_blank(float(p, 4)),
                isk_or_blank(*month),
            ]
        }),
    );
    // The survey as a large ring, the month's value in its middle.
    let ring = (!products.rows.is_empty()).then(|| {
        composition_large(
            products
                .rows
                .iter()
                .take(8)
                .map(|p| {
                    part(
                        text(p, 1),
                        float(p, 3).unwrap_or_default().max(1e-9),
                        value::grade(int(p, 2)),
                    )
                })
                .collect(),
            value::short_isk(total),
        )
    });
    let mut page = Page::new(name)
        .description(format!(
            "A month of mining: price × share × the {:.1} million m³ a drill pulls in a month ÷ \
             10 m³ a unit of ore, at CCP's average price of the ore itself.",
            rates.per_month() / 1e6
        ))
        .card(card);
    if let Some(ring) = ring {
        page = page.card(Card::new("Composition").field("Ores by share", ring));
    }
    let mut page = page.table(composition);
    if viewer.can("extractions_access") && owned {
        let now = chrono::Utc::now();
        let history = extractions(Which::AtMoon, &[moon_id.into()])?;
        page = page.table(with_rows(
            Table::new(vec![
                Column::numeric("Chunk arrival"),
                Column::text("Status"),
                Column::numeric("Value (est.)"),
                Column::numeric("Mined"),
                Column::numeric(""),
            ])
            .title("Extractions")
            .empty("No extractions seen at this moon."),
            history.iter().map(|p| {
                vec![
                    time(rfc3339(p.arrival)),
                    p.status(now),
                    isk_or_blank(p.value()),
                    isk_or_blank(p.mined),
                    p.details(),
                ]
            }),
        ));
    }
    Ok(page)
}

/// How one moon of an upload went.
pub struct Outcome {
    moon: String,
    stored: bool,
    detail: String,
}

pub fn upload_page(viewer: &Viewer, outcomes: Option<&[Outcome]>) -> Result<Page, PageError> {
    if !viewer.can("upload_moon_scan") {
        return Err(PageError::Forbidden);
    }
    let mut page = Page::new("Upload moon surveys").description(
        "Probe a moon, then in the Moon Analysis window choose Copy to clipboard and paste it here. \
         Paste several moons at once; each is read on its own.",
    );
    if let Some(outcomes) = outcomes {
        let stored = outcomes.iter().filter(|o| o.stored).count();
        page = page.table(with_rows(
            Table::new(vec![
                Column::text("Moon"),
                Column::text("Result"),
                Column::text("Details"),
            ])
            .title(format!("{stored} of {} moons stored", outcomes.len()))
            .empty("Nothing in the paste looked like a moon survey."),
            outcomes.iter().map(|o| {
                vec![
                    o.moon.clone().into(),
                    if o.stored {
                        badge("Stored", Tone::Success)
                    } else {
                        badge("Not stored", Tone::Danger)
                    }
                    .into(),
                    o.detail.clone().into(),
                ]
            }),
        ));
    }
    Ok(page.form(
        Form::new("survey", "Upload").field(
            Field::textarea("scan", "Moon surveys", 10_000)
                .help(
                    "The moon's name on a line, then a line per ore: product, quantity, ore type, \
                     system, planet and moon, separated by tabs. A new survey of a moon replaces \
                     its last one.",
                )
                .required(),
        ),
    ))
}

pub fn upload(viewer: &Viewer, submission: &Submission) -> Result<SubmitResult, PageError> {
    if !viewer.can("upload_moon_scan") {
        return Err(PageError::Forbidden);
    }
    let ores = storage::query("SELECT type_id FROM ore_types", &[])
        .map_err(|e| failed("reading moon ores", e))?;
    let ores: Vec<i64> = ores.rows.iter().map(|r| int(r, 0)).collect();
    if ores.is_empty() {
        // Moon ores come from ESI with the daily prices: fetch them now.
        if let Err(err) = jobs::enqueue(NewJob::new("prices").key("prices")) {
            log::warn(format!("queuing prices: {err:?}"));
        }
        return Ok(SubmitResult::Page(with_links(
            upload_page(viewer, None)?.text(
                "Moon Mining doesn't know the moon ores yet: it reads them from ESI now. \
                 Try again in a minute.",
            ),
            viewer,
        )?));
    }
    let parsed = checked(viewer, survey::parse(submission.value("scan")), &ores)?;
    let good: Vec<&Survey> = parsed.iter().filter_map(|r| r.as_ref().ok()).collect();
    if !good.is_empty() {
        store(viewer, &good)?;
        // Names and places for the new moons now, and prices for ores not
        // priced yet (at most hourly, however often people upload).
        if let Err(err) = jobs::enqueue(NewJob::new("places").key("places")) {
            log::warn(format!("queuing a look-up of new moons: {err:?}"));
        }
        let unpriced = storage::query(
            "SELECT 1 FROM survey_products p \
             WHERE NOT EXISTS (SELECT 1 FROM prices r WHERE r.type_id = p.type_id) \
               AND NOT EXISTS (SELECT 1 FROM prices r WHERE r.updated_at > now() - interval '1 hour') \
             LIMIT 1",
            &[],
        )
        .map_err(|e| failed("reading prices", e))?;
        if !unpriced.rows.is_empty()
            && let Err(err) = jobs::enqueue(NewJob::new("prices").key("prices"))
        {
            log::warn(format!("queuing prices: {err:?}"));
        }
        log::info(format!(
            "{} moon surveys uploaded by {} ({})",
            good.len(),
            viewer.main.name,
            viewer.main.id
        ));
    }
    let outcomes: Vec<Outcome> = parsed
        .into_iter()
        .map(|r| match r {
            Ok(s) => Outcome {
                detail: format!(
                    "{} ores: moon {}; its name and place come from ESI shortly",
                    s.products.len(),
                    s.moon_id
                ),
                moon: s.name,
                stored: true,
            },
            Err(why) => Outcome {
                moon: why.moon,
                stored: false,
                detail: why.why,
            },
        })
        .collect();
    Ok(SubmitResult::Page(with_links(
        upload_page(viewer, Some(&outcomes))?,
        viewer,
    )?))
}

/// Surveys refused beyond their paste: ores that aren't moon ores (only
/// ESI's moon ores are stored, named and priced), and, for an uploader
/// who doesn't see which moons are ours, a moon someone else surveyed
/// (whose survey they'd replace).
fn checked(
    viewer: &Viewer,
    parsed: Vec<Result<Survey, survey::Rejected>>,
    ores: &[i64],
) -> Result<Vec<Result<Survey, survey::Rejected>>, PageError> {
    let ids: Vec<String> = parsed
        .iter()
        .filter_map(|r| r.as_ref().ok())
        .map(|s| s.moon_id.to_string())
        .collect();
    let taken: Vec<i64> = if sees_owners(viewer) || ids.is_empty() {
        Vec::new()
    } else {
        storage::query(
            "SELECT moon_id FROM surveys WHERE moon_id = ANY(string_to_array($1, ',')::bigint[]) \
             AND account_id <> $2",
            &[ids.join(",").into(), viewer.account_id.into()],
        )
        .map_err(|e| failed("reading surveys", e))?
        .rows
        .iter()
        .map(|r| int(r, 0))
        .collect()
    };
    Ok(parsed
        .into_iter()
        .map(|r| {
            let s = r?;
            let reject = |why: String| survey::Rejected {
                moon: s.name.clone(),
                why,
            };
            if let Some(p) = s.products.iter().find(|p| !ores.contains(&p.type_id)) {
                return Err(reject(format!("ore type {} isn't a moon ore", p.type_id)));
            }
            if taken.contains(&s.moon_id) {
                return Err(reject(
                    "someone else already surveyed this moon, and only those who see owned or all moons can replace a survey"
                        .into(),
                ));
            }
            Ok(s)
        })
        .collect())
}

/// Stores surveys, each replacing its moon's last, all or none.
fn store(viewer: &Viewer, surveys: &[&Survey]) -> Result<(), PageError> {
    let moons: Vec<serde_json::Value> = surveys
        .iter()
        .map(|s| serde_json::json!({ "moon_id": s.moon_id, "system_id": s.system_id }))
        .collect();
    let products: Vec<serde_json::Value> = surveys
        .iter()
        .flat_map(|s| {
            s.products.iter().map(|p| {
                serde_json::json!({ "moon_id": s.moon_id, "type_id": p.type_id, "amount": p.amount })
            })
        })
        .collect();
    let moons = serde_json::Value::Array(moons).to_string();
    storage::transaction(&[
        Statement::new(
            "INSERT INTO moons (moon_id, system_id) \
             SELECT moon_id, system_id FROM json_to_recordset($1::json) AS x(moon_id bigint, system_id bigint) \
             ON CONFLICT (moon_id) DO NOTHING",
            vec![Db::json(moons.clone())],
        ),
        Statement::new(
            "INSERT INTO surveys (moon_id, account_id, character_id, character_name, uploaded_at) \
             SELECT moon_id, $2, $3, $4, now() FROM json_to_recordset($1::json) AS x(moon_id bigint) \
             ON CONFLICT (moon_id) DO UPDATE SET account_id = EXCLUDED.account_id, \
             character_id = EXCLUDED.character_id, character_name = EXCLUDED.character_name, \
             uploaded_at = now()",
            vec![
                Db::json(moons.clone()),
                viewer.account_id.into(),
                viewer.main.id.into(),
                viewer.main.name.clone().into(),
            ],
        ),
        Statement::new(
            "DELETE FROM survey_products WHERE moon_id IN \
             (SELECT moon_id FROM json_to_recordset($1::json) AS x(moon_id bigint))",
            vec![Db::json(moons)],
        ),
        Statement::new(
            "INSERT INTO survey_products (moon_id, type_id, amount) \
             SELECT moon_id, type_id, amount FROM json_to_recordset($1::json) \
             AS x(moon_id bigint, type_id bigint, amount float8)",
            vec![Db::json(serde_json::Value::Array(products).to_string())],
        ),
    ])
    .map_err(|e| failed("storing surveys", e))?;
    Ok(())
}
