//! `corporation-asset-places`: where a corporation's own items are, from
//! every page of its assets, as aa-blueprints reads them
//! (`update_locations_esi`: every page, the chain built from the list).
//!
//! A large corporation has hundreds of pages, more than an app's call may
//! wait for, so the host reads them in the background: one page after
//! another through the bulk gate (its error-budget backoff included),
//! each page with a token the data source's [`TokenSource`] hands out
//! then, so a read of hundreds of pages outlives one access token.
//!
//! The read is kept in memory for an hour for that corporation *and that
//! character*: only calls with the same character's token are answered
//! from it, and ESI checked that character's scopes and roles when it
//! read the pages. This is the one exception to "calls carrying a token
//! are never cached" (docs/ARCHITECTURE.md, ESI layer). A failed read is
//! answered with its error for a few minutes, again only to that
//! character, then read again. Nothing is kept on disk; a restart starts
//! empty.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use eve_esi_client::Client;
use tether_core::Secret;
use tokio::time::Instant;

use crate::client::{Esi, EsiError, Priority};
use crate::plugin::{Asset, Response, item_ids};

/// Hands out the data source's access token for the next page: one with
/// time left on it (the vault refreshes it shortly before it expires).
pub type TokenSource = Arc<
    dyn Fn() -> Pin<Box<dyn Future<Output = Result<Secret<String>, EsiError>> + Send>>
        + Send
        + Sync,
>;

/// The most pages of a corporation's assets read: about two million
/// items. A corporation with more isn't read at all (its places would be
/// wrong or missing), and the call says so.
pub const MAX_ASSET_PAGES: u32 = 2_000;

/// Flags of an item held by a station or an Upwell structure: a hangar
/// office or the corporation's deliveries. The walk up stops at such an
/// item, so a structure the corporation owns (itself one of its assets,
/// in space) is the place, not its system.
const IN_A_PLACE: &[&str] = &["OfficeFolder", "CorpDeliveries"];

/// How long reads are kept and waited for.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Times {
    /// A read answers for this long (ESI caches corporation assets for an
    /// hour, so a newer read would say the same).
    fresh: Duration,
    /// A failed read answers with its error for this long.
    retry: Duration,
    /// A read that hasn't finished a page for this long is taken as lost
    /// (its task gone), and a call starts another.
    stalled: Duration,
}

const TIMES: Times = Times {
    fresh: Duration::from_secs(60 * 60),
    retry: Duration::from_secs(5 * 60),
    stalled: Duration::from_secs(5 * 60),
};

/// A corporation's assets as read: by item id, each with its type, what
/// holds it and how. Flags and location types are kept once each in
/// `words`, so an item takes a few dozen bytes.
#[derive(Debug, Default)]
pub(crate) struct AssetTree {
    items: HashMap<i64, Held>,
    words: Vec<String>,
    index: HashMap<String, u32>,
}

#[derive(Debug, Clone, Copy)]
struct Held {
    type_id: i64,
    location_id: i64,
    flag: u32,
    kind: u32,
}

impl AssetTree {
    fn word(&mut self, text: String) -> u32 {
        if let Some(i) = self.index.get(&text) {
            return *i;
        }
        // Fewer than a few hundred flags and kinds exist.
        let i = u32::try_from(self.words.len()).unwrap_or(u32::MAX);
        self.words.push(text.clone());
        self.index.insert(text, i);
        i
    }

    fn add(&mut self, assets: Vec<Asset>) {
        for a in assets {
            let flag = self.word(a.location_flag);
            let kind = self.word(a.location_type);
            self.items.insert(
                a.item_id,
                Held {
                    type_id: a.type_id,
                    location_id: a.location_id,
                    flag,
                    kind,
                },
            );
        }
    }

    fn text(&self, word: u32) -> &str {
        usize::try_from(word)
            .ok()
            .and_then(|i| self.words.get(i))
            .map_or("", String::as_str)
    }
}

/// For each asked item found, in id order: the containers and hangars
/// holding it (type and flag only), innermost first, then the station,
/// structure or system at the top. Nothing about any other item.
pub(crate) fn places_in(tree: &AssetTree, ids: &[i64]) -> serde_json::Value {
    let mut ids = ids.to_vec();
    ids.sort_unstable();
    ids.dedup();
    let body = ids
        .iter()
        .filter_map(|id| tree.items.get(id).map(|item| (*id, item)))
        .map(|(id, item)| {
            let mut within = Vec::new();
            let mut at = item;
            // Containers in containers, a few deep at most; a loop in the
            // data stops at the limit.
            while tree.text(at.kind) == "item"
                && within.len() < 10
                && !IN_A_PLACE.contains(&tree.text(at.flag))
            {
                match tree.items.get(&at.location_id) {
                    Some(holder) => {
                        within.push(serde_json::json!({
                            "type_id": holder.type_id,
                            "location_flag": tree.text(holder.flag),
                        }));
                        at = holder;
                    }
                    None => break,
                }
            }
            serde_json::json!({
                "item_id": id,
                "type_id": item.type_id,
                "location_flag": tree.text(item.flag),
                "within": within,
                "place_id": at.location_id,
                "place_type": tree.text(at.kind),
            })
        })
        .collect();
    serde_json::Value::Array(body)
}

/// One corporation and character's read.
#[derive(Debug)]
enum Slot {
    /// Under way; `touched` when it started or its last page came in.
    Reading { read: u64, touched: Instant },
    Read {
        read: u64,
        at: Instant,
        tree: Arc<AssetTree>,
    },
    Failed {
        read: u64,
        at: Instant,
        error: EsiError,
    },
}

impl Slot {
    fn read(&self) -> u64 {
        match self {
            Self::Reading { read, .. } | Self::Read { read, .. } | Self::Failed { read, .. } => {
                *read
            }
        }
    }
}

/// The reads, by corporation and data-source character.
pub(crate) struct AssetTrees {
    times: Times,
    slots: Mutex<HashMap<(i64, i64), Slot>>,
    reads: AtomicU64,
}

impl Default for AssetTrees {
    fn default() -> Self {
        Self::with_times(TIMES)
    }
}

impl std::fmt::Debug for AssetTrees {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AssetTrees").finish_non_exhaustive()
    }
}

impl AssetTrees {
    pub(crate) fn with_times(times: Times) -> Self {
        Self {
            times,
            slots: Mutex::default(),
            reads: AtomicU64::new(0),
        }
    }

    fn slots(&self) -> std::sync::MutexGuard<'_, HashMap<(i64, i64), Slot>> {
        self.slots.lock().unwrap_or_else(|p| p.into_inner())
    }
}

/// What a call gets: the read to answer from, its error, or a read to
/// start (its number).
enum Next {
    Answer(Arc<AssetTree>),
    Fail(EsiError),
    Wait,
    Start(u64),
}

impl Esi {
    /// `corporation-asset-places` for `corporation`, read with
    /// `character`'s tokens from `tokens`: the asked `item_ids` (checked
    /// first) answered from a read of the last hour, or
    /// [`EsiError::Pending`] while one is under way (this call starts one
    /// if there is none), or the error the last read ended with, for a
    /// few minutes. Never waits for ESI.
    pub async fn corporation_asset_places(
        &self,
        tokens: TokenSource,
        corporation: i64,
        character: i64,
        params: &[(String, String)],
    ) -> Result<Response, EsiError> {
        let ids = item_ids(params)?;
        let key = (corporation, character);
        let trees = &self.asset_trees;
        let next = {
            let now = Instant::now();
            let mut slots = trees.slots();
            let next = match slots.get(&key) {
                Some(Slot::Read { at, tree, .. })
                    if now.duration_since(*at) < trees.times.fresh =>
                {
                    Next::Answer(tree.clone())
                }
                Some(Slot::Failed { at, error, .. })
                    if now.duration_since(*at) < trees.times.retry =>
                {
                    Next::Fail(error.clone())
                }
                Some(Slot::Reading { touched, .. })
                    if now.duration_since(*touched) < trees.times.stalled =>
                {
                    Next::Wait
                }
                _ => Next::Start(trees.reads.fetch_add(1, Ordering::Relaxed)),
            };
            if let Next::Start(read) = &next {
                // A stale read goes now: nothing answers from it.
                slots.insert(
                    key,
                    Slot::Reading {
                        read: *read,
                        touched: now,
                    },
                );
            }
            next
        };
        match next {
            Next::Answer(tree) => Ok(Response {
                body: places_in(&tree, &ids),
                pages: 1,
                refetched: 0,
            }),
            Next::Fail(error) => Err(error),
            Next::Wait => Err(EsiError::Pending),
            Next::Start(read) => {
                let esi = self.clone();
                tokio::spawn(async move {
                    esi.read_asset_tree(tokens, key, read).await;
                });
                Err(EsiError::Pending)
            }
        }
    }

    /// Reads every page into the slot for `key`, if it's still read
    /// number `read`, then forgets it once it's no longer answered from.
    async fn read_asset_tree(&self, tokens: TokenSource, key: (i64, i64), read: u64) {
        let (corporation, character) = key;
        let trees = &self.asset_trees;
        let result = self.read_assets(&tokens, key, read).await;
        let now = Instant::now();
        let (slot, keep) = match result {
            // Taken as lost meanwhile, and read again: that read answers.
            Err(EsiError::Pending) => return,
            Ok(tree) => {
                tracing::debug!(
                    corporation,
                    character,
                    items = tree.items.len(),
                    "corporation assets read"
                );
                (
                    Slot::Read {
                        read,
                        at: now,
                        tree: Arc::new(tree),
                    },
                    trees.times.fresh,
                )
            }
            Err(error) => {
                tracing::warn!(corporation, character, %error, "corporation assets not read");
                (
                    Slot::Failed {
                        read,
                        at: now,
                        error,
                    },
                    trees.times.retry,
                )
            }
        };
        {
            let mut slots = trees.slots();
            if slots.get(&key).map(Slot::read) != Some(read) {
                return;
            }
            slots.insert(key, slot);
        }
        tokio::time::sleep(keep).await;
        let mut slots = trees.slots();
        if slots.get(&key).map(Slot::read) == Some(read) {
            slots.remove(&key);
        }
    }

    /// Every page, one after another, each with a token from `tokens`
    /// (the client is built again only when the token changed).
    async fn read_assets(
        &self,
        tokens: &TokenSource,
        key: (i64, i64),
        read: u64,
    ) -> Result<AssetTree, EsiError> {
        let (corporation, _) = key;
        let mut tree = AssetTree::default();
        let mut client: Option<(Secret<String>, Client)> = None;
        let mut page = 1u32;
        loop {
            let token = tokens().await?;
            let current = client
                .as_ref()
                .filter(|(had, _)| had.expose() == token.expose())
                .map(|(_, c)| c.clone());
            let current = match current {
                Some(c) => c,
                None => {
                    let c = self.with_token(&token)?;
                    client = Some((token, c.clone()));
                    c
                }
            };
            let (items, last, _) = self
                .corporation_assets_page(&current, corporation, page, Priority::Bulk)
                .await?;
            if last > MAX_ASSET_PAGES {
                return Err(EsiError::TooManyPages(last));
            }
            tree.add(items);
            {
                let mut slots = self.asset_trees.slots();
                match slots.get_mut(&key) {
                    Some(Slot::Reading { read: r, touched }) if *r == read => {
                        *touched = Instant::now();
                    }
                    // Taken as lost and started again: stop.
                    _ => return Err(EsiError::Pending),
                }
            }
            if page >= last {
                return Ok(tree);
            }
            page += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)] // test code

    use super::*;
    use serde_json::json;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const CORPORATION: i64 = 98000001;
    const CHARACTER: i64 = 2112000001;

    fn asset(item: i64, type_id: i64, flag: &str, at: i64, kind: &str) -> Asset {
        Asset {
            item_id: item,
            type_id,
            location_id: at,
            location_flag: flag.to_owned(),
            location_type: kind.to_owned(),
            quantity: 1,
        }
    }

    fn tree(assets: Vec<Asset>) -> AssetTree {
        let mut tree = AssetTree::default();
        tree.add(assets);
        tree
    }

    #[test]
    fn containers_come_innermost_first_and_only_asked_items_answer() {
        let tree = tree(vec![
            asset(1001, 27, "OfficeFolder", 60003760, "station"),
            asset(2001, 17366, "CorpSAG2", 1001, "item"),
            asset(3001, 1000, "Unlocked", 2001, "item"),
            asset(4001, 34, "Hangar", 60008494, "station"),
        ]);
        assert_eq!(
            places_in(&tree, &[3001, 9999, 1001]),
            json!([
                {
                    "item_id": 1001, "type_id": 27, "location_flag": "OfficeFolder",
                    "within": [], "place_id": 60003760, "place_type": "station"
                },
                {
                    "item_id": 3001, "type_id": 1000, "location_flag": "Unlocked",
                    "within": [
                        {"type_id": 17366, "location_flag": "CorpSAG2"},
                        {"type_id": 27, "location_flag": "OfficeFolder"}
                    ],
                    "place_id": 60003760, "place_type": "station"
                }
            ])
        );
    }

    #[test]
    fn a_loop_in_the_data_stops_at_ten() {
        let tree = tree(vec![
            asset(1, 17366, "Unlocked", 2, "item"),
            asset(2, 17366, "Unlocked", 1, "item"),
        ]);
        let answer = places_in(&tree, &[1]);
        assert_eq!(answer[0]["within"].as_array().unwrap().len(), 10);
    }

    /// The corporation's own Upwell structure is one of its assets, in
    /// space: an office in it is placed in the structure, not its system.
    #[test]
    fn an_office_in_the_corporations_own_structure_is_in_the_structure() {
        const KEEPSTAR: i64 = 1_035_466_617_946;
        const OFFICE: i64 = 1_040_000_000_101;
        const CONTAINER: i64 = 1_040_000_000_201;
        const DELIVERED: i64 = 1_040_000_000_301;
        let tree = tree(vec![
            asset(KEEPSTAR, 35834, "AutoFit", 30004759, "solar_system"),
            asset(OFFICE, 27, "OfficeFolder", KEEPSTAR, "item"),
            asset(CONTAINER, 17366, "CorpSAG3", OFFICE, "item"),
            asset(DELIVERED, 17366, "CorpDeliveries", KEEPSTAR, "item"),
            // A starbase's hangar array and a container in it: the array is
            // in the system.
            asset(5001, 17621, "AutoFit", 30004759, "solar_system"),
            asset(5002, 17366, "CorpSAG1", 5001, "item"),
        ]);
        let answer = places_in(&tree, &[OFFICE, CONTAINER, DELIVERED, 5002]);
        let place = |i: usize| {
            (
                answer[i]["place_id"].clone(),
                answer[i]["place_type"].clone(),
            )
        };
        assert_eq!(place(0), (json!(30004759), json!("solar_system")));
        assert_eq!(answer[0]["item_id"], 5002);
        assert_eq!(
            answer[0]["within"],
            json!([{"type_id": 17621, "location_flag": "AutoFit"}])
        );
        for i in 1..=3 {
            assert_eq!(place(i), (json!(KEEPSTAR), json!("item")), "{answer}");
        }
        assert_eq!(
            answer[2]["within"],
            json!([{"type_id": 27, "location_flag": "OfficeFolder"}])
        );
    }

    fn single(token: &str) -> TokenSource {
        let token = Secret::new(token.to_owned());
        Arc::new(move || {
            let token = token.clone();
            Box::pin(async move { Ok(token) })
        })
    }

    fn page(body: serde_json::Value, pages: u32) -> ResponseTemplate {
        ResponseTemplate::new(200)
            .insert_header("x-pages", pages.to_string())
            .set_body_json(body)
    }

    fn json_asset(item: i64, at: i64) -> serde_json::Value {
        json!({
            "is_singleton": true, "item_id": item, "type_id": 17366, "quantity": 1,
            "location_flag": "CorpSAG1", "location_id": at, "location_type": "item"
        })
    }

    async fn answer(esi: &Esi, ids: &str) -> Result<Response, EsiError> {
        let params = vec![("item_ids".to_owned(), ids.to_owned())];
        for _ in 0..500 {
            match esi
                .corporation_asset_places(single("t"), CORPORATION, CHARACTER, &params)
                .await
            {
                Err(EsiError::Pending) => tokio::time::sleep(Duration::from_millis(10)).await,
                other => return other,
            }
        }
        panic!("the read never finished");
    }

    fn quick(esi: &mut Esi, fresh: u64, retry: u64) {
        esi.asset_trees = Arc::new(AssetTrees::with_times(Times {
            fresh: Duration::from_millis(fresh),
            retry: Duration::from_millis(retry),
            stalled: Duration::from_secs(60),
        }));
    }

    #[tokio::test]
    async fn a_read_older_than_its_hour_is_read_again_and_then_forgotten() {
        let server = MockServer::start().await;
        let mut esi = Esi::new("tether tests", Some(&server.uri())).unwrap();
        quick(&mut esi, 300, 50);
        Mock::given(method("GET"))
            .and(path(format!("/corporations/{CORPORATION}/assets")))
            .respond_with(page(json!([json_asset(1_040_000_000_201, 60003760)]), 1))
            .expect(2)
            .mount(&server)
            .await;
        let out = answer(&esi, "1040000000201").await.unwrap();
        assert_eq!(out.body[0]["place_id"], 60003760);
        // Within its time, from memory.
        esi.corporation_asset_places(
            single("t"),
            CORPORATION,
            CHARACTER,
            &[("item_ids".to_owned(), "1040000000201".to_owned())],
        )
        .await
        .unwrap();
        tokio::time::sleep(Duration::from_millis(400)).await;
        // Past it, the read is gone: read again.
        assert!(esi.asset_trees.slots().is_empty());
        answer(&esi, "1040000000201").await.unwrap();
    }

    #[tokio::test]
    async fn a_corporation_with_too_many_pages_is_not_read_and_says_so() {
        let server = MockServer::start().await;
        let esi = Esi::new("tether tests", Some(&server.uri())).unwrap();
        Mock::given(method("GET"))
            .and(path(format!("/corporations/{CORPORATION}/assets")))
            .and(query_param("page", "1"))
            .respond_with(page(json!([json_asset(1, 60003760)]), MAX_ASSET_PAGES + 1))
            .expect(1)
            .mount(&server)
            .await;
        let err = answer(&esi, "1").await.unwrap_err();
        assert!(
            matches!(err, EsiError::TooManyPages(n) if n == MAX_ASSET_PAGES + 1),
            "{err:?}"
        );
    }
}
