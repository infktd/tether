//! What's new (DESIGN.md): the changelog, `CHANGELOG.md` at the repository's
//! root, built in. After an update each pilot's next page opens the
//! releases they haven't seen, once; each note goes to whom it concerns:
//! `### Everyone` to every pilot, `### Admins` to whoever may open
//! Administration, and `### <app name>` to whoever may open that app.
//! Admins also see which apps changed version since they last looked.
//!
//! Releases are `## ` headings, newest first, and are never removed or
//! reordered: a release's number is its place counted from the oldest, and
//! accounts remember the newest number they've seen.

use std::sync::OnceLock;

/// The changelog as this build ships it.
const CHANGELOG: &str = include_str!("../../../CHANGELOG.md");

/// The popup shows at most this many releases; the page shows them all.
pub const POPUP_RELEASES: usize = 3;

/// Whom a group of notes is for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Audience {
    Everyone,
    Admins,
    /// An app, by the name its manifest gives.
    App(String),
}

impl Audience {
    fn parse(heading: &str) -> Self {
        match heading.trim() {
            "Everyone" => Self::Everyone,
            "Admins" => Self::Admins,
            name => Self::App(name.to_owned()),
        }
    }
}

/// A heading's notes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notes {
    pub audience: Audience,
    pub notes: Vec<String>,
}

/// One update's notes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    /// Its place counted from the oldest (1).
    pub number: i32,
    /// The heading's text: the date it went out.
    pub title: String,
    pub groups: Vec<Notes>,
}

/// Reads a changelog: `## ` starts a release, `### ` a group, `- ` a note
/// (lines indented under it continue it); anything else (the preamble,
/// blank lines) is skipped.
pub fn parse(text: &str) -> Vec<Release> {
    let mut releases: Vec<Release> = Vec::new();
    for line in text.lines() {
        if let Some(title) = line.strip_prefix("## ") {
            releases.push(Release {
                number: 0,
                title: title.trim().to_owned(),
                groups: Vec::new(),
            });
        } else if let Some(heading) = line.strip_prefix("### ") {
            if let Some(release) = releases.last_mut() {
                release.groups.push(Notes {
                    audience: Audience::parse(heading),
                    notes: Vec::new(),
                });
            }
        } else if let Some(note) = line.strip_prefix("- ") {
            if let Some(group) = releases.last_mut().and_then(|r| r.groups.last_mut()) {
                group.notes.push(note.trim().to_owned());
            }
        } else if line.starts_with("  ")
            && !line.trim().is_empty()
            && let Some(note) = releases
                .last_mut()
                .and_then(|r| r.groups.last_mut())
                .and_then(|g| g.notes.last_mut())
        {
            note.push(' ');
            note.push_str(line.trim());
        }
    }
    let total = releases.len();
    for (i, release) in releases.iter_mut().enumerate() {
        release.number = i32::try_from(total - i).unwrap_or(i32::MAX);
    }
    releases
}

/// This build's releases, newest first.
pub fn releases() -> &'static [Release] {
    static RELEASES: OnceLock<Vec<Release>> = OnceLock::new();
    RELEASES.get_or_init(|| parse(CHANGELOG))
}

/// The newest release's number (0 with none).
pub fn latest() -> i32 {
    releases().first().map_or(0, |r| r.number)
}

/// Who is looking, for what concerns them.
pub struct Viewer<'a> {
    /// May open Administration.
    pub admin: bool,
    /// The names of the apps they may open.
    pub apps: &'a [String],
}

impl Viewer<'_> {
    fn sees(&self, audience: &Audience) -> bool {
        match audience {
            Audience::Everyone => true,
            Audience::Admins => self.admin,
            Audience::App(name) => self.apps.iter().any(|a| a.eq_ignore_ascii_case(name)),
        }
    }
}

/// A release as one viewer sees it.
pub struct ReleaseView {
    pub title: String,
    pub groups: Vec<GroupView>,
}

/// A group of notes, under its heading: none for everyone's, which come
/// first.
pub struct GroupView {
    pub heading: Option<String>,
    pub notes: Vec<String>,
}

/// The releases after `seen` with something for `viewer`, newest first, as
/// they see them.
pub fn for_viewer(releases: &[Release], seen: i32, viewer: &Viewer<'_>) -> Vec<ReleaseView> {
    releases
        .iter()
        .filter(|r| r.number > seen)
        .filter_map(|r| {
            let mut groups: Vec<GroupView> = r
                .groups
                .iter()
                .filter(|g| viewer.sees(&g.audience) && !g.notes.is_empty())
                .map(|g| GroupView {
                    heading: match &g.audience {
                        Audience::Everyone => None,
                        Audience::Admins => Some("Administration".to_owned()),
                        Audience::App(name) => Some(name.clone()),
                    },
                    notes: g.notes.clone(),
                })
                .collect();
            // Everyone's first, then the rest as written.
            groups.sort_by_key(|g| g.heading.is_some());
            (!groups.is_empty()).then(|| ReleaseView {
                title: r.title.clone(),
                groups,
            })
        })
        .collect()
}

/// The popup, for the shell: what this viewer hasn't seen.
pub struct WhatsNew {
    /// At most [`POPUP_RELEASES`].
    pub releases: Vec<ReleaseView>,
    /// Unseen releases past those shown, for the page.
    pub more: usize,
    /// For admins: apps updated since they last looked.
    pub apps: Vec<tether_db::whats_new::AppUpdate>,
}

impl WhatsNew {
    /// The popup's, when there's anything to show.
    pub fn build(
        mut releases: Vec<ReleaseView>,
        apps: Vec<tether_db::whats_new::AppUpdate>,
    ) -> Option<Self> {
        if releases.is_empty() && apps.is_empty() {
            return None;
        }
        let more = releases.len().saturating_sub(POPUP_RELEASES);
        releases.truncate(POPUP_RELEASES);
        Some(Self {
            releases,
            more,
            apps,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "# Changelog\n\nPreamble, skipped.\n\n## 2026-10-08\n\n### Structures\n- Jump gates\n  have a tab.\n\n### Everyone\n- A popup says what's new.\n\n## 2026-10-07\n\n### Admins\n- Settings say when they can't take effect.\n";

    #[test]
    fn releases_count_from_the_oldest() {
        let releases = parse(SAMPLE);
        assert_eq!(releases.len(), 2);
        assert_eq!(releases[0].number, 2);
        assert_eq!(releases[0].title, "2026-10-08");
        assert_eq!(
            releases[0].groups[0],
            Notes {
                audience: Audience::App("Structures".to_owned()),
                notes: vec!["Jump gates have a tab.".to_owned()],
            }
        );
        assert_eq!(releases[1].number, 1);
        assert_eq!(releases[1].groups[0].audience, Audience::Admins);
    }

    #[test]
    fn each_sees_what_concerns_them() {
        let releases = parse(SAMPLE);
        let member = Viewer {
            admin: false,
            apps: &[],
        };
        let shown = for_viewer(&releases, 0, &member);
        assert_eq!(shown.len(), 1);
        assert_eq!(shown[0].groups.len(), 1);
        assert_eq!(shown[0].groups[0].heading, None);
        let apps = ["structures".to_owned()];
        let admin = Viewer {
            admin: true,
            apps: &apps,
        };
        let shown = for_viewer(&releases, 0, &admin);
        assert_eq!(shown.len(), 2);
        // Everyone's first.
        assert_eq!(shown[0].groups[0].heading, None);
        assert_eq!(shown[0].groups[1].heading.as_deref(), Some("Structures"));
        assert_eq!(
            shown[1].groups[0].heading.as_deref(),
            Some("Administration")
        );
        // Seen: nothing.
        assert!(for_viewer(&releases, 2, &admin).is_empty());
        assert_eq!(for_viewer(&releases, 1, &admin).len(), 1);
    }

    #[test]
    fn the_popup_shows_the_newest_few() {
        let view = |n: usize| ReleaseView {
            title: n.to_string(),
            groups: Vec::new(),
        };
        let popup = WhatsNew::build((0..5).map(view).collect(), Vec::new()).unwrap();
        assert_eq!(popup.releases.len(), POPUP_RELEASES);
        assert_eq!(popup.more, 2);
        assert!(WhatsNew::build(Vec::new(), Vec::new()).is_none());
    }

    /// The shipped changelog reads as intended: releases newest first by
    /// date, each with notes, every app heading naming a first-party app.
    #[test]
    fn the_changelog_is_well_formed() {
        let releases = releases();
        assert!(!releases.is_empty());
        let apps: Vec<String> =
            std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/../../plugins"))
                .unwrap()
                .filter_map(|entry| {
                    let manifest =
                        std::fs::read_to_string(entry.ok()?.path().join("plugin.toml")).ok()?;
                    manifest
                        .lines()
                        .find_map(|l| l.strip_prefix("name = \""))
                        .map(|n| n.trim_end_matches('"').to_owned())
                })
                .collect();
        let mut previous: Option<&str> = None;
        for release in releases {
            let date = release.title.as_str();
            assert!(
                chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").is_ok(),
                "a release is headed by its date (YYYY-MM-DD): {date:?}"
            );
            if let Some(newer) = previous {
                assert!(newer >= date, "newest first: {newer} before {date}");
            }
            previous = Some(date);
            assert!(!release.groups.is_empty(), "{date} has no notes");
            for group in &release.groups {
                assert!(!group.notes.is_empty(), "{date}: an empty heading");
                if let Audience::App(name) = &group.audience {
                    assert!(
                        apps.iter().any(|a| a == name),
                        "{date}: no first-party app is called {name:?} (Everyone, Admins or an app's name)"
                    );
                }
            }
        }
    }
}
