//! An app page's lists (DESIGN.md, Toolbar and Record panel): the toolbar
//! Tether draws above them (a search box kept in the address, the app's
//! filters as chips, its tabs as view chips), the search Tether runs among
//! the rows a page shows when the page doesn't search itself, and the
//! selected row's record panel beside the list.

use tether_plugins::host::{RecordPanel, Toolbar, ToolbarFilter};
use tether_plugins::page::SEARCH_PARAM;

use super::encode;
use super::plugin_pages::{
    ActionView, SectionView, TAB, TABLE_PAGE, TabLink, TableView, ValueView,
};
use super::plugin_visuals::CompositionView;

/// Rows a table shows from which a page without a search of its own gets
/// Tether's search box (where the browser's row filter appeared).
pub const SEARCH_FROM: usize = 8;

pub use tether_web_core::pages::toolbar::{ChipView, FilterView, ToolbarView};

/// The selected row's details beside the list.
pub struct PanelView {
    pub kind: String,
    pub title: String,
    pub context: Option<String>,
    pub figure: Option<CompositionView>,
    pub facts: Vec<(String, ValueView)>,
    /// Its own page: the label and the address.
    pub open: Option<(String, String)>,
    pub action: Option<ActionView>,
    /// The list without the selection.
    pub close: String,
}

/// The page's address as it stands, for the toolbar's links: its own
/// address, the app's parameters (the search's and filters' among them),
/// the tab, and the record panel's parameter, which a change to the list
/// takes out. The tables go back to their first pages.
pub struct Here<'a> {
    pub href: &'a str,
    pub query: &'a [(String, String)],
    pub tab: usize,
    /// Each table's page (`_pN`), kept when the panel closes.
    pub pages: &'a [(usize, usize)],
    pub panel: Option<&'a str>,
}

impl Here<'_> {
    /// A parameter's value (`""` when it isn't there).
    pub fn value(&self, name: &str) -> &str {
        self.query
            .iter()
            .find(|(k, _)| k == name)
            .map_or("", |(_, v)| v.as_str())
    }

    /// The parameters kept when the list changes: all but the search, the
    /// selected row and those `set` changes.
    fn kept<'q>(&'q self, set: &'q [&str]) -> impl Iterator<Item = &'q (String, String)> {
        self.query
            .iter()
            .filter(move |(k, _)| Some(k.as_str()) != self.panel && !set.contains(&k.as_str()))
    }

    /// The address with `name` set to `value` (or taken out). Kept within
    /// what a page's address may be: past that, the rest of the query goes.
    pub fn with(&self, name: &str, value: Option<&str>) -> String {
        let mut parts: Vec<String> = self
            .kept(&[name])
            .map(|(k, v)| format!("{}={}", encode(k), encode(v)))
            .collect();
        let mut own = Vec::new();
        if self.tab > 0 {
            own.push(format!("{TAB}={}", self.tab));
        }
        if let Some(value) = value {
            own.push(format!("{}={}", encode(name), encode(value)));
        }
        parts.extend(own.iter().cloned());
        if !fits(&parts) {
            parts = own;
        }
        address(self.href, &parts)
    }

    /// The list as it is, without the selected row.
    pub fn closed(&self) -> String {
        let mut parts: Vec<String> = self
            .kept(&[])
            .map(|(k, v)| format!("{}={}", encode(k), encode(v)))
            .collect();
        if self.tab > 0 {
            parts.push(format!("{TAB}={}", self.tab));
        }
        for (n, p) in self.pages {
            parts.push(format!("{TABLE_PAGE}{n}={p}"));
        }
        address(self.href, &parts)
    }

    /// What the search box's form sends besides the search.
    fn keep(&self) -> Vec<(String, String)> {
        let mut keep: Vec<(String, String)> = self.kept(&[SEARCH_PARAM]).cloned().collect();
        if self.tab > 0 {
            keep.push((TAB.to_owned(), self.tab.to_string()));
        }
        keep
    }
}

/// Whether a query of these `name=value` parts is one a page may have.
pub fn fits(parts: &[String]) -> bool {
    parts.len() <= super::plugin_pages::MAX_QUERY_PAIRS
        && parts.iter().map(|p| p.len() + 1).sum::<usize>() <= super::plugin_pages::MAX_QUERY_BYTES
}

fn address(href: &str, parts: &[String]) -> String {
    if parts.is_empty() {
        href.to_owned()
    } else {
        format!("{href}?{}", parts.join("&"))
    }
}

/// The toolbar for a page whose tables have up to `longest` rows: the
/// app's search or Tether's (from [`SEARCH_FROM`] rows, or while a search
/// is in the address), its filters, and its tabs as view chips. `None`
/// when there's nothing to put in it.
pub fn toolbar(
    toolbar: Option<&Toolbar>,
    here: &Here,
    views: Vec<TabLink>,
    longest: usize,
) -> Option<ToolbarView> {
    let q = here.value(SEARCH_PARAM).trim();
    let own = toolbar.and_then(|t| t.search.clone());
    let instant = own.is_none();
    let placeholder = own.or_else(|| {
        (longest >= SEARCH_FROM || !q.is_empty()).then(|| "Search these rows".to_owned())
    });
    let filters = toolbar.map_or(&[][..], |t| t.filters.as_slice());
    if placeholder.is_none() && filters.is_empty() && views.is_empty() {
        return None;
    }
    // Each filter's values chosen, of those it offers: one, or (taking
    // several) any, joined by commas in the address.
    let chosen = |f: &ToolbarFilter| -> Vec<String> {
        let value = here.value(&f.param);
        let values: Vec<&str> = if f.multiple {
            value.split(',').collect()
        } else {
            vec![value]
        };
        f.choices
            .iter()
            .filter(|c| values.contains(&c.value.as_str()))
            .map(|c| c.value.clone())
            .collect()
    };
    // The filter's address with these values (none: taken off).
    let set = |f: &ToolbarFilter, values: &[&str]| {
        here.with(
            &f.param,
            (!values.is_empty()).then(|| values.join(",")).as_deref(),
        )
    };
    let mut chips = Vec::new();
    let mut menus = Vec::new();
    for f in filters {
        let on = chosen(f);
        for c in f.choices.iter().filter(|c| on.contains(&c.value)) {
            let rest: Vec<&str> = on
                .iter()
                .map(String::as_str)
                .filter(|v| *v != c.value)
                .collect();
            chips.push(ChipView {
                label: f.label.clone(),
                value: c.label.clone(),
                remove: set(f, &rest),
            });
        }
        menus.push(FilterView {
            label: f.label.clone(),
            range: None,
            choices: f
                .choices
                .iter()
                .map(|c| {
                    let current = on.contains(&c.value);
                    // Taking several: a value goes on or off the list;
                    // else it's the one.
                    let values: Vec<&str> = if !f.multiple {
                        vec![c.value.as_str()]
                    } else if current {
                        on.iter()
                            .map(String::as_str)
                            .filter(|v| *v != c.value)
                            .collect()
                    } else {
                        on.iter()
                            .map(String::as_str)
                            .chain([c.value.as_str()])
                            .collect()
                    };
                    TabLink {
                        label: c.label.clone(),
                        href: set(f, &values),
                        current,
                    }
                })
                .collect(),
        });
    }
    Some(ToolbarView {
        target: Some("#plugin-content"),
        csv: None,
        id: format!(
            "q-{}",
            here.href
                .bytes()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        ),
        action: here.href.to_owned(),
        keep: here.keep(),
        placeholder,
        q: q.to_owned(),
        instant,
        chips,
        filters: menus,
        views,
    })
}

/// The most rows any table among `views` has.
pub fn longest(views: &[SectionView]) -> usize {
    views
        .iter()
        .map(|view| match view {
            SectionView::Row(members) => longest(members),
            SectionView::Table(table) => table.rows.len(),
            SectionView::Tables(set) => set.groups.iter().map(|g| g.rows.len()).sum(),
            _ => 0,
        })
        .max()
        .unwrap_or(0)
}

/// Tether's search among the rows a page shows: a row stays if its words
/// (every cell's) hold each of the search's, whatever their case
/// ([`tether_web_core::pages::toolbar::words`]).
pub fn find_rows(views: &mut [SectionView], q: &str) {
    let words = tether_web_core::pages::toolbar::words(q);
    if words.is_empty() {
        return;
    }
    find(views, &words, tether_web_core::pages::toolbar::cut(q));
}

fn find(views: &mut [SectionView], words: &[String], q: &str) {
    for view in views {
        match view {
            SectionView::Row(members) => find(members, words, q),
            SectionView::Table(table) => keep_matching(table, words, q),
            SectionView::Tables(set) => {
                for table in &mut set.groups {
                    keep_matching(table, words, q);
                }
            }
            _ => {}
        }
    }
}

fn keep_matching(table: &mut TableView, words: &[String], q: &str) {
    if table.rows.is_empty() {
        return;
    }
    table.rows.retain(|row| {
        let text = row
            .iter()
            .map(|v| v.text.to_lowercase())
            .collect::<Vec<_>>()
            .join(" ");
        words.iter().all(|w| text.contains(w.as_str()))
    });
    if table.rows.is_empty() {
        table.empty = Some(tether_web_core::pages::toolbar::nothing_matches(q));
    }
}

/// Marks the row in each table that selects the record panel shown (one
/// of its links does), once the tables are paged.
pub fn mark_selected(views: &mut [SectionView]) {
    for view in views {
        match view {
            SectionView::Row(members) => mark_selected(members),
            SectionView::Table(table) => mark(table),
            SectionView::Tables(set) => {
                for table in &mut set.groups {
                    mark(table);
                }
            }
            _ => {}
        }
    }
}

fn mark(table: &mut TableView) {
    table.selected = table
        .rows
        .iter()
        .position(|row| row.iter().any(|v| v.selects));
}

/// A page's sections without tabs: those before its first list, and the
/// list on (the toolbar goes between).
pub fn split_at_list(mut views: Vec<SectionView>) -> (Vec<SectionView>, Vec<SectionView>) {
    let at = views.iter().position(has_table).unwrap_or(views.len());
    let list = views.split_off(at);
    (views, list)
}

fn has_table(view: &SectionView) -> bool {
    match view {
        SectionView::Table(_) | SectionView::Tables(_) => true,
        SectionView::Row(members) => members.iter().any(has_table),
        _ => false,
    }
}

/// The record panel, its values drawn by `value` and its action by
/// `action` (the page's own).
pub fn panel(
    panel: &RecordPanel,
    here: &Here,
    value: impl Fn(&tether_plugins::host::Value) -> ValueView,
    open: impl Fn(&str) -> String,
    action: impl Fn(&tether_plugins::host::Action) -> ActionView,
) -> PanelView {
    PanelView {
        kind: panel.kind.clone(),
        title: panel.title.clone(),
        context: panel.context.clone(),
        figure: panel
            .figure
            .as_ref()
            .map(|c| super::plugin_visuals::composition_as(c, true)),
        facts: panel
            .facts
            .iter()
            .map(|(label, v)| (label.clone(), value(v)))
            .collect(),
        open: panel
            .open
            .as_ref()
            .map(|link| (link.label.clone(), open(&link.path))),
        action: panel.action.as_ref().map(action),
        close: here.closed(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
        list.iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn the_toolbars_links_keep_the_list_and_close_the_panel() {
        let query = pairs(&[("q", "jita"), ("rarity", "32"), ("moon", "40161234")]);
        let here = Here {
            href: "/plugins/acme.moons/moons",
            query: &query,
            tab: 1,
            pages: &[(0, 3)],
            panel: Some("moon"),
        };
        assert_eq!(
            here.with("rarity", Some("64")),
            "/plugins/acme.moons/moons?q=jita&_tab=1&rarity=64"
        );
        assert_eq!(
            here.with("rarity", None),
            "/plugins/acme.moons/moons?q=jita&_tab=1"
        );
        assert_eq!(
            here.closed(),
            "/plugins/acme.moons/moons?q=jita&rarity=32&_tab=1&_p0=3"
        );
        assert_eq!(here.keep(), pairs(&[("rarity", "32"), ("_tab", "1")]));
        let bare = Here {
            href: "/plugins/acme.moons/moons",
            query: &[],
            tab: 0,
            pages: &[],
            panel: None,
        };
        assert_eq!(bare.with("rarity", None), "/plugins/acme.moons/moons");
    }

    #[test]
    fn tethers_search_reads_a_few_different_words() {
        let row = |text: &str| vec![ValueView::plain(text.to_owned())];
        let table = |rows: Vec<Vec<ValueView>>| {
            SectionView::Table(TableView {
                title: None,
                columns: Vec::new(),
                rows,
                empty: None,
                pager: None,
                selected: None,
            })
        };
        let mut views = vec![table(vec![row("alpha beta"), row("alpha gamma")])];
        // Repeats count once, and the ninth different word is past what's
        // read: "zzz" would match neither row.
        find_rows(&mut views, "alpha alpha a l p h alph lpha pha zzz");
        assert_eq!(longest(&views), 2);
        find_rows(&mut views, "GAMMA");
        assert_eq!(longest(&views), 1);
    }

    #[test]
    fn an_address_too_long_for_the_list_keeps_only_the_change() {
        let query: Vec<(String, String)> =
            (0..20).map(|n| (format!("p{n}"), "x".repeat(90))).collect();
        let here = Here {
            href: "/plugins/acme.x",
            query: &query,
            tab: 0,
            pages: &[],
            panel: None,
        };
        assert_eq!(here.with("kind", Some("ore")), "/plugins/acme.x?kind=ore");
    }

    #[test]
    fn a_short_list_gets_no_search_box_unless_the_app_has_its_own() {
        let here = Here {
            href: "/plugins/acme.x",
            query: &[],
            tab: 0,
            pages: &[],
            panel: None,
        };
        assert!(toolbar(None, &here, Vec::new(), SEARCH_FROM - 1).is_none());
        let long = toolbar(None, &here, Vec::new(), SEARCH_FROM).unwrap();
        assert!(long.instant && long.placeholder.is_some());
        let own = Toolbar {
            search: Some("Search moons".to_owned()),
            filters: Vec::new(),
        };
        let short = toolbar(Some(&own), &here, Vec::new(), 1).unwrap();
        assert_eq!(short.placeholder.as_deref(), Some("Search moons"));
        assert!(!short.instant);
    }
}
