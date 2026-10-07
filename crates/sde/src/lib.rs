//! CCP's static data export, the parts apps need, built into Tether:
//! types (names, groups, market groups, volumes, portion sizes, meta level,
//! compressed forms), groups, categories, the market group tree,
//! reprocessing materials and solar systems' names.
//!
//! ESI has no reprocessing table, and searching every type by name would
//! take thousands of calls, so `scripts/update-sde.sh` extracts these from
//! CCP's own export (developers.eveonline.com) into `data/`, which is
//! committed: neither the image build nor the running app fetches it
//! (Jay, 2026-10-07). Apps read it through the catalogue's `sde-*`
//! endpoints (`crates/web-core/src/static_data.rs`).

use std::collections::HashMap;
use std::sync::OnceLock;

/// An item type.
#[derive(Debug, Clone, PartialEq)]
pub struct Type {
    pub id: i64,
    /// English, as the game's inventory writes it (what a paste carries).
    pub name: String,
    pub group_id: i64,
    pub market_group_id: Option<i64>,
    /// m³ assembled.
    pub volume: f64,
    /// m³ packaged (a ship's repackaged volume; else the same).
    pub packaged_volume: f64,
    /// How many units reprocess together.
    pub portion_size: i64,
    pub published: bool,
    pub meta_level: Option<i64>,
    /// The compressed form of an ore or ice.
    pub compressed_type_id: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    pub id: i64,
    pub name: String,
    pub category_id: i64,
    pub published: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarketGroup {
    pub id: i64,
    pub name: String,
    pub parent_id: Option<i64>,
}

/// One material a type reprocesses into, per portion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Material {
    pub type_id: i64,
    pub quantity: i64,
}

/// The built-in data.
#[derive(Debug, Default)]
pub struct Sde {
    /// The SDE build, and when CCP released it.
    pub build: String,
    pub released: String,
    types: HashMap<i64, Type>,
    /// Exact name to type: a published type before an unpublished one of
    /// the same name, then the lowest id.
    by_name: HashMap<String, i64>,
    /// Published types, by lowercase name, for searches.
    searchable: Vec<(String, i64)>,
    groups: HashMap<i64, Group>,
    categories: HashMap<i64, String>,
    market_groups: HashMap<i64, MarketGroup>,
    materials: HashMap<i64, Vec<Material>>,
    systems: Vec<(i64, String)>,
}

fn rows(table: &str) -> impl Iterator<Item = Vec<&str>> {
    table.lines().skip(1).map(|line| line.split('\t').collect())
}

fn int(field: Option<&&str>) -> Option<i64> {
    field.and_then(|f| f.parse().ok())
}

fn float(field: Option<&&str>) -> f64 {
    field.and_then(|f| f.parse().ok()).unwrap_or(0.0)
}

fn text(field: Option<&&str>) -> String {
    field.map(|f| (*f).to_owned()).unwrap_or_default()
}

impl Sde {
    /// Reads the tables (as `data/` holds them).
    #[allow(clippy::too_many_arguments)]
    pub fn parse(
        build: &str,
        types: &str,
        groups: &str,
        categories: &str,
        market_groups: &str,
        materials: &str,
        systems: &str,
    ) -> Self {
        let mut sde = Sde::default();
        if let Some(b) = rows(build).next() {
            sde.build = text(b.first());
            sde.released = text(b.get(1));
        }
        for r in rows(types) {
            let Some(id) = int(r.first()) else { continue };
            let t = Type {
                id,
                name: text(r.get(1)),
                group_id: int(r.get(2)).unwrap_or_default(),
                market_group_id: int(r.get(3)),
                volume: float(r.get(4)),
                packaged_volume: float(r.get(5)),
                portion_size: int(r.get(6)).filter(|p| *p > 0).unwrap_or(1),
                published: r.get(7) == Some(&"1"),
                meta_level: int(r.get(8)),
                compressed_type_id: int(r.get(9)),
            };
            sde.types.insert(id, t);
        }
        let mut names: Vec<&Type> = sde.types.values().collect();
        names.sort_by_key(|t| (!t.published, t.id));
        for t in names {
            sde.by_name.entry(t.name.clone()).or_insert(t.id);
            if t.published {
                sde.searchable.push((t.name.to_lowercase(), t.id));
            }
        }
        sde.searchable.sort();
        for r in rows(groups) {
            let Some(id) = int(r.first()) else { continue };
            sde.groups.insert(
                id,
                Group {
                    id,
                    name: text(r.get(1)),
                    category_id: int(r.get(2)).unwrap_or_default(),
                    published: r.get(3) == Some(&"1"),
                },
            );
        }
        for r in rows(categories) {
            if let Some(id) = int(r.first()) {
                sde.categories.insert(id, text(r.get(1)));
            }
        }
        for r in rows(market_groups) {
            let Some(id) = int(r.first()) else { continue };
            sde.market_groups.insert(
                id,
                MarketGroup {
                    id,
                    name: text(r.get(1)),
                    parent_id: int(r.get(2)),
                },
            );
        }
        for r in rows(materials) {
            let (Some(id), Some(material), Some(quantity)) =
                (int(r.first()), int(r.get(1)), int(r.get(2)))
            else {
                continue;
            };
            sde.materials.entry(id).or_default().push(Material {
                type_id: material,
                quantity,
            });
        }
        for r in rows(systems) {
            if let Some(id) = int(r.first()) {
                sde.systems.push((id, text(r.get(1))));
            }
        }
        sde.systems.sort_by(|a, b| a.1.cmp(&b.1));
        sde
    }

    pub fn type_by_id(&self, id: i64) -> Option<&Type> {
        self.types.get(&id)
    }

    /// The type with exactly this name (a published one first).
    pub fn type_by_name(&self, name: &str) -> Option<&Type> {
        self.by_name.get(name).and_then(|id| self.types.get(id))
    }

    /// Published types whose name contains `query` (any case), names
    /// starting with it first, then by name; at most `limit`.
    pub fn search_types(&self, query: &str, limit: usize) -> Vec<&Type> {
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            return Vec::new();
        }
        let mut found: Vec<(bool, &str, i64)> = self
            .searchable
            .iter()
            .filter(|(name, _)| name.contains(&q))
            .map(|(name, id)| (!name.starts_with(&q), name.as_str(), *id))
            .collect();
        found.sort();
        found
            .into_iter()
            .filter_map(|(_, _, id)| self.types.get(&id))
            .take(limit)
            .collect()
    }

    pub fn group(&self, id: i64) -> Option<&Group> {
        self.groups.get(&id)
    }

    pub fn category_name(&self, id: i64) -> Option<&str> {
        self.categories.get(&id).map(String::as_str)
    }

    /// Groups whose name contains `query` (any case), by name.
    pub fn search_groups(&self, query: &str, limit: usize) -> Vec<&Group> {
        let q = query.trim().to_lowercase();
        let mut found: Vec<&Group> = self
            .groups
            .values()
            .filter(|g| !q.is_empty() && g.name.to_lowercase().contains(&q))
            .collect();
        found.sort_by(|a, b| a.name.cmp(&b.name).then(a.id.cmp(&b.id)));
        found.truncate(limit);
        found
    }

    pub fn market_group(&self, id: i64) -> Option<&MarketGroup> {
        self.market_groups.get(&id)
    }

    /// A market group and its ancestors, the group first, up to the root
    /// (a loop in the data stops at 20).
    pub fn market_group_chain(&self, id: i64) -> Vec<&MarketGroup> {
        let mut chain = Vec::new();
        let mut at = self.market_groups.get(&id);
        while let Some(group) = at {
            if chain.len() >= 20 {
                break;
            }
            chain.push(group);
            at = group.parent_id.and_then(|p| self.market_groups.get(&p));
        }
        chain
    }

    /// Market groups whose name contains `query` (any case), by name.
    pub fn search_market_groups(&self, query: &str, limit: usize) -> Vec<&MarketGroup> {
        let q = query.trim().to_lowercase();
        let mut found: Vec<&MarketGroup> = self
            .market_groups
            .values()
            .filter(|g| !q.is_empty() && g.name.to_lowercase().contains(&q))
            .collect();
        found.sort_by(|a, b| a.name.cmp(&b.name).then(a.id.cmp(&b.id)));
        found.truncate(limit);
        found
    }

    /// What one portion of the type reprocesses into.
    pub fn materials(&self, id: i64) -> &[Material] {
        self.materials.get(&id).map_or(&[], Vec::as_slice)
    }

    pub fn system_name(&self, id: i64) -> Option<&str> {
        self.systems
            .iter()
            .find(|(s, _)| *s == id)
            .map(|(_, n)| n.as_str())
    }

    /// Solar systems whose name contains `query` (any case), by name.
    pub fn search_systems(&self, query: &str, limit: usize) -> Vec<(i64, &str)> {
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            return Vec::new();
        }
        self.systems
            .iter()
            .filter(|(_, n)| n.to_lowercase().contains(&q))
            .map(|(id, n)| (*id, n.as_str()))
            .take(limit)
            .collect()
    }
}

/// The data this build carries, read once.
pub fn sde() -> &'static Sde {
    static SDE: OnceLock<Sde> = OnceLock::new();
    SDE.get_or_init(|| {
        Sde::parse(
            include_str!("../data/build.tsv"),
            include_str!("../data/types.tsv"),
            include_str!("../data/groups.tsv"),
            include_str!("../data/categories.tsv"),
            include_str!("../data/market_groups.tsv"),
            include_str!("../data/materials.tsv"),
            include_str!("../data/systems.tsv"),
        )
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)] // test code

    use super::sde;

    /// The built-in data reads, with what apps lean on: an ore's portion,
    /// materials and compressed form, a ship's packaged volume, the
    /// market tree, and searches.
    #[test]
    fn the_built_in_data_reads() {
        let sde = sde();
        assert!(!sde.build.is_empty());
        let veldspar = sde.type_by_name("Veldspar").unwrap();
        assert_eq!(veldspar.portion_size, 100);
        assert!(veldspar.compressed_type_id.is_some());
        assert!(sde.materials(veldspar.id).iter().any(|m| m.type_id == 34));
        let rifter = sde.type_by_id(587).unwrap();
        assert_eq!(rifter.name, "Rifter");
        assert_eq!(rifter.packaged_volume, 2500.0);
        assert_eq!(rifter.meta_level, Some(0));
        let chain = sde.market_group_chain(rifter.market_group_id.unwrap());
        assert!(chain.len() > 1);
        assert!(chain.last().unwrap().parent_id.is_none());
        let found = sde.search_types("rifter", 10);
        assert_eq!(found.first().unwrap().name, "Rifter");
        assert_eq!(
            sde.category_name(sde.group(rifter.group_id).unwrap().category_id),
            Some("Ship")
        );
        assert_eq!(sde.system_name(30000142), Some("Jita"));
        assert!(!sde.search_systems("jit", 5).is_empty());
    }
}
