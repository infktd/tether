//! Who may see which characters: aa-memberaudit's permissions.
//!
//! - `basic`: the app, and your own characters (their sheets and mail).
//! - `finder`: the Character Finder and Reports, listing the characters in
//!   your scope.
//! - `characters`: opening sheets of characters in your scope.
//! - Scope: `view_same_corporation` (your main's corporation),
//!   `view_same_alliance` (your main's alliance), `view_everything` (every
//!   member character). Without one, the scope is your own characters.
//! - `view_mail`: reading the mail of characters whose sheets you may
//!   open. Your own characters' mail needs no permission. Every view of a
//!   mail page is in Tether's audit log either way.
//!
//! aa-memberaudit matches the owner's main's corporation and alliance.
//! Apps aren't told who owns a character, so the scope here is each
//! character's own corporation and alliance.

use tether_plugin_sdk::identity::Viewer;
use tether_plugin_sdk::storage::Value as Db;

pub(crate) struct Access<'a> {
    pub viewer: &'a Viewer,
    pub finder: bool,
    pub characters: bool,
    pub mail: bool,
    everything: bool,
    corporation: Option<i64>,
    alliance: Option<i64>,
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
        }
    }

    pub fn owns(&self, character: i64) -> bool {
        self.viewer.characters.iter().any(|c| c.id == character)
    }

    /// Within the viewer's scope, going by the character's corporation and
    /// alliance (not counting their own characters).
    fn in_scope(&self, corporation: i64, alliance: Option<i64>) -> bool {
        self.everything
            || self.corporation.is_some_and(|c| c == corporation)
            || self.alliance.is_some_and(|a| Some(a) == alliance)
    }

    /// May open this character's sheet.
    pub fn may_open(&self, character: i64, corporation: i64, alliance: Option<i64>) -> bool {
        self.owns(character) || (self.characters && self.in_scope(corporation, alliance))
    }

    /// May read this character's mail.
    pub fn may_read_mail(&self, character: i64, corporation: i64, alliance: Option<i64>) -> bool {
        self.owns(character) || (self.mail && self.may_open(character, corporation, alliance))
    }

    /// SQL (over `characters c`) for the characters the Finder and Reports
    /// list, and its parameters from `$first` on: the viewer's own, and
    /// those in scope. Fixed SQL; only ids are parameters.
    pub fn listed(&self, first: usize) -> (String, Vec<Db>) {
        if self.everything {
            return ("true".to_owned(), Vec::new());
        }
        let own: Vec<String> = self
            .viewer
            .characters
            .iter()
            .map(|c| c.id.to_string())
            .collect();
        let mut sql = format!("c.character_id = ANY(string_to_array(${first}, ',')::bigint[])");
        let mut params: Vec<Db> = vec![own.join(",").into()];
        if let Some(corporation) = self.corporation {
            params.push(corporation.into());
            sql.push_str(&format!(
                " OR c.corporation_id = ${}",
                first + params.len() - 1
            ));
        }
        if let Some(alliance) = self.alliance {
            params.push(alliance.into());
            sql.push_str(&format!(
                " OR c.alliance_id = ${}",
                first + params.len() - 1
            ));
        }
        (format!("({sql})"), params)
    }

    /// What the scope is, in words.
    pub fn scope_words(&self) -> &'static str {
        if self.everything {
            "every member character"
        } else {
            match (self.corporation.is_some(), self.alliance.is_some()) {
                (_, true) => "your own characters and your main's alliance",
                (true, false) => "your own characters and your main's corporation",
                (false, false) => "your own characters",
            }
        }
    }
}
