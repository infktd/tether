//! Who may see which characters: aa-memberaudit's permissions.
//!
//! - `basic_access`: the app, and your own characters (their sheets and
//!   mail).
//! - `finder_access`: the Character Finder, listing the characters in your
//!   scope (and, with `view_shared_characters`, shared ones).
//! - `reports_access`: Reports, over the characters in your scope.
//! - `characters_access`: opening sheets of characters in your scope.
//! - Scope: `view_same_corporation` (characters whose owner's main is in
//!   your main's corporation), `view_same_alliance` (in your main's
//!   alliance), `view_everything` (every character registered with Member
//!   Audit). Without one, the scope is your own characters.
//! - `share_characters`: sharing your own characters; `view_shared_characters`:
//!   opening the sheets of characters their pilots shared (recruiters).
//! - `view_skill_sets`: a sheet's Skill Sets tab, and the Skill Sets page.
//! - Mail goes with the sheet, as in aa-memberaudit: whoever may open a
//!   character's sheet may read its mail. Every view of a mail page is in
//!   Tether's audit log.
//!
//! As in aa-memberaudit, the scopes go by the owner's main, so officers see
//! every alt of the pilots whose main is in their corporation, wherever the
//! alt is. Tether tells Member Audit, and no other app, who owns each
//! character (`identity::owners`). A character with no owner it knows of
//! is in nobody's corporation or alliance scope.

use std::cell::OnceCell;
use std::collections::{BTreeMap, BTreeSet};

use tether_plugin_sdk::identity::{self, Character, Member, MemberCharacter, Owner, Viewer};
use tether_plugin_sdk::log;
use tether_plugin_sdk::storage::Value as Db;

pub(crate) struct Access<'a> {
    pub viewer: &'a Viewer,
    pub finder: bool,
    pub reports: bool,
    pub characters: bool,
    /// May share their own characters.
    pub share: bool,
    /// May open shared characters' sheets.
    pub shared: bool,
    pub skill_sets: bool,
    everything: bool,
    corporation: Option<i64>,
    alliance: Option<i64>,
    /// Who owns each character, by character id: asked of the host the
    /// first time it's needed.
    owners: OnceCell<BTreeMap<i64, Owner>>,
    /// The characters their pilots share: read the first time it's needed.
    shared_ids: OnceCell<BTreeSet<i64>>,
    /// Every member account with all its characters: asked of the host
    /// the first time it's needed.
    members: OnceCell<Vec<Member>>,
}

impl<'a> Access<'a> {
    pub fn of(viewer: &'a Viewer) -> Self {
        Self {
            viewer,
            finder: viewer.can("finder_access"),
            reports: viewer.can("reports_access"),
            characters: viewer.can("characters_access"),
            share: viewer.can("share_characters"),
            shared: viewer.can("view_shared_characters"),
            skill_sets: viewer.can("view_skill_sets"),
            everything: viewer.can("view_everything"),
            corporation: viewer
                .can("view_same_corporation")
                .then_some(viewer.main.corporation_id)
                .filter(|id| *id > 0),
            alliance: if viewer.can("view_same_alliance") {
                viewer.main.alliance_id.filter(|id| *id > 0)
            } else {
                None
            },
            owners: OnceCell::new(),
            shared_ids: OnceCell::new(),
            members: OnceCell::new(),
        }
    }

    fn owners(&self) -> &BTreeMap<i64, Owner> {
        self.owners.get_or_init(|| match identity::owners() {
            Some(owners) => owners.into_iter().map(|o| (o.character_id, o)).collect(),
            None => {
                log::warn(
                    "Tether didn't say who owns characters (Member Audit isn't the bundled \
                     app?): scopes list only the viewer's own characters",
                );
                BTreeMap::new()
            }
        })
    }

    /// Who owns a character registered with Member Audit: their main and
    /// state.
    pub fn owner(&self, character: i64) -> Option<&Owner> {
        self.owners().get(&character)
    }

    /// Every character registered with Member Audit, with its owner's
    /// main and state (none when Tether doesn't say).
    pub fn all_owners(&self) -> impl Iterator<Item = &Owner> {
        self.owners().values()
    }

    pub fn owns(&self, character: i64) -> bool {
        self.viewer.characters.iter().any(|c| c.id == character)
    }

    /// Characters their pilots share, for holders of
    /// `view_shared_characters` (none for anyone else): within the sharing
    /// timeout, and only while the main who shared it still owns it.
    fn shared_ids(&self) -> &BTreeSet<i64> {
        self.shared_ids.get_or_init(|| {
            if !self.shared {
                return BTreeSet::new();
            }
            let rows = crate::query(
                "SELECT c.character_id, c.shared_by_main FROM characters c \
                 LEFT JOIN settings s ON s.id = 1 \
                 WHERE c.is_shared AND (coalesce(s.sharing_timeout_minutes, 0) = 0 \
                   OR c.shared_at > now() - make_interval(mins => s.sharing_timeout_minutes))",
                &[],
            )
            .unwrap_or_default();
            rows.iter()
                .filter(|r| {
                    let id = crate::int(r, 0);
                    let by = crate::opt_int(r, 1);
                    self.owner(id).is_some_and(|o| Some(o.main.id) == by)
                })
                .map(|r| crate::int(r, 0))
                .collect()
        })
    }

    /// Whether the character is shared and the viewer may see shared ones.
    pub fn sees_shared(&self, character: i64) -> bool {
        self.shared && self.shared_ids().contains(&character)
    }

    /// Whether a main is within a corporation or alliance scope.
    fn main_in_scope(&self, main: &Character) -> bool {
        self.corporation.is_some_and(|c| c == main.corporation_id)
            || self.alliance.is_some_and(|a| Some(a) == main.alliance_id)
    }

    /// Every account holding one of Member Audit's permissions, with its
    /// main, state and all its characters, registered or not (Tether
    /// tells the bundled Member Audit alone). Without that, only the
    /// viewer's own account.
    pub fn members(&self) -> &[Member] {
        self.members.get_or_init(|| match identity::members() {
            Some(members) => members,
            None => {
                let ids: Vec<i64> = self.viewer.characters.iter().map(|c| c.id).collect();
                let registered: BTreeSet<i64> = crate::query(
                    "SELECT character_id FROM characters \
                     WHERE character_id = ANY(string_to_array($1, ',')::bigint[])",
                    &[crate::id_list(&ids).into()],
                )
                .unwrap_or_default()
                .iter()
                .map(|r| crate::int(r, 0))
                .collect();
                vec![Member {
                    main: self.viewer.main.clone(),
                    state: self.viewer.state.clone(),
                    characters: self
                        .viewer
                        .characters
                        .iter()
                        .map(|c| MemberCharacter {
                            character: c.clone(),
                            registered: registered.contains(&c.id),
                        })
                        .collect(),
                }]
            }
        })
    }

    /// The member account a character is on, if Tether says.
    pub fn member_of(&self, character: i64) -> Option<&Member> {
        self.members()
            .iter()
            .find(|m| m.characters.iter().any(|c| c.character.id == character))
    }

    /// The member accounts the viewer's scope covers, by their main
    /// (aa-memberaudit's `accessible_users`): all of them with
    /// `view_everything`, those whose main is in the viewer's main's
    /// corporation or alliance with those scopes, and always their own.
    pub fn members_in_scope(&self) -> impl Iterator<Item = &Member> {
        self.members().iter().filter(|m| {
            self.everything || m.main.id == self.viewer.main.id || self.main_in_scope(&m.main)
        })
    }

    /// Within the viewer's scope, going by the character's owner's main
    /// (not counting their own characters).
    fn in_scope(&self, character: i64) -> bool {
        if self.everything {
            return true;
        }
        if self.corporation.is_none() && self.alliance.is_none() {
            return false;
        }
        self.owner(character)
            .is_some_and(|owner| self.main_in_scope(&owner.main))
    }

    /// May open this character's sheet, and so read its mail.
    pub fn may_open(&self, character: i64) -> bool {
        self.owns(character)
            || (self.characters && self.in_scope(character))
            || self.sees_shared(character)
    }

    /// May read this character's mail: whoever may open its sheet, as in
    /// aa-memberaudit.
    pub fn may_read_mail(&self, character: i64) -> bool {
        self.may_open(character)
    }

    /// SQL (over `characters c`) for the characters Reports list, and its
    /// parameter as `$first`: the viewer's own, and those in scope. Fixed
    /// SQL; only ids are parameters.
    pub fn listed(&self, first: usize) -> (String, Vec<Db>) {
        self.listing(first, false)
    }

    /// As [`Self::listed`], for the Character Finder: shared characters
    /// too, for holders of `view_shared_characters` (aa-memberaudit's
    /// Finder).
    pub fn found(&self, first: usize) -> (String, Vec<Db>) {
        self.listing(first, true)
    }

    fn listing(&self, first: usize, with_shared: bool) -> (String, Vec<Db>) {
        if self.everything {
            return ("true".to_owned(), Vec::new());
        }
        let mut ids: Vec<i64> = self.viewer.characters.iter().map(|c| c.id).collect();
        if self.corporation.is_some() || self.alliance.is_some() {
            ids.extend(
                self.owners()
                    .values()
                    .filter(|owner| self.main_in_scope(&owner.main))
                    .map(|owner| owner.character_id),
            );
        }
        if with_shared {
            ids.extend(self.shared_ids().iter().copied());
        }
        ids.sort_unstable();
        ids.dedup();
        (
            format!("c.character_id = ANY(string_to_array(${first}, ',')::bigint[])"),
            vec![crate::id_list(&ids).into()],
        )
    }

    /// Characters whose owner's main is named like `q` (lowercase), for
    /// the Finder's search.
    pub fn mains_named(&self, q: &str) -> Vec<i64> {
        self.owners()
            .values()
            .filter(|owner| owner.main.name.to_lowercase().contains(q))
            .map(|owner| owner.character_id)
            .collect()
    }

    /// What the scope is, in words.
    pub fn scope_words(&self) -> &'static str {
        if self.everything {
            "every character registered with Member Audit"
        } else {
            match (self.corporation.is_some(), self.alliance.is_some()) {
                (_, true) => {
                    "yours and every character of pilots whose main is in your main's alliance"
                }
                (true, false) => {
                    "yours and every character of pilots whose main is in your main's corporation"
                }
                (false, false) => "your own characters",
            }
        }
    }
}
