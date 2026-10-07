//! The Character Sheet: aa-memberaudit's tabs over a few pages, each with
//! its own tabs, linked beside the title:
//!
//! - Overview (`character/{id}`): the profile, whose it is and the
//!   owner's characters; corporation history, roles
//!   (when the Settings read them) and titles, killmails, bio. Its pilot
//!   shares it from here (`share_characters`).
//! - Skills: the queue (live), skills by group, skill sets (for
//!   `view_skill_sets`), attributes.
//! - Assets, by location (`assets/{location}` for one location's items).
//! - Wallet: journal, transactions, market orders, contracts (and each
//!   contract's items), loyalty points.
//! - Clones: implants, jump clones.
//! - Industry: jobs, blueprints, mining ledger, planets.
//! - Contacts: contacts, NPC standings.
//! - Mail, for whoever may open the sheet (`mail/{id}`, audited).

use chrono::{Duration, Utc};
use tether_plugin_sdk::jobs::{self, NewJob};
use tether_plugin_sdk::storage::{self, Value as Db};
use tether_plugin_sdk::{
    Card, Column, Page, PageError, Profile, Request, Section, Stat, Submission, SubmitResult,
    Table, Tone, Value, action, alliance, badge, character, corporation, countdown, faction, isk,
    item_type, levels, link,
};

use crate::access::Access;
use crate::pages::{entity, named, roman};
use crate::{
    boolean, clip, count, failed, float, int, name_of, opt_float, opt_int, query, rfc3339, text,
    time_or_blank, when, with_rows,
};

/// Rows in one table, at most (the host's limit is 500).
const MAX_ROWS: i64 = 300;
/// An Update now may be asked for once in this long per character: a
/// minute (Jay, 2026-10-05: when a change needs ESI, read now). Sections
/// read in the last few minutes are skipped and ESI's cache answers the
/// rest, and Tether holds apps' ESI calls back while its budget is low.
const UPDATE_WAIT: Duration = Duration::minutes(1);
/// Updates of other pilots' characters one person may ask for an hour.
const MAX_ASKS_PER_HOUR: i64 = 10;

/// The character a sheet is about.
pub(crate) struct Subject {
    pub id: i64,
    pub name: String,
    pub corporation_id: i64,
    pub corporation: String,
    pub alliance_id: Option<i64>,
    pub alliance: String,
    pub may_read_mail: bool,
    /// Its Skill Sets tab (`view_skill_sets`).
    pub skill_sets: bool,
}

/// The character, if the viewer may open its sheet; not found otherwise
/// (the same answer as for a character Member Audit doesn't have).
pub(crate) fn subject(access: &Access, id: i64) -> Result<Subject, PageError> {
    let rows = query(
        &format!(
            "SELECT c.name, c.corporation_id, c.alliance_id, {corp}, {ally} FROM characters c \
             WHERE c.character_id = $1",
            corp = name_of("c.corporation_id"),
            ally = name_of("c.alliance_id"),
        ),
        &[id.into()],
    )?;
    let row = rows.first().ok_or(PageError::NotFound)?;
    let corporation_id = int(row, 1);
    let alliance_id = opt_int(row, 2).filter(|a| *a > 0);
    if !access.may_open(id) {
        return Err(PageError::NotFound);
    }
    Ok(Subject {
        id,
        name: text(row, 0),
        corporation_id,
        corporation: text(row, 3),
        alliance_id,
        alliance: text(row, 4),
        may_read_mail: access.may_read_mail(id),
        skill_sets: access.skill_sets,
    })
}

/// A page about the character, with its sections as chips under the
/// app's views.
pub(crate) fn sheet_page(who: &Subject, what: &str) -> Page {
    let id = who.id;
    let mut page = Page::new(who.name.clone())
        .description(what.to_owned())
        .link("Summary", format!("character/{id}"))
        .link("Skills", format!("character/{id}/skills"))
        .link("Assets", format!("character/{id}/assets"))
        .link("Wallet", format!("character/{id}/wallet"))
        .link("Clones", format!("character/{id}/clones"))
        .link("Industry", format!("character/{id}/industry"))
        .link("Contacts", format!("character/{id}/contacts"));
    if who.may_read_mail {
        page = page.link("Mail", format!("mail/{id}"));
    }
    page
}

/// When each section was last read, and how it went.
pub(crate) struct Freshness(Vec<(String, Option<chrono::DateTime<Utc>>, bool, String)>);

impl Freshness {
    pub fn of(id: i64) -> Result<Self, PageError> {
        let rows = query(
            "SELECT section, synced_at, ok, coalesce(error, '') FROM section_syncs WHERE character_id = $1",
            &[id.into()],
        )?;
        Ok(Self(
            rows.iter()
                .map(|r| (text(r, 0), when(r, 1), boolean(r, 2), text(r, 3)))
                .collect(),
        ))
    }

    /// Whether a section has been tried at all: one that hasn't shows no
    /// numbers (its zeros would read as facts).
    pub fn tried(&self, section: &str) -> bool {
        self.0.iter().any(|(name, ..)| name == section)
    }

    /// `stat` as it is once its section has been tried, else "—" and "not
    /// read yet".
    pub fn stat(&self, section: &str, stat: Stat) -> Stat {
        if self.tried(section) {
            stat
        } else {
            Stat {
                value: "".into(),
                caption: Some("not read yet".to_owned()),
                ..stat
            }
        }
    }

    /// A line saying how fresh these sections are, for the end of a tab
    /// (aa-memberaudit's "Last update" under each).
    pub fn line(&self, sections: &[&str]) -> Section {
        let found: Vec<_> = self
            .0
            .iter()
            .filter(|(name, ..)| sections.contains(&name.as_str()))
            .collect();
        if found.len() < sections.len() {
            return Section::Text("Not read yet: it's on its way.".to_owned());
        }
        // Skipped, not read: its login lacks a scope asked for since
        // (`sync::WAITS_FOR_REGISTERING`).
        let (waiting, found): (Vec<_>, Vec<_>) = found
            .into_iter()
            .partition(|(_, _, ok, why)| *ok && !why.is_empty());
        if found.is_empty() && !waiting.is_empty() {
            return Section::Text(
                "Not read yet: its pilot needs to register it with Member Audit again.".to_owned(),
            );
        }
        let oldest = found.iter().filter_map(|(_, at, ..)| *at).min();
        let failed: Vec<&str> = found
            .iter()
            .filter(|(_, _, ok, _)| !ok)
            .map(|(_, _, _, why)| why.as_str())
            .collect();
        let at = oldest.map_or_else(String::new, |t| t.format("%Y-%m-%d %H:%M").to_string());
        Section::Text(if failed.is_empty() {
            format!("Last update {at} EVE.")
        } else {
            format!(
                "Last tried {at} EVE, and it failed: {}. It's tried again on its next turn.",
                clip(&failed.join("; "), 300)
            )
        })
    }
}

pub(crate) fn render(
    access: &Access,
    id: i64,
    rest: &[&str],
    request: &Request,
) -> Result<Page, PageError> {
    let who = subject(access, id)?;
    match rest {
        [] => overview(access, &who, None),
        ["skills"] => skills(&who, request.param("set")),
        ["assets"] => assets(&who, None),
        ["assets", location] => {
            let location: i64 = location.parse().map_err(|_| PageError::NotFound)?;
            assets(&who, Some(location))
        }
        ["wallet"] => wallet(&who),
        ["contract", contract] => {
            let contract: i64 = contract.parse().map_err(|_| PageError::NotFound)?;
            contract_page(&who, contract)
        }
        ["clones"] => clones(&who),
        ["industry"] => industry(&who),
        ["contacts"] => contacts(&who),
        _ => Err(PageError::NotFound),
    }
}

// ---- Update now ------------------------------------------------------------

/// Queues a read of the character's every section (not those read in the
/// last few minutes).
pub(crate) fn update_now(
    access: &Access,
    submission: &Submission,
) -> Result<SubmitResult, PageError> {
    let id: i64 = submission
        .value("character")
        .parse()
        .map_err(|_| PageError::NotFound)?;
    // The sheet the button is on, and one the viewer may open.
    if submission.request.path != format!("character/{id}") {
        return Err(PageError::Forbidden);
    }
    let who = subject(access, id)?;
    // Other pilots' characters: a few an hour per person, so nobody
    // spends the app's ESI budget in bulk.
    let own = access.owns(id);
    if !own {
        let asked = query(
            "SELECT count(*) FROM update_asks WHERE account_id = $1 AND at > now() - interval '1 hour'",
            &[access.viewer.account_id.into()],
        )?;
        if asked.first().map_or(0, |r| int(r, 0)) >= MAX_ASKS_PER_HOUR {
            return Ok(SubmitResult::Page(overview(
                access,
                &who,
                Some(&format!(
                    "You've asked for {MAX_ASKS_PER_HOUR} updates of other pilots' characters in \
                     the last hour: this one waits for its turn in the regular sync."
                )),
            )?));
        }
    }
    let queued = storage::query(
        "UPDATE characters SET update_requested_at = now() WHERE character_id = $1 \
         AND (update_requested_at IS NULL OR update_requested_at < now() - make_interval(mins => $2::int)) \
         RETURNING character_id",
        &[id.into(), UPDATE_WAIT.num_minutes().into()],
    )
    .map_err(|e| failed("asking for an update", e))?;
    if !queued.rows.is_empty() {
        let enqueued = jobs::enqueue(
            NewJob::new(crate::sync::UPDATE)
                .key(format!("update:{id}"))
                .payload(serde_json::json!({ "character": id }).to_string()),
        );
        if let Err(err) = enqueued {
            // Nothing is coming: don't say it is.
            let _ = storage::execute(
                "UPDATE characters SET update_requested_at = NULL WHERE character_id = $1",
                &[id.into()],
            );
            return Err(failed("queuing the update", err));
        }
        if !own {
            storage::execute(
                "INSERT INTO update_asks (account_id, character_id) VALUES ($1, $2)",
                &[access.viewer.account_id.into(), id.into()],
            )
            .map_err(|e| failed("recording the update", e))?;
        }
        tether_plugin_sdk::log::info(format!(
            "update of character {id} asked for by {} ({})",
            access.viewer.main.name, access.viewer.main.id
        ));
    }
    Ok(SubmitResult::Redirect(format!("character/{id}")))
}

// ---- Sharing ---------------------------------------------------------------

/// Shares the viewer's own character with holders of
/// `view_shared_characters` (for `share_characters`), or stops sharing it
/// (always), as aa-memberaudit's launcher.
pub(crate) fn share(
    access: &Access,
    submission: &Submission,
    shared: bool,
) -> Result<SubmitResult, PageError> {
    let id: i64 = submission
        .value("character")
        .parse()
        .map_err(|_| PageError::NotFound)?;
    // The sheet the button is on, of one of the viewer's own characters.
    if submission.request.path != format!("character/{id}") || !access.owns(id) {
        return Err(PageError::Forbidden);
    }
    if shared && !access.share {
        return Err(PageError::Forbidden);
    }
    subject(access, id)?;
    // Who shared it: the share counts only while they own it.
    storage::execute(
        "UPDATE characters SET is_shared = $2, shared_at = CASE WHEN $2 THEN now() END, \
         shared_by_main = CASE WHEN $2 THEN $3 END WHERE character_id = $1",
        &[id.into(), shared.into(), access.viewer.main.id.into()],
    )
    .map_err(|e| failed("sharing the character", e))?;
    tether_plugin_sdk::log::info(format!(
        "character {id} {} by {} ({})",
        if shared { "shared" } else { "no longer shared" },
        access.viewer.main.name,
        access.viewer.main.id
    ));
    Ok(SubmitResult::Redirect(format!("character/{id}")))
}

// ---- Overview --------------------------------------------------------------

fn overview(access: &Access, who: &Subject, note: Option<&str>) -> Result<Page, PageError> {
    let id = who.id;
    let rows = query(
        &format!(
            "SELECT coalesce(c.total_sp, 0), coalesce(c.unallocated_sp, 0), c.wallet, c.system_id, {system}, \
                    c.location_id, {place}, c.location_type, c.ship_type_id, {ship}, coalesce(c.ship_name, ''), \
                    c.security_status, c.birthday, c.synced_at, c.home_location_id, {home}, \
                    c.faction_id, {fact}, coalesce(c.bio, ''), c.update_requested_at, \
                    (SELECT count(*) FROM clones WHERE character_id = c.character_id), \
                    (SELECT max(finish) FROM queue WHERE character_id = c.character_id), \
                    c.update_done_at, c.is_shared, c.last_login \
             FROM characters c WHERE c.character_id = $1",
            system = name_of("c.system_id"),
            place = name_of("c.location_id"),
            ship = name_of("c.ship_type_id"),
            home = name_of("c.home_location_id"),
            fact = name_of("c.faction_id"),
        ),
        &[id.into()],
    )?;
    let c = rows.first().ok_or(PageError::NotFound)?;
    let mut profile = Profile::new(character(id, who.name.clone()))
        .corporation(corporation(who.corporation_id, who.corporation.clone()));
    if let Some(a) = who.alliance_id {
        profile = profile.alliance(alliance(a, who.alliance.clone()));
    }
    // Whose it is (aa-memberaudit's sidebar): the owner's main and their
    // other characters, those not registered marked.
    // Whoever may open the sheet sees them, a recruiter opening a shared
    // character too, as aa-memberaudit's sidebar (the Share button says
    // so).
    let member = access.member_of(id);
    match member {
        Some(m) if m.main.id == id => {
            profile = profile
                .badge(badge("Main", Tone::Neutral))
                .subtitle(format!("Main of {} characters", m.characters.len()));
        }
        Some(m) => {
            profile = profile.subtitle(format!(
                "One of {}'s {} characters",
                m.main.name,
                m.characters.len()
            ));
        }
        None if access.viewer.main.id == id => {
            profile = profile.badge(badge("Main", Tone::Neutral));
        }
        None => {}
    }
    let corp_names = crate::pages::names_of(
        member
            .iter()
            .flat_map(|m| m.characters.iter().map(|c| c.character.corporation_id))
            .collect(),
    )?;
    let corp_names = &corp_names;
    let owner_characters = with_rows(
        Table::new(vec![
            Column::text("Character"),
            Column::text("Corporation"),
            Column::text(""),
        ])
        .empty("Tether doesn't say whose character this is."),
        member.into_iter().flat_map(|m| {
            let mut characters: Vec<_> = m.characters.iter().collect();
            characters
                .sort_by_key(|c| (c.character.id != m.main.id, c.character.name.to_lowercase()));
            characters.into_iter().map(move |c| {
                let ch = &c.character;
                let mut status = Vec::new();
                if ch.id == m.main.id {
                    status.push("Main");
                }
                if !c.registered {
                    status.push("Unregistered");
                }
                vec![
                    if c.registered && ch.id != id && access.may_open(ch.id) {
                        character(ch.id, ch.name.clone())
                            .link(format!("character/{}", ch.id))
                            .into()
                    } else {
                        character(ch.id, ch.name.clone()).into()
                    },
                    corporation(
                        ch.corporation_id,
                        corp_names
                            .get(&ch.corporation_id)
                            .cloned()
                            .unwrap_or_default(),
                    )
                    .into(),
                    match (ch.id == m.main.id, c.registered) {
                        (_, false) => badge(status.join(", "), Tone::Warning).into(),
                        (true, true) => badge("Main", Tone::Neutral).into(),
                        (false, true) => Value::from(""),
                    },
                ]
            })
        }),
    );
    let is_shared = boolean(c, 23);
    if is_shared {
        profile = profile.badge(badge("Shared", Tone::Neutral));
    }
    let ids: Db = id.to_string().into();
    let training = crate::pages::training(&ids)?;
    let training = training.first().map(|(_, t)| t);
    let fact_or = |v: Option<Value>| v.unwrap_or_else(|| "".into());
    profile = profile
        .fact("System", fact_or(opt_int(c, 3).map(|_| text(c, 4).into())))
        .fact(
            "Docked at",
            match text(c, 7).as_str() {
                "space" => "In space".into(),
                _ => fact_or(opt_int(c, 5).map(|_| text(c, 6).into())),
            },
        )
        .fact(
            "Ship",
            fact_or(opt_int(c, 8).map(|s| item_type(s, text(c, 9)).into())),
        )
        .fact("Ship name", text(c, 10))
        .fact("Skill points", int(c, 0))
        .fact("Unallocated", int(c, 1))
        .fact("Wallet", fact_or(opt_float(c, 2).map(isk)))
        .fact(
            "Security status",
            fact_or(opt_float(c, 11).map(|s| format!("{s:.1}").into())),
        )
        .fact("Born", time_or_blank(c, 12))
        // aa-memberaudit's online status.
        .fact("Last login", time_or_blank(c, 24))
        .fact(
            "Training",
            training.map_or_else(
                || badge("Not training", Tone::Warning).into(),
                |t| t.value(),
            ),
        )
        .fact(
            "Queue ends",
            fact_or(
                when(c, 21)
                    .filter(|t| *t > Utc::now())
                    .map(|t| countdown(rfc3339(t))),
            ),
        )
        .fact(
            "Home station",
            fact_or(opt_int(c, 14).map(|_| text(c, 15).into())),
        )
        .fact("Jump clones", int(c, 20));
    if let Some(f) = opt_int(c, 16).filter(|f| *f > 0) {
        profile = profile.fact("Faction", faction(f, text(c, 17)));
    }
    profile = profile.fact("Last update", time_or_blank(c, 13));
    let asked = when(c, 19).filter(|t| Utc::now() - *t < UPDATE_WAIT);
    let done = when(c, 22);
    profile = profile.fact(
        "Update",
        if let Some(asked) = asked {
            if done.is_some_and(|d| d >= asked) {
                Value::from(badge("Updated", Tone::Success))
            } else {
                Value::from(badge("Update queued", Tone::Neutral))
            }
        } else {
            action("Update now", "update_character")
                .field("character", id.to_string())
                .into()
        },
    );

    // Its pilot shares it, or stops (aa-memberaudit's launcher).
    if access.owns(id) {
        if is_shared {
            profile = profile.fact(
                "Sharing",
                Value::from(
                    action("Stop sharing", "unshare_character").field("character", id.to_string()),
                ),
            );
        } else if access.share {
            profile = profile.fact(
                "Sharing",
                Value::from(
                    action("Share", "share_character")
                        .field("character", id.to_string())
                        .confirm(
                            "Recruiters will see this character's whole sheet, mail included, \
                             and the names of your main and other characters, until you stop \
                             sharing it.",
                        ),
                ),
            );
        }
    }

    let settings = crate::settings::for_page()?;
    let fresh = Freshness::of(id)?;
    let history = query(
        &format!(
            "SELECT h.corporation_id, {corp}, h.start_date, is_deleted, \
                    lead(h.start_date) OVER (ORDER BY h.start_date) \
             FROM corporation_history h WHERE h.character_id = $1 ORDER BY h.start_date DESC LIMIT $2",
            corp = name_of("h.corporation_id")
        ),
        &[id.into(), MAX_ROWS.into()],
    )?;
    let now = Utc::now();
    let history = with_rows(
        Table::new(vec![
            Column::text("Corporation"),
            Column::numeric("Joined"),
            Column::numeric("Left"),
            Column::numeric("Days"),
        ])
        .empty("No corporation history read yet."),
        history.iter().map(|r| {
            let start = when(r, 2);
            let end = when(r, 4);
            let days = start.map_or(0, |s| (end.unwrap_or(now) - s).num_days());
            let mut corp: Value = corporation(int(r, 0), text(r, 1)).into();
            if boolean(r, 3) {
                corp = format!("{} (closed)", text(r, 1)).into();
            }
            vec![corp, time_or_blank(r, 2), time_or_blank(r, 4), days.into()]
        }),
    );
    // Roles only when the Settings read them (aa-memberaudit's
    // MEMBERAUDIT_FEATURE_ROLES_ENABLED, off by default).
    let roles = if settings.roles {
        query(
            "SELECT scope, role FROM roles WHERE character_id = $1 ORDER BY scope, role",
            &[id.into()],
        )?
    } else {
        Vec::new()
    };
    let roles = with_rows(
        Table::new(vec![Column::text("Role"), Column::text("Where")])
            .title("Corporation roles")
            .empty("No corporation roles."),
        roles.iter().map(|r| {
            vec![
                humanize(&text(r, 1)).into(),
                match text(r, 0).as_str() {
                    "roles_at_hq" => "At headquarters",
                    "roles_at_base" => "At base",
                    "roles_at_other" => "Elsewhere",
                    _ => "Everywhere",
                }
                .into(),
            ]
        }),
    );
    let titles = query(
        "SELECT name FROM titles WHERE character_id = $1 ORDER BY title_id",
        &[id.into()],
    )?;
    let titles = with_rows(
        Table::new(vec![Column::text("Title")])
            .title("Titles")
            .empty("No titles."),
        titles.iter().map(|r| vec![text(r, 0).into()]),
    );
    let kills = query(
        &format!(
            "SELECT k.at, k.victim_id, {victim}, k.ship_type_id, {ship}, k.victim_corporation_id, {corp}, \
                    k.solar_system_id, {system}, coalesce(k.attackers, 0), k.killmail_id \
             FROM killmails k WHERE k.character_id = $1 AND k.at IS NOT NULL \
             ORDER BY k.at DESC LIMIT $2",
            victim = name_of("k.victim_id"),
            ship = name_of("k.ship_type_id"),
            corp = name_of("k.victim_corporation_id"),
            system = name_of("k.solar_system_id"),
        ),
        &[id.into(), MAX_ROWS.into()],
    )?;
    let kills = with_rows(
        Table::new(vec![
            Column::numeric("When"),
            Column::text(""),
            Column::text("Ship"),
            Column::text("Victim"),
            Column::text("Victim's corporation"),
            Column::text("System"),
            Column::numeric("Attackers"),
        ])
        .empty("No killmails in the last 90 days."),
        kills.iter().map(|r| {
            let loss = opt_int(r, 1) == Some(id);
            vec![
                time_or_blank(r, 0),
                if loss {
                    badge("Loss", Tone::Danger).into()
                } else {
                    badge("Kill", Tone::Success).into()
                },
                match opt_int(r, 3) {
                    Some(s) => item_type(s, text(r, 4)).into(),
                    None => "".into(),
                },
                match opt_int(r, 1) {
                    Some(v) => character(v, text(r, 2)).into(),
                    None => "A structure".into(),
                },
                match opt_int(r, 5) {
                    Some(corp) => corporation(corp, text(r, 6)).into(),
                    None => "".into(),
                },
                text(r, 8).into(),
                int(r, 9).into(),
            ]
        }),
    );
    let bio = text(c, 18);
    let mut bio_sections: Vec<Section> = crate::mail::paragraphs(&bio)
        .into_iter()
        .map(Section::Text)
        .take(10)
        .collect();
    if bio_sections.is_empty() {
        bio_sections.push(Section::Text("No bio.".to_owned()));
    }
    bio_sections.push(fresh.line(&["public"]));
    let mut page = sheet_page(who, "Overview");
    if let Some(note) = note {
        page = page.text(note);
    }
    Ok(page
        .profile(profile)
        .tab(
            "Corporation history",
            vec![Section::Table(history), fresh.line(&["history"])],
        )
        .tab(
            if settings.roles {
                "Roles and titles"
            } else {
                "Titles"
            },
            if settings.roles {
                vec![
                    Section::Table(roles),
                    Section::Table(titles),
                    fresh.line(&["roles", "titles"]),
                ]
            } else {
                vec![Section::Table(titles), fresh.line(&["titles"])]
            },
        )
        .tab(
            "Killmails",
            vec![Section::Table(kills), fresh.line(&["killmails"])],
        )
        .tab("Bio", bio_sections)
        .tab("Characters", vec![Section::Table(owner_characters)]))
}

/// `station_manager` as "Station manager".
fn humanize(code: &str) -> String {
    let words = code.replace('_', " ");
    let mut chars = words.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

// ---- Skills ----------------------------------------------------------------

fn skills(who: &Subject, set: &str) -> Result<Page, PageError> {
    let id = who.id;
    let fresh = Freshness::of(id)?;
    let head = query(
        "SELECT coalesce(total_sp, 0), coalesce(unallocated_sp, 0), \
                (SELECT count(*) FROM skills WHERE character_id = c.character_id), \
                (SELECT count(*) FILTER (WHERE trained_level = 5) FROM skills WHERE character_id = c.character_id), \
                (SELECT max(finish) FROM queue WHERE character_id = c.character_id) \
         FROM characters c WHERE c.character_id = $1",
        &[id.into()],
    )?;
    let head = head.first().ok_or(PageError::NotFound)?;
    let queue = query(
        &format!(
            "SELECT q.position, q.skill_id, {skill}, q.level, q.start, q.finish FROM queue q \
             WHERE q.character_id = $1 ORDER BY q.position LIMIT $2",
            skill = name_of("q.skill_id")
        ),
        &[id.into(), MAX_ROWS.into()],
    )?;
    let now = Utc::now();
    let queue_table = with_rows(
        Table::new(vec![
            Column::numeric("#"),
            Column::text("Skill"),
            Column::text("Level"),
            Column::text("Progress"),
            Column::numeric("Finishes in"),
            Column::numeric("Finishes"),
        ])
        .empty("The queue is empty."),
        queue.iter().map(|r| {
            let (start, finish) = (when(r, 4), when(r, 5));
            let label = format!("{} {}", text(r, 2), roman(int(r, 3)));
            let bar = crate::pages::Training {
                skill_id: int(r, 1),
                label: label.clone(),
                start,
                finish,
            };
            // EVE's squares: the levels below this one trained, this one in
            // training.
            let level = int(r, 3).clamp(1, 5) as u8;
            vec![
                (int(r, 0) + 1).into(),
                item_type(bar.skill_id, label).into(),
                levels(level - 1, Some(level)),
                match finish {
                    Some(f) if f <= now => badge("Completed", Tone::Success).into(),
                    Some(_) if start.is_some_and(|s| s <= now) => bar.value(),
                    Some(_) => badge("Queued", Tone::Neutral).into(),
                    None => badge("Paused", Tone::Warning).into(),
                },
                match finish {
                    Some(f) => countdown(rfc3339(f)),
                    None => "".into(),
                },
                time_or_blank(r, 5),
            ]
        }),
    );
    // Skills by group (once the groups are known; "Skills" until then).
    let skills = query(
        &format!(
            "SELECT coalesce(g.name, 'Skills'), s.skill_id, {skill}, s.trained_level, s.active_level, s.sp \
             FROM skills s LEFT JOIN skill_types t ON t.type_id = s.skill_id \
             LEFT JOIN skill_groups g ON g.group_id = t.group_id \
             WHERE s.character_id = $1 ORDER BY 1, 3",
            skill = name_of("s.skill_id")
        ),
        &[id.into()],
    )?;
    let mut groups: Vec<(String, Vec<Vec<Value>>, i64)> = Vec::new();
    for r in &skills {
        let group = text(r, 0);
        let row = vec![
            item_type(int(r, 1), text(r, 2)).into(),
            levels(int(r, 3).clamp(0, 5) as u8, None),
            if int(r, 4) < int(r, 3) {
                badge(format!("{} (Alpha)", roman(int(r, 4))), Tone::Warning).into()
            } else {
                roman(int(r, 4)).into()
            },
            int(r, 5).into(),
        ];
        match groups.last_mut() {
            Some((name, rows, sp)) if *name == group => {
                rows.push(row);
                *sp += int(r, 5);
            }
            _ => groups.push((group, vec![row], int(r, 5))),
        }
    }
    let mut skill_sections: Vec<Section> = groups
        .into_iter()
        .take(30)
        .map(|(name, rows, sp)| {
            Section::Table(with_rows(
                Table::new(vec![
                    Column::text("Skill"),
                    Column::text("Trained"),
                    Column::text("Active"),
                    Column::numeric("Skill points"),
                ])
                .title(format!("{name} · {} SP", grouped(sp))),
                rows,
            ))
        })
        .collect();
    if skill_sections.is_empty() {
        skill_sections.push(Section::Text("No skills read yet.".to_owned()));
    }
    skill_sections.push(fresh.line(&["skills"]));
    let sets = if who.skill_sets {
        crate::sets::for_character(id)?
    } else {
        Vec::new()
    };
    let yes_no = |yes: bool| -> Value {
        if yes {
            badge("Yes", Tone::Success).into()
        } else {
            badge("No", Tone::Neutral).into()
        }
    };
    let none_or = |missing: &[String]| -> Value {
        if missing.is_empty() {
            "".into()
        } else {
            clip(&missing.join(", "), 1500).into()
        }
    };
    let sets_table = with_rows(
        Table::new(vec![
            Column::text("Group"),
            Column::text("Skill set"),
            Column::text("Doctrine"),
            Column::text("Required skills"),
            Column::text("Missing"),
            Column::text("Recommended skills"),
            Column::text("Missing"),
        ])
        .empty("No skill sets yet: officers add them under Skill Sets."),
        sets.iter().map(|s| {
            let open = format!("character/{id}/skills?set={}", s.set.id);
            vec![
                s.group.clone().into(),
                match &s.set.ship {
                    Some((ship, _)) => item_type(*ship, s.set.name.clone()).link(open).into(),
                    None => link(s.set.name.clone(), open).into(),
                },
                yes_no(s.doctrine),
                yes_no(s.missing_required.is_empty()),
                none_or(&s.missing_required),
                yes_no(s.missing_recommended.is_empty()),
                none_or(&s.missing_recommended),
            ]
        }),
    );
    let attributes = query(
        "SELECT charisma, intelligence, memory, perception, willpower, bonus_remaps, last_remap, remap_cooldown \
         FROM attributes WHERE character_id = $1",
        &[id.into()],
    )?;
    let attributes = match attributes.first() {
        Some(a) => Section::Card(
            Card::new("Attributes")
                .field("Charisma", int(a, 0))
                .field("Intelligence", int(a, 1))
                .field("Memory", int(a, 2))
                .field("Perception", int(a, 3))
                .field("Willpower", int(a, 4))
                .field("Bonus remaps", int(a, 5))
                .field("Last remap", time_or_blank(a, 6))
                .field(
                    "Next remap",
                    match when(a, 7) {
                        Some(t) if t > now => countdown(rfc3339(t)),
                        _ => "Available".into(),
                    },
                ),
        ),
        None => Section::Text("Attributes not read yet.".to_owned()),
    };
    let queue_end = when(head, 4).filter(|t| *t > now);
    let page = sheet_page(who, "Skills")
        .stats(vec![
            fresh.stat("skills", Stat::new("Skill points", int(head, 0))),
            fresh.stat("skills", Stat::new("Unallocated", int(head, 1))),
            fresh.stat(
                "skills",
                Stat::new("Skills", int(head, 2)).caption(format!("{} at V", int(head, 3))),
            ),
            fresh.stat(
                "skills",
                Stat::new(
                    "Queue ends",
                    queue_end.map_or_else(
                        || badge("Empty", Tone::Warning).into(),
                        |t| countdown(rfc3339(t)),
                    ),
                ),
            ),
        ])
        .tab(
            "Skill queue",
            vec![Section::Table(queue_table), fresh.line(&["skills"])],
        )
        .tab("Skills", skill_sections);
    // aa-memberaudit's Skill Sets tab needs view_skill_sets; a set's
    // name opens its skills beside it (aa-memberaudit's details).
    let page = if who.skill_sets {
        let page = page.tab("Skill sets", vec![Section::Table(sets_table)]);
        let chosen = set
            .parse::<i64>()
            .ok()
            .and_then(|n| sets.iter().find(|s| s.set.id == n));
        match chosen {
            Some(chosen) => page.panel(crate::sets::sheet_panel(id, chosen)?),
            None => page,
        }
    } else {
        page
    };
    Ok(page.tab("Attributes", vec![attributes, fresh.line(&["attributes"])]))
}

/// `1,234,567`.
pub(crate) fn grouped(n: i64) -> String {
    let digits = n.unsigned_abs().to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    if n < 0 { format!("-{out}") } else { out }
}

// ---- Assets ----------------------------------------------------------------

/// Assets whose place is not another of the character's items, with every
/// item inside them: `root` is that place, `container` the item holding
/// one (none for the top level).
const TREE: &str = "WITH RECURSIVE tree AS ( \
       SELECT a.item_id, a.type_id, a.quantity, a.location_flag, a.location_id AS root, \
              NULL::bigint AS container, 0 AS depth \
       FROM assets a WHERE a.character_id = $1 AND NOT EXISTS ( \
         SELECT 1 FROM assets p WHERE p.character_id = $1 AND p.item_id = a.location_id) \
       UNION ALL \
       SELECT a.item_id, a.type_id, a.quantity, a.location_flag, t.root, t.type_id, t.depth + 1 \
       FROM assets a JOIN tree t ON a.location_id = t.item_id \
       WHERE a.character_id = $1 AND t.depth < 6) ";

fn assets(who: &Subject, location: Option<i64>) -> Result<Page, PageError> {
    let id = who.id;
    let fresh = Freshness::of(id)?;
    let places = query(
        &format!(
            "{TREE} SELECT t.root, {place}, count(*), coalesce(sum(t.quantity), 0)::bigint \
             FROM tree t GROUP BY t.root ORDER BY count(*) DESC LIMIT $2",
            place = name_of("t.root")
        ),
        &[id.into(), MAX_ROWS.into()],
    )?;
    let total: i64 = places.iter().map(|r| int(r, 2)).sum();
    let chosen = location.or_else(|| places.first().map(|r| int(r, 0)));
    if location.is_some() && !places.iter().any(|r| Some(int(r, 0)) == chosen) {
        return Err(PageError::NotFound);
    }
    let places_table = with_rows(
        Table::new(vec![
            Column::text("Location"),
            Column::numeric("Items"),
            Column::numeric("Quantity"),
        ])
        .title("Locations")
        .empty("No assets read yet."),
        places.iter().map(|r| {
            let root = int(r, 0);
            vec![
                if Some(root) == chosen {
                    Value::from(text(r, 1))
                } else {
                    link(text(r, 1), format!("character/{id}/assets/{root}")).into()
                },
                int(r, 2).into(),
                int(r, 3).into(),
            ]
        }),
    );
    let mut page = sheet_page(who, "Assets").stats(vec![
        fresh.stat("assets", Stat::new("Items", total)),
        fresh.stat("assets", Stat::new("Locations", count(places.len()))),
    ]);
    page = page.table(places_table);
    if let Some(root) = chosen {
        let items = query(
            &format!(
                "{TREE} SELECT t.type_id, {item}, t.quantity, t.location_flag, t.container, {container}, \
                        (SELECT {place}) FROM tree t WHERE t.root = $2 \
                 ORDER BY t.container NULLS FIRST, 2 LIMIT 500",
                item = name_of("t.type_id"),
                container = name_of("t.container"),
                place = name_of("$2::bigint"),
            ),
            &[id.into(), root.into()],
        )?;
        let place = items.first().map(|r| text(r, 6)).unwrap_or_default();
        page = page.table(with_rows(
            Table::new(vec![
                Column::text("Item"),
                Column::numeric("Quantity"),
                Column::text("Where"),
                Column::text("In"),
            ])
            .title(if items.len() >= 500 {
                format!("{place}: the first 500 items")
            } else {
                place
            })
            .empty("Nothing here."),
            items.iter().map(|r| {
                vec![
                    item_type(int(r, 0), text(r, 1)).into(),
                    int(r, 2).into(),
                    humanize_flag(&text(r, 3)).into(),
                    match opt_int(r, 4) {
                        Some(c) => item_type(c, text(r, 5)).into(),
                        None => "".into(),
                    },
                ]
            }),
        ));
    }
    Ok(page.section(fresh.line(&["assets"])))
}

/// `CorpSAG1` and `HiSlot0` as ESI names them, a little easier to read.
fn humanize_flag(flag: &str) -> String {
    let mut out = String::new();
    for (i, c) in flag.chars().enumerate() {
        if i > 0 && c.is_ascii_uppercase() {
            out.push(' ');
        }
        out.push(c);
    }
    out
}

// ---- Wallet ----------------------------------------------------------------

fn wallet(who: &Subject) -> Result<Page, PageError> {
    let id = who.id;
    let fresh = Freshness::of(id)?;
    let head = query(
        "SELECT c.wallet, \
                (SELECT coalesce(sum(amount) FILTER (WHERE amount > 0), 0) FROM journal \
                   WHERE character_id = c.character_id AND at > now() - interval '30 days'), \
                (SELECT coalesce(sum(amount) FILTER (WHERE amount < 0), 0) FROM journal \
                   WHERE character_id = c.character_id AND at > now() - interval '30 days'), \
                (SELECT coalesce(sum(points), 0)::bigint FROM loyalty WHERE character_id = c.character_id) \
         FROM characters c WHERE c.character_id = $1",
        &[id.into()],
    )?;
    let head = head.first().ok_or(PageError::NotFound)?;
    let journal = query(
        &format!(
            "SELECT j.at, j.ref_type, j.first_party_id, {first}, j.second_party_id, {second}, \
                    coalesce(j.amount, 0), coalesce(j.balance, 0), j.description \
             FROM journal j WHERE j.character_id = $1 ORDER BY j.at DESC LIMIT $2",
            first = named("j.first_party_id"),
            second = named("j.second_party_id"),
        ),
        &[id.into(), MAX_ROWS.into()],
    )?;
    let party = |r: &[Db], i: usize| match opt_int(r, i) {
        Some(p) => entity(p, text(r, i + 1), &text(r, i + 2)),
        None => "".into(),
    };
    let journal = with_rows(
        Table::new(vec![
            Column::numeric("Date"),
            Column::text("Type"),
            Column::text("First party"),
            Column::text("Second party"),
            Column::numeric("Amount"),
            Column::numeric("Balance"),
            Column::text("Description"),
        ])
        .empty("No journal entries read yet."),
        journal.iter().map(|r| {
            vec![
                time_or_blank(r, 0),
                humanize(&text(r, 1)).into(),
                party(r, 2),
                party(r, 5),
                isk(float(r, 8)),
                isk(float(r, 9)),
                text(r, 10).into(),
            ]
        }),
    );
    let transactions = query(
        &format!(
            "SELECT t.at, t.type_id, {item}, t.quantity, t.unit_price, t.is_buy, t.client_id, {client}, {place} \
             FROM transactions t WHERE t.character_id = $1 ORDER BY t.at DESC LIMIT $2",
            item = name_of("t.type_id"),
            client = named("t.client_id"),
            place = name_of("t.location_id"),
        ),
        &[id.into(), MAX_ROWS.into()],
    )?;
    let transactions = with_rows(
        Table::new(vec![
            Column::numeric("Date"),
            Column::text(""),
            Column::text("Item"),
            Column::numeric("Quantity"),
            Column::numeric("Price"),
            Column::numeric("Total"),
            Column::text("Client"),
            Column::text("Where"),
        ])
        .empty("No market transactions read yet."),
        transactions.iter().map(|r| {
            let (quantity, price) = (int(r, 3), float(r, 4));
            vec![
                time_or_blank(r, 0),
                if boolean(r, 5) {
                    badge("Buy", Tone::Neutral).into()
                } else {
                    badge("Sell", Tone::Success).into()
                },
                item_type(int(r, 1), text(r, 2)).into(),
                quantity.into(),
                isk(price),
                isk(price * quantity as f64),
                entity(int(r, 6), text(r, 7), &text(r, 8)),
                text(r, 9).into(),
            ]
        }),
    );
    let orders = query(
        &format!(
            "SELECT o.type_id, {item}, o.is_buy, o.price, o.volume_remain, o.volume_total, {place}, \
                    o.issued, o.issued + make_interval(days => o.duration) \
             FROM orders o WHERE o.character_id = $1 ORDER BY o.issued DESC LIMIT $2",
            item = name_of("o.type_id"),
            place = name_of("o.location_id"),
        ),
        &[id.into(), MAX_ROWS.into()],
    )?;
    let orders = with_rows(
        Table::new(vec![
            Column::text("Item"),
            Column::text(""),
            Column::numeric("Price"),
            Column::numeric("Remaining"),
            Column::numeric("Of"),
            Column::text("Where"),
            Column::numeric("Issued"),
            Column::numeric("Expires in"),
        ])
        .empty("No open market orders."),
        orders.iter().map(|r| {
            vec![
                item_type(int(r, 0), text(r, 1)).into(),
                if boolean(r, 2) {
                    badge("Buy", Tone::Neutral).into()
                } else {
                    badge("Sell", Tone::Success).into()
                },
                isk(float(r, 3)),
                int(r, 4).into(),
                int(r, 5).into(),
                text(r, 6).into(),
                time_or_blank(r, 7),
                when(r, 8).map_or_else(|| "".into(), |t| countdown(rfc3339(t))),
            ]
        }),
    );
    let contracts = query(
        &format!(
            "SELECT k.contract_id, k.kind, k.title, k.issuer_id, {issuer}, k.assignee_id, {assignee}, \
                    k.status, k.issued, k.expires, k.completed, coalesce(k.price, 0) + coalesce(k.reward, 0), \
                    (SELECT count(*) FROM contract_items i WHERE i.character_id = k.character_id \
                       AND i.contract_id = k.contract_id) \
             FROM contracts k WHERE k.character_id = $1 ORDER BY k.issued DESC LIMIT $2",
            issuer = named("k.issuer_id"),
            assignee = named("k.assignee_id"),
        ),
        &[id.into(), MAX_ROWS.into()],
    )?;
    let contracts = with_rows(
        Table::new(vec![
            Column::text("Contract"),
            Column::text("Type"),
            Column::text("From"),
            Column::text("To"),
            Column::text("Status"),
            Column::numeric("Issued"),
            Column::numeric("Expires in"),
            Column::numeric("ISK"),
            Column::numeric("Items"),
        ])
        .empty("No contracts in the last 30 days."),
        contracts.iter().map(|r| {
            let contract = int(r, 0);
            let title = text(r, 2);
            let title = if title.is_empty() {
                format!("#{contract}")
            } else {
                title
            };
            let status = text(r, 9);
            vec![
                link(title, format!("character/{id}/contract/{contract}")).into(),
                humanize(&text(r, 1)).into(),
                entity(int(r, 3), text(r, 4), &text(r, 5)),
                if int(r, 6) > 0 {
                    entity(int(r, 6), text(r, 7), &text(r, 8))
                } else {
                    "Public".into()
                },
                badge(
                    humanize(&status),
                    match status.as_str() {
                        "outstanding" | "in_progress" => Tone::Neutral,
                        "finished" | "finished_issuer" | "finished_contractor" => Tone::Success,
                        "failed" | "rejected" | "deleted" => Tone::Danger,
                        _ => Tone::Warning,
                    },
                )
                .into(),
                time_or_blank(r, 10),
                match (when(r, 12), when(r, 11)) {
                    (None, Some(t)) if t > Utc::now() => countdown(rfc3339(t)),
                    _ => "".into(),
                },
                isk(float(r, 13)),
                int(r, 14).into(),
            ]
        }),
    );
    let loyalty = query(
        &format!(
            "SELECT l.corporation_id, {corp}, l.points FROM loyalty l WHERE l.character_id = $1 \
             ORDER BY l.points DESC LIMIT $2",
            corp = name_of("l.corporation_id")
        ),
        &[id.into(), MAX_ROWS.into()],
    )?;
    let loyalty = with_rows(
        Table::new(vec![
            Column::text("Corporation"),
            Column::numeric("Loyalty points"),
        ])
        .empty("No loyalty points."),
        loyalty
            .iter()
            .map(|r| vec![corporation(int(r, 0), text(r, 1)).into(), int(r, 2).into()]),
    );
    Ok(sheet_page(who, "Wallet")
        .stats(vec![
            fresh.stat(
                "wallet",
                Stat::new("Balance", opt_float(head, 0).map_or_else(|| "".into(), isk)),
            ),
            fresh.stat(
                "journal",
                Stat::new("Income", isk(float(head, 1))).caption("the last 30 days"),
            ),
            fresh.stat(
                "journal",
                Stat::new("Spending", isk(float(head, 2))).caption("the last 30 days"),
            ),
            fresh.stat("loyalty", Stat::new("Loyalty points", int(head, 3))),
        ])
        .tab(
            "Journal",
            vec![Section::Table(journal), fresh.line(&["journal"])],
        )
        .tab(
            "Transactions",
            vec![Section::Table(transactions), fresh.line(&["transactions"])],
        )
        .tab(
            "Market orders",
            vec![Section::Table(orders), fresh.line(&["orders"])],
        )
        .tab(
            "Contracts",
            vec![Section::Table(contracts), fresh.line(&["contracts"])],
        )
        .tab(
            "Loyalty points",
            vec![Section::Table(loyalty), fresh.line(&["loyalty"])],
        ))
}

fn contract_page(who: &Subject, contract: i64) -> Result<Page, PageError> {
    let id = who.id;
    let rows = query(
        &format!(
            "SELECT k.kind, k.title, k.issuer_id, {issuer}, k.assignee_id, {assignee}, k.acceptor_id, {acceptor}, \
                    k.status, k.issued, k.expires, k.completed, k.price, k.reward, k.collateral, k.volume, \
                    {start}, {end_} \
             FROM contracts k WHERE k.character_id = $1 AND k.contract_id = $2",
            issuer = named("k.issuer_id"),
            assignee = named("k.assignee_id"),
            acceptor = named("k.acceptor_id"),
            start = name_of("k.start_location_id"),
            end_ = name_of("k.end_location_id"),
        ),
        &[id.into(), contract.into()],
    )?;
    let k = rows.first().ok_or(PageError::NotFound)?;
    let money = |i: usize| opt_float(k, i).map_or_else(|| "".into(), isk);
    let mut card = Card::new(if text(k, 1).is_empty() {
        format!("Contract #{contract}")
    } else {
        text(k, 1)
    })
    .field("Type", humanize(&text(k, 0)))
    .field("Status", humanize(&text(k, 11)))
    .field("From", entity(int(k, 2), text(k, 3), &text(k, 4)))
    .field(
        "To",
        if int(k, 5) > 0 {
            entity(int(k, 5), text(k, 6), &text(k, 7))
        } else {
            "Public".into()
        },
    )
    .field(
        "Accepted by",
        if int(k, 8) > 0 {
            entity(int(k, 8), text(k, 9), &text(k, 10))
        } else {
            "".into()
        },
    )
    .field("Issued", time_or_blank(k, 12))
    .field("Expires", time_or_blank(k, 13))
    .field("Completed", time_or_blank(k, 14))
    .field("Price", money(15))
    .field("Reward", money(16))
    .field("Collateral", money(17));
    if text(k, 0) == "courier" {
        card = card
            .field("From station", text(k, 19))
            .field("To station", text(k, 20))
            .field(
                "Volume",
                opt_float(k, 18).map_or_else(|| Value::from(""), |v| format!("{v:.0} m³").into()),
            );
    }
    let items = query(
        &format!(
            "SELECT i.type_id, {item}, i.quantity, i.is_included FROM contract_items i \
             WHERE i.character_id = $1 AND i.contract_id = $2 ORDER BY i.is_included DESC, 2 LIMIT 500",
            item = name_of("i.type_id")
        ),
        &[id.into(), contract.into()],
    )?;
    let items = with_rows(
        Table::new(vec![
            Column::text("Item"),
            Column::numeric("Quantity"),
            Column::text(""),
        ])
        .title("Items")
        .empty("No items (or not read yet)."),
        items.iter().map(|r| {
            vec![
                item_type(int(r, 0), text(r, 1)).into(),
                int(r, 2).into(),
                if boolean(r, 3) {
                    "Offered".into()
                } else {
                    "Asked for".into()
                },
            ]
        }),
    );
    Ok(sheet_page(who, "A contract").card(card).table(items))
}

// ---- Clones ----------------------------------------------------------------

fn clones(who: &Subject) -> Result<Page, PageError> {
    let id = who.id;
    let fresh = Freshness::of(id)?;
    let head = query(
        &format!(
            "SELECT {home}, c.home_location_id, c.last_clone_jump, c.last_station_change \
             FROM characters c WHERE c.character_id = $1",
            home = name_of("c.home_location_id")
        ),
        &[id.into()],
    )?;
    let head = head.first().ok_or(PageError::NotFound)?;
    let implants = query(
        &format!(
            "SELECT m.type_id, {implant} FROM implants m WHERE m.character_id = $1 ORDER BY 2",
            implant = name_of("m.type_id")
        ),
        &[id.into()],
    )?;
    let active = with_rows(
        Table::new(vec![Column::text("Implant")])
            .title("Active implants")
            .empty("No implants."),
        implants
            .iter()
            .map(|r| vec![item_type(int(r, 0), text(r, 1)).into()]),
    );
    let jump_clones = query(
        &format!(
            "SELECT cl.jump_clone_id, {place}, \
                    coalesce((SELECT json_agg(json_build_array(i.t::bigint, {implant})) \
                              FROM jsonb_array_elements_text(cl.implants) AS i(t)), '[]')::text \
             FROM clones cl WHERE cl.character_id = $1 ORDER BY 2 LIMIT 20",
            place = name_of("cl.location_id"),
            implant = name_of("i.t::bigint")
        ),
        &[id.into()],
    )?;
    let mut page = sheet_page(who, "Clones and implants")
        .stats(
            [
                Stat::new("Jump clones", count(jump_clones.len())),
                Stat::new("Implants", count(implants.len())),
                Stat::new("Last clone jump", time_or_blank(head, 2)),
                Stat::new("Home station", text(head, 0)).caption(format!(
                    "changed {}",
                    when(head, 3)
                        .map_or_else(|| "never".to_owned(), |t| t.format("%Y-%m-%d").to_string())
                )),
            ]
            .into_iter()
            .map(|stat| fresh.stat("clones", stat))
            .collect(),
        )
        .table(active);
    for r in &jump_clones {
        let implants: Vec<(i64, String)> =
            serde_json::from_str::<Vec<(i64, String)>>(&text(r, 2)).unwrap_or_default();
        page = page.table(with_rows(
            Table::new(vec![Column::text("Implant")])
                .title(format!("Jump clone in {}", text(r, 1)))
                .empty("No implants."),
            implants
                .into_iter()
                .map(|(t, name)| vec![item_type(t, name).into()]),
        ));
    }
    Ok(page.section(fresh.line(&["clones"])))
}

// ---- Industry --------------------------------------------------------------

fn industry(who: &Subject) -> Result<Page, PageError> {
    let id = who.id;
    let fresh = Freshness::of(id)?;
    let jobs = query(
        &format!(
            "SELECT j.activity_id, j.blueprint_type_id, {bp}, j.product_type_id, {product}, j.runs, j.status, \
                    {facility}, j.ends FROM industry_jobs j WHERE j.character_id = $1 \
             ORDER BY j.ends DESC LIMIT $2",
            bp = name_of("j.blueprint_type_id"),
            product = name_of("j.product_type_id"),
            facility = name_of("j.facility_id"),
        ),
        &[id.into(), MAX_ROWS.into()],
    )?;
    let now = Utc::now();
    let jobs = with_rows(
        Table::new(vec![
            Column::text("Activity"),
            Column::text("Blueprint"),
            Column::text("Product"),
            Column::numeric("Runs"),
            Column::text("Status"),
            Column::text("Facility"),
            Column::numeric("Ends in"),
        ])
        .empty("No industry jobs in the last 90 days."),
        jobs.iter().map(|r| {
            let status = text(r, 6);
            vec![
                activity(int(r, 0)).into(),
                item_type(int(r, 1), text(r, 2)).into(),
                match opt_int(r, 3) {
                    Some(p) => item_type(p, text(r, 4)).into(),
                    None => "".into(),
                },
                int(r, 5).into(),
                badge(
                    humanize(&status),
                    match status.as_str() {
                        "active" => Tone::Neutral,
                        "ready" => Tone::Accent,
                        "delivered" => Tone::Success,
                        _ => Tone::Warning,
                    },
                )
                .into(),
                text(r, 7).into(),
                match when(r, 8) {
                    Some(t) if t > now && status == "active" => countdown(rfc3339(t)),
                    _ => "".into(),
                },
            ]
        }),
    );
    let blueprints = query(
        &format!(
            "SELECT b.type_id, {bp}, b.material_efficiency, b.time_efficiency, b.runs, b.quantity, {place} \
             FROM blueprints b WHERE b.character_id = $1 ORDER BY 2 LIMIT $2",
            bp = name_of("b.type_id"),
            place = name_of("b.location_id"),
        ),
        &[id.into(), MAX_ROWS.into()],
    )?;
    let blueprints = with_rows(
        Table::new(vec![
            Column::text("Blueprint"),
            Column::text("Kind"),
            Column::numeric("ME"),
            Column::numeric("TE"),
            Column::numeric("Runs"),
            Column::text("Where"),
        ])
        .empty("No blueprints read yet."),
        blueprints.iter().map(|r| {
            let runs = int(r, 4);
            vec![
                item_type(int(r, 0), text(r, 1)).into(),
                if runs < 0 {
                    badge("Original", Tone::Neutral).into()
                } else {
                    badge("Copy", Tone::Neutral).into()
                },
                int(r, 2).into(),
                int(r, 3).into(),
                if runs < 0 { "".into() } else { runs.into() },
                text(r, 6).into(),
            ]
        }),
    );
    let mining = query(
        &format!(
            "SELECT m.day::text, m.type_id, {ore}, m.quantity, {system} FROM mining m \
             WHERE m.character_id = $1 ORDER BY m.day DESC, 3 LIMIT $2",
            ore = name_of("m.type_id"),
            system = name_of("m.solar_system_id"),
        ),
        &[id.into(), MAX_ROWS.into()],
    )?;
    let mining = with_rows(
        Table::new(vec![
            Column::text("Day"),
            Column::text("Ore"),
            Column::numeric("Quantity"),
            Column::text("System"),
        ])
        .empty("Nothing mined in the last 30 days."),
        mining.iter().map(|r| {
            vec![
                text(r, 0).into(),
                item_type(int(r, 1), text(r, 2)).into(),
                int(r, 3).into(),
                text(r, 4).into(),
            ]
        }),
    );
    let planets = query(
        &format!(
            "SELECT {planet}, {system}, p.planet_type, p.upgrade_level, p.pins, p.last_update \
             FROM planets p WHERE p.character_id = $1 ORDER BY 2, 1",
            planet = name_of("p.planet_id"),
            system = name_of("p.solar_system_id"),
        ),
        &[id.into()],
    )?;
    let planets = with_rows(
        Table::new(vec![
            Column::text("Planet"),
            Column::text("System"),
            Column::text("Type"),
            Column::numeric("Upgrade level"),
            Column::numeric("Installations"),
            Column::numeric("Last update"),
        ])
        .empty("No colonies."),
        planets.iter().map(|r| {
            vec![
                text(r, 0).into(),
                text(r, 1).into(),
                humanize(&text(r, 2)).into(),
                int(r, 3).into(),
                int(r, 4).into(),
                time_or_blank(r, 5),
            ]
        }),
    );
    Ok(sheet_page(who, "Industry")
        .tab(
            "Industry jobs",
            vec![Section::Table(jobs), fresh.line(&["industry"])],
        )
        .tab(
            "Blueprints",
            vec![Section::Table(blueprints), fresh.line(&["blueprints"])],
        )
        .tab(
            "Mining ledger",
            vec![Section::Table(mining), fresh.line(&["mining"])],
        )
        .tab(
            "Planets",
            vec![Section::Table(planets), fresh.line(&["planets"])],
        ))
}

fn activity(id: i64) -> &'static str {
    match id {
        1 => "Manufacturing",
        3 => "Time efficiency research",
        4 => "Material efficiency research",
        5 => "Copying",
        7 => "Reverse engineering",
        8 => "Invention",
        9 | 11 => "Reactions",
        _ => "Other",
    }
}

// ---- Contacts --------------------------------------------------------------

fn contacts(who: &Subject) -> Result<Page, PageError> {
    let id = who.id;
    let fresh = Freshness::of(id)?;
    let contacts = query(
        &format!(
            "SELECT k.contact_id, {contact}, k.contact_type, k.standing, k.is_watched, k.is_blocked \
             FROM contacts k WHERE k.character_id = $1 ORDER BY k.standing DESC, 2 LIMIT $2",
            contact = name_of("k.contact_id"),
        ),
        &[id.into(), MAX_ROWS.into()],
    )?;
    let contacts = with_rows(
        Table::new(vec![
            Column::text("Contact"),
            Column::text("Kind"),
            Column::numeric("Standing"),
            Column::text(""),
        ])
        .empty("No contacts."),
        contacts.iter().map(|r| {
            let kind = text(r, 2);
            let category = match kind.as_str() {
                "character" | "corporation" | "alliance" | "faction" => kind.as_str(),
                _ => "",
            };
            let flags = match (boolean(r, 4), boolean(r, 5)) {
                (true, true) => "Watched, blocked",
                (true, false) => "Watched",
                (false, true) => "Blocked",
                (false, false) => "",
            };
            vec![
                entity(int(r, 0), text(r, 1), category),
                humanize(&kind).into(),
                standing(float(r, 3)),
                flags.into(),
            ]
        }),
    );
    let standings = query(
        &format!(
            "SELECT s.from_id, {from}, s.from_type, s.standing FROM standings s \
             WHERE s.character_id = $1 ORDER BY s.standing DESC LIMIT $2",
            from = name_of("s.from_id"),
        ),
        &[id.into(), MAX_ROWS.into()],
    )?;
    let standings = with_rows(
        Table::new(vec![
            Column::text("From"),
            Column::text("Kind"),
            Column::numeric("Standing"),
        ])
        .empty("No NPC standings."),
        standings.iter().map(|r| {
            let kind = text(r, 2);
            vec![
                match kind.as_str() {
                    "faction" => faction(int(r, 0), text(r, 1)).into(),
                    "npc_corp" => corporation(int(r, 0), text(r, 1)).into(),
                    _ => format!("Agent {}", int(r, 0)).into(),
                },
                match kind.as_str() {
                    "npc_corp" => "Corporation",
                    "faction" => "Faction",
                    _ => "Agent",
                }
                .into(),
                standing(float(r, 3)),
            ]
        }),
    );
    Ok(sheet_page(who, "Contacts and standings")
        .tab(
            "Contacts",
            vec![Section::Table(contacts), fresh.line(&["contacts"])],
        )
        .tab(
            "NPC standings",
            vec![Section::Table(standings), fresh.line(&["standings"])],
        ))
}

/// A standing, `+5.0`, as a badge in the in-game colours' spirit (words
/// with the colour, never the colour alone).
/// A standing as EVE colours it: blue above zero, red below, neutral
/// plain.
fn standing(s: f64) -> Value {
    let text = format!("{s:+.1}");
    match s {
        s if s > 0.0 => badge(text, Tone::Success).into(),
        s if s < 0.0 => badge(text, Tone::Danger).into(),
        _ => "0.0".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_read_as_words() {
        assert_eq!(humanize("station_manager"), "Station manager");
        assert_eq!(humanize("bounty_prizes"), "Bounty prizes");
        assert_eq!(humanize(""), "");
        assert_eq!(humanize_flag("CorpSAG1"), "Corp S A G1");
        assert_eq!(humanize_flag("Hangar"), "Hangar");
        assert_eq!(grouped(1_234_567), "1,234,567");
        assert_eq!(grouped(-12), "-12");
    }
}
