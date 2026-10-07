//! Every ESI scope, with a plain description for admins and pilots, and
//! the scope compliance check (F11): every character on an account carries
//! the scopes its state requires. The same check tells which characters
//! are registered for an app (F16): their tokens carry the app's scopes.

use std::collections::BTreeSet;

/// Whose data a scope reads: the character's own, or its corporation's or
/// alliance's (which also needs an in-game role for most endpoints).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeKind {
    Character,
    Corporation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScopeInfo {
    pub scope: &'static str,
    pub kind: ScopeKind,
    pub description: &'static str,
}

const fn character(scope: &'static str, description: &'static str) -> ScopeInfo {
    ScopeInfo {
        scope,
        kind: ScopeKind::Character,
        description,
    }
}

const fn corporation(scope: &'static str, description: &'static str) -> ScopeInfo {
    ScopeInfo {
        scope,
        kind: ScopeKind::Corporation,
        description,
    }
}

/// Every scope in ESI's current spec.
pub const ALL: &[ScopeInfo] = &[
    character(
        "esi-access.read_lists.v1",
        "Read the access lists the character manages",
    ),
    character(
        "esi-activities.read_character.v1",
        "Read the character's activities",
    ),
    corporation(
        "esi-alliances.read_contacts.v1",
        "Read the alliance's contacts",
    ),
    character("esi-assets.read_assets.v1", "Read the character's assets"),
    corporation(
        "esi-assets.read_corporation_assets.v1",
        "Read the corporation's assets",
    ),
    character(
        "esi-calendar.read_calendar_events.v1",
        "Read the character's calendar",
    ),
    character(
        "esi-calendar.respond_calendar_events.v1",
        "Respond to calendar events",
    ),
    character(
        "esi-characters.read_agents_research.v1",
        "Read agent research",
    ),
    character(
        "esi-characters.read_blueprints.v1",
        "Read the character's blueprints",
    ),
    character(
        "esi-characters.read_contacts.v1",
        "Read the character's contacts and standings to them",
    ),
    character(
        "esi-characters.read_corporation_roles.v1",
        "Read the character's corporation roles",
    ),
    character("esi-characters.read_fatigue.v1", "Read jump fatigue"),
    character(
        "esi-characters.read_freelance_jobs.v1",
        "Read the character's freelance jobs",
    ),
    character(
        "esi-characters.read_fw_stats.v1",
        "Read faction warfare statistics",
    ),
    character("esi-characters.read_loyalty.v1", "Read loyalty points"),
    character(
        "esi-characters.read_medals.v1",
        "Read the character's medals",
    ),
    character(
        "esi-characters.read_notifications.v1",
        "Read in-game notifications (structure attacks, moon extractions, ...)",
    ),
    character("esi-characters.read_standings.v1", "Read NPC standings"),
    character(
        "esi-characters.read_titles.v1",
        "Read the character's corporation titles",
    ),
    character(
        "esi-characters.write_contacts.v1",
        "Change the character's contacts",
    ),
    character(
        "esi-clones.read_clones.v1",
        "Read jump clones and home station",
    ),
    character("esi-clones.read_implants.v1", "Read active implants"),
    character(
        "esi-contracts.read_character_contracts.v1",
        "Read the character's contracts",
    ),
    corporation(
        "esi-contracts.read_corporation_contracts.v1",
        "Read the corporation's contracts",
    ),
    corporation(
        "esi-corporations.read_blueprints.v1",
        "Read the corporation's blueprints",
    ),
    corporation(
        "esi-corporations.read_contacts.v1",
        "Read the corporation's contacts",
    ),
    corporation(
        "esi-corporations.read_container_logs.v1",
        "Read the corporation's container logs",
    ),
    corporation(
        "esi-corporations.read_corporation_membership.v1",
        "Read the corporation's member list and who holds which roles",
    ),
    corporation(
        "esi-corporations.read_divisions.v1",
        "Read the corporation's hangar and wallet division names",
    ),
    corporation(
        "esi-corporations.read_facilities.v1",
        "Read the corporation's industry facilities",
    ),
    corporation(
        "esi-corporations.read_freelance_jobs.v1",
        "Read the corporation's freelance jobs",
    ),
    corporation(
        "esi-corporations.read_fw_stats.v1",
        "Read the corporation's faction warfare statistics",
    ),
    corporation(
        "esi-corporations.read_medals.v1",
        "Read the corporation's medals",
    ),
    corporation(
        "esi-corporations.read_projects.v1",
        "Read the corporation's projects",
    ),
    corporation(
        "esi-corporations.read_standings.v1",
        "Read the corporation's NPC standings",
    ),
    corporation(
        "esi-corporations.read_starbases.v1",
        "Read the corporation's starbases (POSes)",
    ),
    corporation(
        "esi-corporations.read_structures.v1",
        "Read the corporation's structures and their fuel",
    ),
    corporation(
        "esi-corporations.read_titles.v1",
        "Read the corporation's titles",
    ),
    corporation(
        "esi-corporations.track_members.v1",
        "Track the corporation's members (logins, locations, ships)",
    ),
    character("esi-fittings.read_fittings.v1", "Read saved fittings"),
    character("esi-fittings.write_fittings.v1", "Save and delete fittings"),
    character(
        "esi-fleets.read_fleet.v1",
        "Read the fleet the character is in",
    ),
    character(
        "esi-fleets.write_fleet.v1",
        "Manage the fleet the character commands",
    ),
    character(
        "esi-industry.read_character_jobs.v1",
        "Read the character's industry jobs",
    ),
    character(
        "esi-industry.read_character_mining.v1",
        "Read the character's mining ledger",
    ),
    corporation(
        "esi-industry.read_corporation_jobs.v1",
        "Read the corporation's industry jobs",
    ),
    corporation(
        "esi-industry.read_corporation_mining.v1",
        "Read moon extractions and the corporation's mining ledgers",
    ),
    corporation(
        "esi-killmails.read_corporation_killmails.v1",
        "Read the corporation's killmails",
    ),
    character(
        "esi-killmails.read_killmails.v1",
        "Read the character's killmails",
    ),
    character(
        "esi-location.read_location.v1",
        "Read where the character is",
    ),
    character(
        "esi-location.read_online.v1",
        "Read whether the character is online",
    ),
    character(
        "esi-location.read_ship_type.v1",
        "Read the ship the character is flying",
    ),
    character(
        "esi-mail.organize_mail.v1",
        "Organise EVE mail (labels, read, delete)",
    ),
    character("esi-mail.read_mail.v1", "Read EVE mail"),
    character("esi-mail.send_mail.v1", "Send EVE mail as the character"),
    character(
        "esi-markets.read_character_orders.v1",
        "Read the character's market orders",
    ),
    corporation(
        "esi-markets.read_corporation_orders.v1",
        "Read the corporation's market orders",
    ),
    character(
        "esi-markets.structure_markets.v1",
        "Read markets in structures the character can use",
    ),
    character(
        "esi-planets.manage_planets.v1",
        "Read the character's planetary colonies",
    ),
    corporation(
        "esi-planets.read_customs_offices.v1",
        "Read the corporation's customs offices",
    ),
    character(
        "esi-search.search_structures.v1",
        "Search for structures the character can see",
    ),
    character("esi-skills.read_skillqueue.v1", "Read the skill queue"),
    character("esi-skills.read_skills.v1", "Read skills and attributes"),
    character(
        "esi-structures.read_character.v1",
        "Read the character's structure access",
    ),
    corporation(
        "esi-structures.read_corporation.v1",
        "Read the corporation's structure access",
    ),
    character("esi-ui.open_window.v1", "Open windows in the game client"),
    character("esi-ui.write_waypoint.v1", "Set autopilot waypoints"),
    character(
        "esi-universe.read_structures.v1",
        "Read names and locations of structures the character can see",
    ),
    character(
        "esi-wallet.read_character_wallet.v1",
        "Read the character's wallet and journal",
    ),
    corporation(
        "esi-wallet.read_corporation_wallets.v1",
        "Read the corporation's wallets",
    ),
];

/// The scope Corporation Stats reads a corporation's member list with,
/// from any registered Member character in it (ESI needs no in-game role
/// for the list).
pub const CORP_MEMBERSHIP: &str = "esi-corporations.read_corporation_membership.v1";

/// Scopes Tether itself needs from every Member character (and from no
/// other state's): the corporation member list, so Corporation Stats reads
/// every covered corporation's list, as Alliance Auth's does, without a
/// member volunteering. Everything else core does uses public ESI.
pub const CORE: &[&str] = &[CORP_MEMBERSHIP];

/// Scopes that act as the character (send mail, change contacts, manage
/// fleets, drive the client): never required of anyone, since nothing in
/// Tether uses them and they widen what a leaked token could do.
pub const WRITE: &[&str] = &[
    "esi-calendar.respond_calendar_events.v1",
    "esi-characters.write_contacts.v1",
    "esi-fittings.write_fittings.v1",
    "esi-fleets.write_fleet.v1",
    "esi-mail.organize_mail.v1",
    "esi-mail.send_mail.v1",
    "esi-ui.open_window.v1",
    "esi-ui.write_waypoint.v1",
];

pub fn is_write(scope: &str) -> bool {
    WRITE.contains(&scope)
}

pub fn info(scope: &str) -> Option<&'static ScopeInfo> {
    ALL.iter().find(|s| s.scope == scope)
}

/// A plain description, or the scope itself if ESI added one since.
pub fn describe(scope: &str) -> &str {
    info(scope).map_or(scope, |s| s.description)
}

/// A character's stored SSO token, as compliance sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Token {
    None,
    /// Revoked (by EVE, or deleted), with the scopes it carried: still a
    /// registration, as aa-memberaudit's, until the character leaves the
    /// account (the ownership check, a day later).
    Revoked(Vec<String>),
    Valid(Vec<String>),
}

/// Why a character doesn't meet its state's requirements.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Problem {
    /// Never registered with scopes (a plain login).
    NotRegistered,
    /// The token was revoked (by the player on EVE's site, or expired)
    /// and didn't carry every required scope: log in again.
    Revoked,
    /// Registered, but without these.
    Missing(Vec<String>),
    /// Carries every required scope, but isn't registered for these apps
    /// (by name), which the state requires: Alliance Auth's Member Audit
    /// compliance is registration with Member Audit.
    NotRegisteredFor(Vec<String>),
    /// Not registered for these apps, which the state requires, and the
    /// account holds none of their permissions, so it can't register (as
    /// AA's Member Audit registration needs `basic_access`): an admin must
    /// grant one first.
    NoAccessTo(Vec<String>),
}

/// An app a state requires every character to be registered for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequiredApp {
    pub name: String,
    /// Its user scopes: registering grants them (a character not yet
    /// registered needs them, one registered counts without them).
    pub scopes: BTreeSet<String>,
    /// The characters registered for it.
    pub registered: BTreeSet<i64>,
    /// The account holds one of its permissions, so it may register
    /// characters for it.
    pub may_register: bool,
}

/// [`check`] for a state that also requires apps: `required` (the state's
/// own scopes) on every character, and each character registered for
/// every app. A character registered for an app needs none of the app's
/// scopes to count: one the app asked for after it registered doesn't
/// undo the registration (aa-memberaudit's compliance is every character
/// registered). One not yet registered needs the app's scopes too, and a
/// scope problem comes first (registering fixes both).
pub fn check_with_apps(
    required: &BTreeSet<String>,
    apps: &[RequiredApp],
    characters: &[(i64, Token)],
) -> Vec<(i64, Problem)> {
    let mut problems: Vec<(i64, Problem)> = characters
        .iter()
        .flat_map(|(id, token)| {
            let mut all = required.clone();
            for app in apps.iter().filter(|app| !app.registered.contains(id)) {
                all.extend(app.scopes.iter().cloned());
            }
            check(&all, &[(*id, token.clone())])
        })
        .collect();
    for (id, _) in characters {
        if problems.iter().any(|(p, _)| p == id) {
            continue;
        }
        let missing: Vec<&RequiredApp> = apps
            .iter()
            .filter(|app| !app.registered.contains(id))
            .collect();
        // What the pilot can't do anything about comes first.
        let barred: Vec<String> = missing
            .iter()
            .filter(|app| !app.may_register)
            .map(|app| app.name.clone())
            .collect();
        if !barred.is_empty() {
            problems.push((*id, Problem::NoAccessTo(barred)));
        } else if !missing.is_empty() {
            problems.push((
                *id,
                Problem::NotRegisteredFor(missing.iter().map(|app| app.name.clone()).collect()),
            ));
        }
    }
    problems.sort_by_key(|(id, _)| characters.iter().position(|(c, _)| c == id));
    problems
}

/// Which characters fall short of `required`. Empty when compliant; a
/// state that requires nothing is met by everyone. A revoked token that
/// carried every required scope still counts (aa-memberaudit: compliance
/// is registration; a token error only asks the pilot to register again).
pub fn check(required: &BTreeSet<String>, characters: &[(i64, Token)]) -> Vec<(i64, Problem)> {
    if required.is_empty() {
        return Vec::new();
    }
    characters
        .iter()
        .filter_map(|(id, token)| {
            let problem = match token {
                Token::None => Problem::NotRegistered,
                Token::Revoked(scopes) => {
                    if required.iter().all(|r| scopes.contains(r)) {
                        return None;
                    }
                    Problem::Revoked
                }
                Token::Valid(scopes) => {
                    let missing: Vec<String> = required
                        .iter()
                        .filter(|r| !scopes.contains(r))
                        .cloned()
                        .collect();
                    if missing.is_empty() {
                        return None;
                    }
                    if scopes.is_empty() {
                        Problem::NotRegistered
                    } else {
                        Problem::Missing(missing)
                    }
                }
            };
            Some((*id, problem))
        })
        .collect()
}

/// A state's required scopes: what admins added, and for Member also
/// Tether's own ([`CORE`]). Apps' user scopes aren't required by
/// themselves: pilots register characters for an app, and admins may
/// require an app's scopes of a state (Alliance Auth's Member Audit
/// compliance groups).
pub fn required(member: bool, admin_scopes: &[String]) -> BTreeSet<String> {
    let mut all: BTreeSet<String> = admin_scopes.iter().cloned().collect();
    if member {
        all.extend(CORE.iter().map(|s| (*s).to_owned()));
    }
    all
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(scopes: &[&str]) -> BTreeSet<String> {
        scopes.iter().map(|s| (*s).to_owned()).collect()
    }

    fn valid(scopes: &[&str]) -> Token {
        Token::Valid(scopes.iter().map(|s| (*s).to_owned()).collect())
    }

    #[test]
    fn every_character_needs_every_scope() {
        let required = set(&["esi-skills.read_skills.v1", "esi-assets.read_assets.v1"]);
        let characters = [
            (
                1,
                valid(&[
                    "esi-skills.read_skills.v1",
                    "esi-assets.read_assets.v1",
                    "extra",
                ]),
            ),
            (2, valid(&["esi-skills.read_skills.v1"])),
            (
                3,
                Token::Revoked(vec!["esi-skills.read_skills.v1".to_owned()]),
            ),
            (4, Token::None),
            (5, valid(&[])),
            // Revoked, but it carried everything: still registered.
            (
                6,
                Token::Revoked(vec![
                    "esi-skills.read_skills.v1".to_owned(),
                    "esi-assets.read_assets.v1".to_owned(),
                ]),
            ),
        ];
        assert_eq!(
            check(&required, &characters),
            [
                (
                    2,
                    Problem::Missing(vec!["esi-assets.read_assets.v1".to_owned()])
                ),
                (3, Problem::Revoked),
                (4, Problem::NotRegistered),
                (5, Problem::NotRegistered),
            ]
        );
    }

    #[test]
    fn a_required_app_needs_its_scopes_and_a_registration() {
        let mut app = RequiredApp {
            name: "Member Audit".to_owned(),
            scopes: set(&["esi-skills.read_skills.v1"]),
            registered: [1, 3].into_iter().collect(),
            may_register: true,
        };
        let characters = [
            (1, valid(&["esi-skills.read_skills.v1"])),
            // The scope, but not registered for the app.
            (2, valid(&["esi-skills.read_skills.v1"])),
            // Registered before the app asked for the scope: still counts,
            // as aa-memberaudit's compliance is every character registered.
            (3, valid(&["esi-assets.read_assets.v1"])),
            // A revoked token still counts with what it carried.
            (
                4,
                Token::Revoked(vec!["esi-skills.read_skills.v1".to_owned()]),
            ),
            // Neither registered nor carrying the scope: the scope first.
            (5, valid(&["esi-assets.read_assets.v1"])),
        ];
        assert_eq!(
            check_with_apps(&BTreeSet::new(), std::slice::from_ref(&app), &characters),
            [
                (
                    2,
                    Problem::NotRegisteredFor(vec!["Member Audit".to_owned()])
                ),
                (
                    4,
                    Problem::NotRegisteredFor(vec!["Member Audit".to_owned()])
                ),
                (
                    5,
                    Problem::Missing(vec!["esi-skills.read_skills.v1".to_owned()])
                ),
            ]
        );
        // The state's own scopes are still needed by everyone.
        let own = set(&["esi-clones.read_clones.v1"]);
        assert_eq!(
            check_with_apps(&own, std::slice::from_ref(&app), &characters[2..3]),
            [(
                3,
                Problem::Missing(vec!["esi-clones.read_clones.v1".to_owned()])
            )]
        );
        assert!(check_with_apps(&BTreeSet::new(), &[], &characters).is_empty());
        // Without one of the app's permissions the account can't register:
        // it says so instead of asking it to.
        app.may_register = false;
        assert_eq!(
            check_with_apps(&BTreeSet::new(), std::slice::from_ref(&app), &characters)[0],
            (2, Problem::NoAccessTo(vec!["Member Audit".to_owned()]))
        );
    }

    #[test]
    fn nothing_required_is_always_met() {
        assert!(
            check(
                &BTreeSet::new(),
                &[(1, Token::None), (2, Token::Revoked(Vec::new()))]
            )
            .is_empty()
        );
    }

    #[test]
    fn member_adds_core_scopes_and_others_only_admin_ones() {
        let admin = vec!["esi-clones.read_clones.v1".to_owned()];
        assert_eq!(
            required(true, &admin),
            set(&["esi-clones.read_clones.v1", CORP_MEMBERSHIP])
        );
        // Member always requires the member list scope; nobody else does.
        assert_eq!(required(true, &[]), set(&[CORP_MEMBERSHIP]));
        assert!(required(false, &[]).is_empty());
        assert_eq!(required(false, &admin), set(&["esi-clones.read_clones.v1"]));
    }

    #[test]
    fn the_catalogue_is_well_formed() {
        let mut seen = BTreeSet::new();
        for s in ALL {
            assert!(
                s.scope.starts_with("esi-") && s.scope.contains(".v"),
                "{}",
                s.scope
            );
            assert!(seen.insert(s.scope), "duplicate {}", s.scope);
            assert!(!s.description.is_empty());
        }
        assert_eq!(
            info(CORP_MEMBERSHIP).map(|s| s.kind),
            Some(ScopeKind::Corporation)
        );
        assert_eq!(describe("esi-new.thing.v1"), "esi-new.thing.v1");
        for write in WRITE {
            assert!(info(write).is_some(), "{write}");
        }
    }
}
