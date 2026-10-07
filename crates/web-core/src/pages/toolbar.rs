//! The toolbar over a list (DESIGN.md, Toolbar), core pages' and apps'
//! alike (`templates/toolbar.html`): a search box kept in the address,
//! the filters applied as chips with "+ Filter" for the rest, the tabs as
//! view chips, and a CSV button where the list can be downloaded.
//!
//! A core page describes its list's address with [`ListQuery`] (its path
//! and the parameters it reads) and builds its toolbar from it; every link
//! keeps the rest of the list as it is and goes back to its first page.
//! Its search and filters run on the server, so the address is the list.

use super::encode;

/// The search box's parameter, the same on every list.
pub const SEARCH: &str = "q";
/// A search reads at most this much of the box, and this many different
/// words: a long search costs no more than a short one.
const SEARCH_BYTES: usize = 200;
const SEARCH_WORDS: usize = 8;

/// A link among others, one of them current: a view chip, a filter's
/// value, a tab.
#[derive(Clone)]
pub struct TabLink {
    pub label: String,
    pub href: String,
    pub current: bool,
}

/// The toolbar above a list.
pub struct ToolbarView {
    /// The search box's id, the page's own: htmx keeps the box (and what's
    /// typed in it) across this page's answers, and no other page's.
    pub id: String,
    /// What the toolbar's links and form swap: an app page's content
    /// (`#plugin-content`), or `None` for the whole page (core pages,
    /// which are boosted).
    pub target: Option<&'static str>,
    /// The page's address: the search box's form goes there, keeping
    /// `keep` (the filters, the tab and the page's other parameters).
    pub action: String,
    pub keep: Vec<(String, String)>,
    /// The search box's words; `None` draws none.
    pub placeholder: Option<String>,
    pub q: String,
    /// Tether's own search among the rows shown: the browser hides the
    /// rows not matching as you type, before the server answers.
    pub instant: bool,
    /// Filters applied: a chip each, with a × taking it off.
    pub chips: Vec<ChipView>,
    /// "+ Filter": every filter with its values.
    pub filters: Vec<FilterView>,
    /// The page's tabs, as view chips.
    pub views: Vec<TabLink>,
    /// The list as a CSV file, with its search and filters.
    pub csv: Option<String>,
}

pub struct ChipView {
    pub label: String,
    pub value: String,
    pub remove: String,
}

pub struct FilterView {
    pub label: String,
    pub choices: Vec<TabLink>,
    /// A range typed in (two dates), besides the choices.
    pub range: Option<RangeView>,
}

/// Two dates in "+ Filter", sent as the list's address with them set.
pub struct RangeView {
    pub action: String,
    pub keep: Vec<(String, String)>,
    pub from: (String, String),
    pub to: (String, String),
}

impl ToolbarView {
    /// An empty toolbar for `list`, swapping the whole page.
    pub fn new(list: &ListQuery) -> Self {
        Self {
            id: format!(
                "q-{}",
                list.path
                    .bytes()
                    .map(|b| format!("{b:02x}"))
                    .collect::<String>()
            ),
            target: None,
            action: list.path.clone(),
            keep: list.keep(&[SEARCH]),
            placeholder: None,
            q: list.get(SEARCH).to_owned(),
            instant: false,
            chips: Vec::new(),
            filters: Vec::new(),
            views: Vec::new(),
            csv: None,
        }
    }

    /// The search box, its words in the address as `q`.
    pub fn search(mut self, placeholder: &str) -> Self {
        self.placeholder = Some(placeholder.to_owned());
        self
    }

    /// A filter taking one of `choices` (`(value, label)`) in `param`: a
    /// chip when one is chosen, and its values under "+ Filter". No
    /// filter without a choice.
    pub fn filter<V: Into<String>, L: Into<String>>(
        mut self,
        list: &ListQuery,
        label: &str,
        param: &str,
        choices: impl IntoIterator<Item = (V, L)>,
    ) -> Self {
        let chosen = list.get(param);
        let mut links = Vec::new();
        for (value, text) in choices {
            let (value, text) = (value.into(), text.into());
            let current = value == chosen;
            if current {
                self.chips.push(ChipView {
                    label: label.to_owned(),
                    value: text.clone(),
                    remove: list.with(param, None),
                });
            }
            links.push(TabLink {
                label: text,
                href: list.with(param, Some(&value)),
                current,
            });
        }
        if !links.is_empty() {
            self.filters.push(FilterView {
                label: label.to_owned(),
                choices: links,
                range: None,
            });
        }
        self
    }

    /// A chip for a filter applied that isn't among a menu's values (a
    /// pilot chosen from a row), taking off `params`.
    pub fn chip(mut self, list: &ListQuery, label: &str, value: String, params: &[&str]) -> Self {
        self.chips.push(ChipView {
            label: label.to_owned(),
            value,
            remove: list.without(params),
        });
        self
    }

    /// The tabs as view chips: `choices` (`(value, label)`) in `param`,
    /// the first the list without it.
    pub fn views<V: AsRef<str>, L: Into<String>>(
        mut self,
        list: &ListQuery,
        param: &str,
        choices: impl IntoIterator<Item = (V, L)>,
    ) -> Self {
        let chosen = list.get(param);
        let mut first = true;
        for (value, text) in choices {
            let value = value.as_ref();
            let current = if first {
                chosen.is_empty() || chosen == value
            } else {
                chosen == value
            };
            self.views.push(TabLink {
                label: text.into(),
                href: list.with(param, (!first).then_some(value)),
                current,
            });
            first = false;
        }
        self
    }

    /// The CSV button, to `href`.
    pub fn csv(mut self, href: String) -> Self {
        self.csv = Some(href);
        self
    }

    /// Whether there's anything to draw.
    pub fn is_empty(&self) -> bool {
        self.placeholder.is_none()
            && self.chips.is_empty()
            && self.filters.is_empty()
            && self.views.is_empty()
            && self.csv.is_none()
    }
}

/// A list's address: its path and the parameters it reads, those set.
#[derive(Clone, Debug)]
pub struct ListQuery {
    pub path: String,
    params: Vec<(String, String)>,
}

impl ListQuery {
    pub fn new(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            params: Vec::new(),
        }
    }

    /// The same list at another address (its CSV).
    pub fn at(&self, path: &str) -> Self {
        Self {
            path: path.to_owned(),
            params: self.params.clone(),
        }
    }

    /// Adds `name` when `value` (trimmed) isn't empty. The search keeps
    /// what's read of it ([`cut`]), so a long one isn't copied into every
    /// link.
    pub fn param(mut self, name: &str, value: &str) -> Self {
        let value = if name == SEARCH {
            cut(value)
        } else {
            value.trim()
        };
        if !value.is_empty() {
            self.params.push((name.to_owned(), value.to_owned()));
        }
        self
    }

    /// A parameter's value (`""` when it isn't there).
    pub fn get(&self, name: &str) -> &str {
        self.params
            .iter()
            .find(|(k, _)| k == name)
            .map_or("", |(_, v)| v.as_str())
    }

    /// The search's words ([`words`]).
    pub fn words(&self) -> Vec<String> {
        words(self.get(SEARCH))
    }

    /// The address as it is.
    pub fn href(&self) -> String {
        self.without(&[])
    }

    /// The address with `name` set to `value`, or taken out.
    pub fn with(&self, name: &str, value: Option<&str>) -> String {
        let mut parts: Vec<String> = self
            .params
            .iter()
            .filter(|(k, _)| k != name)
            .map(|(k, v)| format!("{}={}", encode(k), encode(v)))
            .collect();
        if let Some(value) = value {
            parts.push(format!("{}={}", encode(name), encode(value)));
        }
        address(&self.path, &parts)
    }

    /// The address without `names`.
    pub fn without(&self, names: &[&str]) -> String {
        let parts: Vec<String> = self
            .params
            .iter()
            .filter(|(k, _)| !names.contains(&k.as_str()))
            .map(|(k, v)| format!("{}={}", encode(k), encode(v)))
            .collect();
        address(&self.path, &parts)
    }

    /// The address with `extra` added (a page of the list).
    pub fn and(&self, extra: &[(&str, &str)]) -> String {
        let mut parts: Vec<String> = self
            .params
            .iter()
            .map(|(k, v)| format!("{}={}", encode(k), encode(v)))
            .collect();
        parts.extend(
            extra
                .iter()
                .map(|(k, v)| format!("{}={}", encode(k), encode(v))),
        );
        address(&self.path, &parts)
    }

    /// The parameters but `names`, for a form's hidden fields.
    pub fn keep(&self, names: &[&str]) -> Vec<(String, String)> {
        self.params
            .iter()
            .filter(|(k, _)| !names.contains(&k.as_str()))
            .cloned()
            .collect()
    }
}

fn address(path: &str, parts: &[String]) -> String {
    if parts.is_empty() {
        path.to_owned()
    } else {
        format!("{path}?{}", parts.join("&"))
    }
}

/// What's read of a search: trimmed, its first [`SEARCH_BYTES`].
pub fn cut(q: &str) -> &str {
    let q = q.trim();
    if q.len() <= SEARCH_BYTES {
        return q;
    }
    let mut end = SEARCH_BYTES;
    while !q.is_char_boundary(end) {
        end -= 1;
    }
    q[..end].trim_end()
}

/// A search's words, lowercase: its first [`SEARCH_WORDS`] different
/// ones, from its first [`SEARCH_BYTES`].
pub fn words(q: &str) -> Vec<String> {
    let mut words: Vec<String> = Vec::new();
    for word in cut(q).split_whitespace().map(str::to_lowercase) {
        if words.len() == SEARCH_WORDS {
            break;
        }
        if !words.contains(&word) {
            words.push(word);
        }
    }
    words
}

/// Whether a row whose text is `fields` holds every word, whatever their
/// case (no words: every row).
pub fn matches(words: &[String], fields: &[&str]) -> bool {
    if words.is_empty() {
        return true;
    }
    let text = fields
        .iter()
        .map(|f| f.to_lowercase())
        .collect::<Vec<_>>()
        .join(" ");
    words.iter().all(|w| text.contains(w.as_str()))
}

/// The query of the page the browser is on (htmx's `HX-Current-URL`)
/// when it's `path`, else the default: an action answered with the page
/// itself shows it with the search and filters it had.
pub fn current_query<T>(origin: &str, headers: &axum::http::HeaderMap, path: &str) -> T
where
    T: serde::de::DeserializeOwned + Default,
{
    let Some(page) = super::stay::current_page(origin, headers) else {
        return T::default();
    };
    if page.split('?').next() != Some(path) {
        return T::default();
    }
    axum::http::Uri::try_from(page.as_str())
        .ok()
        .and_then(|uri| axum::extract::Query::<T>::try_from_uri(&uri).ok())
        .map_or_else(T::default, |q| q.0)
}

/// What an empty list says under a search: `Nothing matches “jita”.`
pub fn nothing_matches(q: &str) -> String {
    format!("Nothing matches \u{201c}{}\u{201d}.", q.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_keep_the_rest_of_the_list() {
        let list = ListQuery::new("/admin/users")
            .param("q", " jita ")
            .param("state", "3")
            .param("status", "");
        assert_eq!(list.href(), "/admin/users?q=jita&state=3");
        assert_eq!(list.with("state", Some("4")), "/admin/users?q=jita&state=4");
        assert_eq!(list.with("state", None), "/admin/users?q=jita");
        assert_eq!(list.without(&["q", "state"]), "/admin/users");
        assert_eq!(
            list.and(&[("before", "10")]),
            "/admin/users?q=jita&state=3&before=10"
        );
        let t = ToolbarView::new(&list)
            .search("Name")
            .filter(&list, "State", "state", [("3", "Member"), ("4", "Blue")])
            .views(&list, "view", [("all", "All"), ("mine", "Mine")]);
        assert_eq!(t.keep, vec![("state".to_owned(), "3".to_owned())]);
        assert_eq!(t.q, "jita");
        assert_eq!(t.chips.len(), 1);
        assert_eq!(t.chips[0].value, "Member");
        assert_eq!(t.chips[0].remove, "/admin/users?q=jita");
        assert!(t.filters[0].choices[0].current);
        assert!(t.views[0].current && !t.views[1].current);
        assert_eq!(t.views[1].href, "/admin/users?q=jita&state=3&view=mine");
        assert_eq!(t.views[0].href, "/admin/users?q=jita&state=3");
    }

    #[test]
    fn a_search_reads_a_few_different_words() {
        assert_eq!(words("  Alpha alpha BETA "), vec!["alpha", "beta"]);
        assert_eq!(words("a b c d e f g h i j").len(), 8);
        assert!(matches(&words("rif PIL"), &["Rifter", "Pilot"]));
        assert!(!matches(&words("rif zzz"), &["Rifter", "Pilot"]));
        assert!(matches(&[], &["anything"]));
        // A long search keeps what's read of it, in every link too.
        let long = "é".repeat(150);
        let list = ListQuery::new("/x").param("q", &long);
        assert_eq!(list.get("q").len(), 200);
        assert!(cut(&long).chars().all(|c| c == 'é'));
    }
}
