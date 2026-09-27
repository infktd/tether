//! Who may see which characters: aa-memberaudit's permissions.
//!
//! - `basic`: the app, and your own characters (their sheets and mail).
//! - `finder`: the Character Finder and Reports, listing the characters in
//!   your scope.
//! - `characters`: opening sheets of characters in your scope.
//! - Scope: `view_same_corporation` (characters whose owner's main is in
//!   your main's corporation), `view_same_alliance` (in your main's
//!   alliance), `view_everything` (every member character). Without one,
//!   the scope is your own characters.
//! - `view_mail`: reading the mail of characters whose sheets you may
//!   open. Your own characters' mail needs no permission. Every view of a
//!   mail page is in Tether's audit log either way.
//!
//! As in aa-memberaudit, the scopes go by the owner's main, so officers see
//! every alt of the pilots whose main is in their corporation, wherever the
//! alt is. Tether tells Member Audit, and no other app, who owns each
//! character (`identity::owners`). A character with no owner it knows of
//! (no longer a Member's) is in nobody's corporation or alliance scope.

use std::cell::OnceCell;
use std::collections::BTreeMap;

use tether_plugin_sdk::identity::{self, Owner, Viewer};
use tether_plugin_sdk::log;
use tether_plugin_sdk::storage::Value as Db;

pub(crate) struct Access<'a> {
    pub viewer: &'a Viewer,
    pub finder: bool,
    pub characters: bool,
    pub mail: bool,
    everything: bool,
    corporation: Option<i64>,
    alliance: Option<i64>,
    /// Who owns each member character, by character id: asked of the host
    /// the first time it's needed.
    owners: OnceCell<BTreeMap<i64, Owner>>,
}

impl<'a> Access<'a> {
    pub fn of(viewer: &'a Viewer) -> Self {
        Self {
            viewer,
            finder: viewer.can("finder"),
            characters: viewer.can("characters"),
            mail: viewer.can("view_mail"),
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

    /// Who owns a member character: their main and state.
    pub fn owner(&self, character: i64) -> Option<&Owner> {
        self.owners().get(&character)
    }

    pub fn owns(&self, character: i64) -> bool {
        self.viewer.characters.iter().any(|c| c.id == character)
    }

    /// Whether an owner's main is within a corporation or alliance scope.
    fn main_in_scope(&self, owner: &Owner) -> bool {
        self.corporation
            .is_some_and(|c| c == owner.main.corporation_id)
            || self
                .alliance
                .is_some_and(|a| Some(a) == owner.main.alliance_id)
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
            .is_some_and(|owner| self.main_in_scope(owner))
    }

    /// May open this character's sheet.
    pub fn may_open(&self, character: i64) -> bool {
        self.owns(character) || (self.characters && self.in_scope(character))
    }

    /// May read this character's mail.
    pub fn may_read_mail(&self, character: i64) -> bool {
        self.owns(character) || (self.mail && self.may_open(character))
    }

    /// SQL (over `characters c`) for the characters the Finder and Reports
    /// list, and its parameter as `$first`: the viewer's own, and those in
    /// scope. Fixed SQL; only ids are parameters.
    pub fn listed(&self, first: usize) -> (String, Vec<Db>) {
        if self.everything {
            return ("true".to_owned(), Vec::new());
        }
        let mut ids: Vec<i64> = self.viewer.characters.iter().map(|c| c.id).collect();
        if self.corporation.is_some() || self.alliance.is_some() {
            ids.extend(
                self.owners()
                    .values()
                    .filter(|owner| self.main_in_scope(owner))
                    .map(|owner| owner.character_id),
            );
        }
        ids.sort_unstable();
        ids.dedup();
        (
            format!("c.character_id = ANY(string_to_array(${first}, ',')::bigint[])"),
            vec![crate::id_list(&ids).into()],
        )
    }

    /// Member characters whose owner's main is named like `q` (lowercase),
    /// for the Finder's search.
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
            "every member character"
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
