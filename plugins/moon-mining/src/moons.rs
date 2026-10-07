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
    Card, Column, Field, Form, Page, PageError, RecordPanel, Request, Section, Submission,
    SubmitResult, Table, Tone, Toolbar, Value, badge, character, composition, composition_large,
    corporation, item_type, link, log, part, time,
};

use crate::labels;
use crate::survey::{self, Survey};
use crate::{
    Which, extractions, failed, float, int, isk_or_blank, refinery, rfc3339, system_label, text,
    value, when, with_rows,
};

/// Owned moons: the latest extraction at each moon, with its refinery and
/// corporation, while that refinery is still the corporation's and has a
/// Moon Drill (aa-moonmining's refineries; it deletes the rest,
/// `models/owners.py:210-211`, `:226-232`).
pub(crate) const OWNED: &str = "WITH owned AS (SELECT * FROM (SELECT DISTINCT ON (e.moon_id) e.moon_id, e.structure_id, e.corporation_id \
     FROM extractions e ORDER BY e.moon_id, e.chunk_arrival DESC) l \
     WHERE EXISTS (SELECT 1 FROM structures r WHERE r.structure_id = l.structure_id \
                   AND r.gone_at IS NULL AND r.drill IS NOT FALSE))";

/// Σ share × unit price and the rarest class, per surveyed moon.
const WORTH: &str = "(SELECT p.moon_id, \
         sum(p.amount * coalesce(pr.unit_price, 0))::float8 AS worth, \
         max(t.rarity) AS rarity \
     FROM survey_products p LEFT JOIN prices pr ON pr.type_id = p.type_id \
     LEFT JOIN ore_types t ON t.type_id = p.type_id GROUP BY p.moon_id)";

/// The Moons page's search and filters, from its address (Tether's
/// toolbar): aa-moonmining's rarity, owner, region, ore type and label.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Filter {
    /// Words in the moon's, system's, constellation's, region's, refinery's
    /// or owner's name.
    pub q: String,
    /// R4 to R64 (4 to 64), or 0 for any.
    pub rarity: i64,
    /// The owning corporation, the region, an ore type in the survey and
    /// the label (each 0 for any).
    pub owner: i64,
    pub region: i64,
    pub ore: i64,
    pub label: i64,
    /// The moon whose record panel is open (a row's name selects it).
    pub moon: Option<i64>,
}

/// A filter's id from the address: a positive number, else 0 (any).
fn id_param(request: &Request, name: &str) -> i64 {
    request
        .param(name)
        .parse()
        .ok()
        .filter(|id: &i64| *id > 0)
        .unwrap_or(0)
}

impl From<&Request> for Filter {
    fn from(request: &Request) -> Self {
        Filter {
            q: request.search().to_owned(),
            rarity: request
                .param("rarity")
                .parse()
                .ok()
                .filter(|r| [4, 8, 16, 32, 64].contains(r))
                .unwrap_or(0),
            owner: id_param(request, "owner"),
            region: id_param(request, "region"),
            ore: id_param(request, "ore"),
            label: id_param(request, "label"),
            moon: request.param("moon").parse().ok(),
        }
    }
}

/// The moons the viewer may see, as SQL over `m` (moons), `o` (owned) and
/// `sv` (surveys) with `may_see`'s rules; `$n` is the viewer's account.
fn visible_moons(n: usize) -> String {
    format!(
        "($1::boolean OR ($2::boolean AND o.moon_id IS NOT NULL) OR ($3::boolean AND sv.account_id = ${n}))"
    )
}

/// The toolbar's choices: owners (for those who see them), regions, ore
/// types and labels of the moons the viewer may see, each at most the
/// host's 100.
fn choices(viewer: &Viewer) -> Result<[Vec<(String, String)>; 4], PageError> {
    let rows = storage::query(
        &format!(
            "{OWNED}, seen AS (SELECT m.moon_id, m.system_id, m.label_id, o.corporation_id FROM moons m \
                 LEFT JOIN owned o ON o.moon_id = m.moon_id \
                 LEFT JOIN surveys sv ON sv.moon_id = m.moon_id \
                 WHERE (o.moon_id IS NOT NULL OR sv.moon_id IS NOT NULL) AND {}) \
             SELECT * FROM ( \
                 SELECT DISTINCT 'owner' AS kind, s.corporation_id AS id, \
                        coalesce(n.name, 'Corporation ' || s.corporation_id::text) AS name \
                 FROM seen s LEFT JOIN names n ON n.id = s.corporation_id \
                 WHERE $5::boolean AND s.corporation_id IS NOT NULL \
                 UNION SELECT DISTINCT 'region', y.region_id, coalesce(n.name, 'Region ' || y.region_id::text) \
                 FROM seen s JOIN systems y ON y.system_id = s.system_id LEFT JOIN names n ON n.id = y.region_id \
                 UNION SELECT DISTINCT 'ore', p.type_id, coalesce(n.name, 'Type ' || p.type_id::text) \
                 FROM seen s JOIN survey_products p ON p.moon_id = s.moon_id LEFT JOIN names n ON n.id = p.type_id \
                 UNION SELECT DISTINCT 'label', l.id, l.name FROM seen s JOIN labels l ON l.id = s.label_id) c \
             ORDER BY kind, name",
            visible_moons(4)
        ),
        &[
            viewer.can("view_all_moons").into(),
            viewer.can("extractions_access").into(),
            viewer.can("upload_moon_scan").into(),
            viewer.account_id.into(),
            sees_owners(viewer).into(),
        ],
    )
    .map_err(|e| failed("reading the filters", e))?;
    let mut out: [Vec<(String, String)>; 4] = Default::default();
    for r in &rows.rows {
        let slot = match text(r, 0).as_str() {
            "owner" => 0,
            "region" => 1,
            "ore" => 2,
            _ => 3,
        };
        if out[slot].len() < 100 {
            out[slot].push((int(r, 1).to_string(), text(r, 2)));
        }
    }
    Ok(out)
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
    [4, 8, 16, 32, 64]
        .map(|r| (r.to_string(), value::rarity(r)))
        .to_vec()
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
            "sv.account_id = $8",
            "You haven't uploaded surveys of moons that match.",
        ),
    };
    // The search finds refineries and owners only for those who see them:
    // else it would tell an uploader which moons are ours.
    let mut params: Vec<Db> = vec![
        filter.q.clone().into(),
        filter.rarity.into(),
        owners.into(),
        // The owner filter only for those who see owners.
        if owners { filter.owner } else { 0 }.into(),
        filter.region.into(),
        filter.ore.into(),
        filter.label.into(),
    ];
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
                     WHERE p.moon_id = m.moon_id), \
                    coalesce(lb.name, ''), coalesce(lb.style, '') \
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
             LEFT JOIN labels lb ON lb.id = m.label_id \
             WHERE (o.moon_id IS NOT NULL OR sv.moon_id IS NOT NULL) AND {which} \
               AND ($1::text = '' OR strpos(lower(concat_ws(' ', mn.name, yn.name, cn.name, rn.name, \
                        CASE WHEN $3::boolean THEN st.name END, CASE WHEN $3::boolean THEN co.name END)), \
                                            lower($1::text)) > 0) \
               AND ($2::bigint = 0 OR v.rarity = $2::bigint) \
               AND ($4::bigint = 0 OR o.corporation_id = $4::bigint) \
               AND ($5::bigint = 0 OR y.region_id = $5::bigint) \
               AND ($6::bigint = 0 OR EXISTS (SELECT 1 FROM survey_products f \
                        WHERE f.moon_id = m.moon_id AND f.type_id = $6::bigint)) \
               AND ($7::bigint = 0 OR m.label_id = $7::bigint) \
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
            Column::text("Label"),
            Column::numeric("Value / month (est.)"),
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
                // The name opens the moon's record panel, beside the list.
                link(text(r, 1), format!("moons?moon={}", int(r, 0))).into(),
                system_label(&text(r, 2), float(r, 3)).into(),
                place.into(),
                drill,
                value::rarity(int(r, 9)).into(),
                labels::badge_of(&text(r, 12), &text(r, 13)),
                isk_or_blank(float(r, 8).map(|w| rates.monthly(w))),
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
    let [owner_choices, region_choices, ore_choices, label_choices] = choices(viewer)?;
    let mut toolbar = Toolbar::new()
        .search(if sees_owners(viewer) {
            "Search moons, systems, regions, refineries, owners"
        } else {
            "Search moons, systems, regions"
        })
        .filter("rarity", "Rarity", rarity_choices());
    for (param, label, choices) in [
        ("owner", "Owner", owner_choices),
        ("region", "Region", region_choices),
        ("ore", "Ore type", ore_choices),
        ("label", "Label", label_choices),
    ] {
        if !choices.is_empty() {
            toolbar = toolbar.filter(param, label, choices);
        }
    }
    let mut page = Page::new("Moons")
        .description(format!(
            "Moons with their ores from surveys, and what a month of mining them is worth at {}.",
            crate::pricing()?
        ))
        .toolbar(toolbar);
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
    // A moon the viewer may not see, or that isn't there, opens nothing.
    if let Some(moon) = filter.moon
        && let Ok(panel) = moon_panel(viewer, moon)
    {
        page = page.panel(panel);
    }
    Ok(page)
}

/// A moon as its page and its record panel show it, if the viewer may see
/// it (else as if it weren't there).
struct Moon {
    name: String,
    system: String,
    place: String,
    rarest: i64,
    owned: bool,
    /// The refinery's name and type, and the owner, for those who see
    /// owners.
    refinery: Option<(String, i64, i64, String)>,
    /// The ores, the largest share first.
    products: Vec<Ore>,
    total: f64,
    /// When it was last surveyed, and by whom.
    survey: Option<(String, i64, String)>,
    /// Its label: id, name and style.
    label: Option<(i64, String, String)>,
}

/// One of a moon's ores: its type, name, rarity, share, unit price and a
/// month's value.
struct Ore {
    type_id: i64,
    name: String,
    rarity: i64,
    share: f64,
    price: Option<f64>,
    month: Option<f64>,
}

impl Moon {
    fn load(viewer: &Viewer, moon_id: i64) -> Result<Moon, PageError> {
        let rates = crate::rates()?;
        let rows = storage::query(
            &format!(
                "{OWNED} \
                 SELECT coalesce(mn.name, 'Moon ' || m.moon_id::text), coalesce(yn.name, ''), y.security, \
                        coalesce(cn.name, ''), coalesce(rn.name, ''), o.structure_id, coalesce(st.name, ''), \
                        coalesce(st.type_id, 0), o.corporation_id, coalesce(co.name, ''), \
                        sv.account_id, sv.character_id, sv.character_name, sv.uploaded_at, \
                        lb.id, lb.name, lb.style \
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
                 LEFT JOIN labels lb ON lb.id = m.label_id \
                 WHERE m.moon_id = $1"
            ),
            &[moon_id.into()],
        )
        .map_err(|e| failed("reading the moon", e))?;
        let r = rows.rows.first().ok_or(PageError::NotFound)?;
        let owned = r.get(5).is_some_and(|v| !v.is_null());
        let uploader = r.get(10).and_then(Db::as_integer);
        // Someone who may not see it gets the same as a moon that isn't
        // there.
        if !may_see(viewer, owned, uploader) {
            return Err(PageError::NotFound);
        }
        let products: Vec<Ore> = storage::query(
            "SELECT p.type_id, coalesce(n.name, 'Type ' || p.type_id::text), coalesce(t.rarity, 0), p.amount, \
                    pr.unit_price::float8 \
             FROM survey_products p LEFT JOIN names n ON n.id = p.type_id \
             LEFT JOIN ore_types t ON t.type_id = p.type_id \
             LEFT JOIN prices pr ON pr.type_id = p.type_id \
             WHERE p.moon_id = $1 ORDER BY p.amount DESC",
            &[moon_id.into()],
        )
        .map_err(|e| failed("reading the survey", e))?
        .rows
        .iter()
        .map(|p| {
            let share = float(p, 3).unwrap_or_default();
            let price = float(p, 4);
            Ore {
                type_id: int(p, 0),
                name: text(p, 1),
                rarity: int(p, 2),
                share,
                price,
                month: price.map(|price| rates.monthly(share * price)),
            }
        })
        .collect();
        Ok(Moon {
            name: text(r, 0),
            system: system_label(&text(r, 1), float(r, 2)),
            place: [text(r, 3), text(r, 4)]
                .into_iter()
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(", "),
            rarest: products.iter().map(|p| p.rarity).max().unwrap_or(0),
            owned,
            refinery: (owned && sees_owners(viewer))
                .then(|| (text(r, 6), int(r, 7), int(r, 8), text(r, 9))),
            total: products.iter().filter_map(|p| p.month).sum(),
            products,
            survey: when(r, 13).map(|at| (rfc3339(at), int(r, 11), text(r, 12))),
            label: r
                .get(14)
                .and_then(Db::as_integer)
                .map(|id| (id, text(r, 15), text(r, 16))),
        })
    }

    /// Its ores as a large ring, the month's value in the middle.
    fn ring(&self) -> Option<(Vec<tether_plugin_sdk::Share>, String)> {
        (!self.products.is_empty()).then(|| {
            (
                self.products
                    .iter()
                    .take(8)
                    .map(|p| part(p.name.clone(), p.share.max(1e-9), value::grade(p.rarity)))
                    .collect(),
                value::short_isk(self.total),
            )
        })
    }
}

/// The Moons list's record panel (DESIGN.md, Record panel): the moon a
/// row's name selects, as its page has it in short.
fn moon_panel(viewer: &Viewer, moon_id: i64) -> Result<RecordPanel, PageError> {
    let moon = Moon::load(viewer, moon_id)?;
    let mut panel = RecordPanel::new(
        "moon",
        format!("Moon · {}", value::rarity(moon.rarest))
            .trim_end_matches(" · ")
            .to_owned(),
        moon.name.clone(),
    )
    .context(if moon.place.is_empty() {
        moon.system.clone()
    } else {
        format!("{} · {}", moon.system, moon.place)
    });
    if let Some((parts, center)) = moon.ring() {
        panel = panel.figure(parts, center);
    }
    if let Some((_, name, style)) = &moon.label {
        panel = panel.fact("Label", labels::badge_of(name, style));
    }
    panel = panel.fact(
        "Value / month (est.)",
        isk_or_blank((!moon.products.is_empty()).then_some(moon.total)),
    );
    if let Some((refinery_name, type_id, corp, corp_name)) = &moon.refinery {
        panel = panel
            .fact("Refinery", refinery(refinery_name, *type_id))
            .fact("Owner", corporation(*corp, corp_name.clone()));
    }
    panel = match &moon.survey {
        Some((at, by_id, by)) => panel
            .fact("Last survey", time(at.clone()))
            .fact("Surveyed by", character(*by_id, by.clone())),
        None => panel.fact("Last survey", "Not surveyed yet"),
    };
    Ok(panel.open("Open moon", format!("moon/{moon_id}")))
}

pub fn moon_page(viewer: &Viewer, moon_id: i64) -> Result<Page, PageError> {
    let rates = crate::rates()?;
    let moon = Moon::load(viewer, moon_id)?;
    let owned = moon.owned;
    let name = moon.name.clone();
    let total = moon.total;
    let mut card = Card::new(name.clone())
        .field("System", moon.system.clone())
        .field("Location", moon.place.clone())
        .field("Rarity", value::rarity(moon.rarest));
    if let Some((_, name, style)) = &moon.label {
        card = card.field("Label", labels::badge_of(name, style));
    }
    if let Some((refinery_name, type_id, corp, corp_name)) = &moon.refinery {
        card = card
            .field("Refinery", refinery(refinery_name, *type_id))
            .field("Owner", corporation(*corp, corp_name.clone()));
    }
    card = card.field(
        "Value / month (est.)",
        isk_or_blank((!moon.products.is_empty()).then_some(total)),
    );
    match &moon.survey {
        Some((at, by_id, by)) => {
            card = card
                .field("Last survey", time(at.clone()))
                .field("Surveyed by", character(*by_id, by.clone()));
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
        moon.products.iter().map(|ore| {
            vec![
                item_type(ore.type_id, ore.name.clone()).into(),
                value::rarity(ore.rarity).into(),
                value::percent(ore.share).into(),
                isk_or_blank(ore.price),
                isk_or_blank(ore.month),
            ]
        }),
    );
    // The survey as a large ring, the month's value in its middle.
    let ring = moon
        .ring()
        .map(|(parts, center)| composition_large(parts, center));
    let mut page = Page::new(name)
        .description(format!(
            "A month of mining: price × share × the {:.1} million m³ a drill pulls in a month ÷ \
             10 m³ a unit of ore, at {}.",
            rates.per_month() / 1e6,
            crate::pricing()?
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
    // aa-moonmining's label, set by whoever runs the app (AA's admin).
    if viewer.can("manage")
        && let Some(form) = labels::moon_form(&labels::all()?, moon.label.as_ref().map(|l| l.0))
    {
        page = page.form(form);
    }
    Ok(page)
}

/// A manager's label for a moon they may see.
pub fn save_label(
    viewer: &Viewer,
    moon_id: i64,
    submission: &Submission,
) -> Result<SubmitResult, PageError> {
    if submission.form != "moon_label" || !viewer.can("manage") {
        return Err(PageError::NotFound);
    }
    Moon::load(viewer, moon_id)?;
    labels::save_moon_label(viewer, moon_id, submission)
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
        return Ok(SubmitResult::Page(upload_page(viewer, None)?.text(
            "Moon Mining doesn't know the moon ores yet: it reads them from ESI now. \
                 Try again in a minute.",
        )));
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
    Ok(SubmitResult::Page(upload_page(viewer, Some(&outcomes))?))
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
