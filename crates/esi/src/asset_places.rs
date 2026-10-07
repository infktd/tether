//! `corporation-asset-places`: where a corporation's own items are, from
//! every page of its assets, as aa-blueprints reads them
//! (`update_locations_esi`: every page, the chain built from the list);
//! and `corporation-structure-assets`, what sits in its structures, from
//! the same read (Jay, 2026-10-07).
//!
//! A large corporation has hundreds of pages, more than an app's call may
//! wait for, so the host reads them in the background: one page after
//! another through the bulk gate (its error-budget backoff included),
//! each page with a token the data source's [`TokenSource`] hands out
//! then, so a read of hundreds of pages outlives one access token.
//!
//! The read is kept in memory for an hour after it finished, for that
//! corporation *and that character*: only calls with the same
//! character's token are answered from it. ESI still checks that
//! character's scopes and roles on every call: before answering from the
//! read, the call asks ESI for the first page with a fresh token, and a
//! refusal (the Director role gone) ends the read for that character.
//! This is the one exception to "nothing a token fetched is kept"
//! (docs/ARCHITECTURE.md, ESI layer): an answer may be up to an hour old,
//! plus the read's own length for its first pages. A failed read is
//! answered with its error for a few minutes, again only to that
//! character, then read again. Nothing is kept on disk; a restart starts
//! empty.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
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

/// At most this many reads run at once, each holding its pages' items
/// until done; others wait their turn.
const READS_AT_ONCE: usize = 2;

/// Items kept across every read, about 200 MB: past it the oldest reads
/// go first (a call then reads that corporation again).
const MAX_KEPT_ITEMS: usize = 4_000_000;

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
    quantity: i64,
    singleton: bool,
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
                    quantity: a.quantity,
                    singleton: a.is_singleton,
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

/// `corporation-structure-assets` from a read: what sits in structures'
/// slots and bays, and the Orbital Skyhooks (`Asset::about_structures`),
/// in item id order. Flags ships share only for items in `upwell`.
pub(crate) fn structure_items(tree: &AssetTree, upwell: Option<&[i64]>) -> serde_json::Value {
    let mut items: Vec<(i64, Asset)> = tree
        .items
        .iter()
        .map(|(id, held)| (*id, tree.asset(*id, held)))
        .filter(|(_, a)| a.about_structures(upwell))
        .collect();
    items.sort_unstable_by_key(|(id, _)| *id);
    serde_json::Value::Array(
        items
            .into_iter()
            .map(|(_, a)| {
                serde_json::json!({
                    "item_id": a.item_id,
                    "type_id": a.type_id,
                    "location_id": a.location_id,
                    "location_flag": a.location_flag,
                    "location_type": a.location_type,
                    "quantity": a.quantity,
                })
            })
            .collect(),
    )
}

impl AssetTree {
    /// The item as ESI listed it.
    fn asset(&self, id: i64, held: &Held) -> Asset {
        Asset {
            item_id: id,
            type_id: held.type_id,
            location_id: held.location_id,
            location_flag: self.text(held.flag).to_owned(),
            location_type: self.text(held.kind).to_owned(),
            quantity: held.quantity,
            is_singleton: held.singleton,
        }
    }

    /// Whether any item sits in a slot or bay ships have too.
    fn any_in_shared_slot(&self) -> bool {
        self.items
            .iter()
            .any(|(id, held)| self.asset(*id, held).in_shared_slot())
    }
}

/// Hangar flags: the corporation's hangar divisions (and an NPC
/// station's plain hangar).
const HANGAR_FLAGS: &[&str] = &[
    "Hangar", "CorpSAG1", "CorpSAG2", "CorpSAG3", "CorpSAG4", "CorpSAG5", "CorpSAG6", "CorpSAG7",
];

/// `corporation-hangar-assets` from a read, for the asked structures (or
/// stations): what's in the corporation's hangars there, by structure,
/// hangar flag and type, loose (`container_id` null) or in a container
/// in one of those hangars (its id); and those hangars' assembled items
/// that hold others (`containers`, with their type, structure and flag).
/// Office hangars are placed at their structure, as aa-buybackprogram
/// places them (`OfficeFolder`).
pub(crate) fn hangar_items(tree: &AssetTree, structures: &[i64]) -> serde_json::Value {
    let flag = |held: &Held| tree.text(held.flag);
    let office_at: HashMap<i64, i64> = tree
        .items
        .iter()
        .filter(|(_, held)| flag(held) == "OfficeFolder")
        .map(|(id, held)| (*id, held.location_id))
        .collect();
    let structures: std::collections::HashSet<i64> = structures.iter().copied().collect();
    // Items in the asked structures' hangars: where each is.
    let mut in_hangar: HashMap<i64, (i64, &str)> = HashMap::new();
    for (id, held) in &tree.items {
        if !HANGAR_FLAGS.contains(&flag(held)) {
            continue;
        }
        let at = office_at
            .get(&held.location_id)
            .copied()
            .unwrap_or(held.location_id);
        if structures.contains(&at) {
            in_hangar.insert(*id, (at, flag(held)));
        }
    }
    let holders: std::collections::HashSet<i64> = tree
        .items
        .values()
        .filter(|held| in_hangar.contains_key(&held.location_id))
        .map(|held| held.location_id)
        .collect();
    let mut stock: std::collections::BTreeMap<(i64, String, Option<i64>, i64), i64> =
        std::collections::BTreeMap::new();
    for (id, held) in &tree.items {
        let key = if let Some((at, hangar)) = in_hangar.get(id) {
            (*at, (*hangar).to_owned(), None)
        } else if let Some((at, hangar)) = in_hangar.get(&held.location_id) {
            (*at, (*hangar).to_owned(), Some(held.location_id))
        } else {
            continue;
        };
        *stock
            .entry((key.0, key.1, key.2, held.type_id))
            .or_default() += held.quantity;
    }
    let mut containers: Vec<serde_json::Value> = in_hangar
        .iter()
        .filter(|(id, _)| tree.items.get(id).is_some_and(|h| h.singleton) || holders.contains(id))
        .filter_map(|(id, (at, hangar))| {
            let held = tree.items.get(id)?;
            Some(serde_json::json!({
                "item_id": id,
                "type_id": held.type_id,
                "structure_id": at,
                "location_flag": hangar,
                "holds_items": holders.contains(id),
            }))
        })
        .collect();
    containers.sort_by_key(|c| c["item_id"].as_i64());
    serde_json::json!({
        "stock": stock
            .into_iter()
            .map(|((at, hangar, container, type_id), quantity)| serde_json::json!({
                "structure_id": at,
                "location_flag": hangar,
                "container_id": container,
                "type_id": type_id,
                "quantity": quantity,
            }))
            .collect::<Vec<_>>(),
        "containers": containers,
    })
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
    /// Reads under way (at most [`READS_AT_ONCE`]).
    running: AtomicUsize,
}

/// A read's place among those running, given back when it ends (panics
/// included).
struct Turn<'a>(&'a AtomicUsize);

impl Drop for Turn<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Past [`MAX_KEPT_ITEMS`] across every kept read, the oldest go (never
/// `newest`, which was just read).
fn trim(slots: &mut HashMap<(i64, i64), Slot>, newest: (i64, i64), max: usize) {
    let mut kept: Vec<((i64, i64), Instant, usize)> = slots
        .iter()
        .filter_map(|(key, slot)| match slot {
            Slot::Read { at, tree, .. } => Some((*key, *at, tree.items.len())),
            _ => None,
        })
        .collect();
    let mut total: usize = kept.iter().map(|(_, _, n)| n).sum();
    kept.sort_by_key(|(_, at, _)| *at);
    for (key, _, n) in kept {
        if total <= max {
            break;
        }
        if key != newest {
            slots.remove(&key);
            total -= n;
        }
    }
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
            running: AtomicUsize::new(0),
        }
    }

    fn slots(&self) -> std::sync::MutexGuard<'_, HashMap<(i64, i64), Slot>> {
        self.slots.lock().unwrap_or_else(|p| p.into_inner())
    }
}

/// What a call gets: the read to answer from (its number and tree), its
/// error, or a read to start (its number).
enum Next {
    Answer(u64, Arc<AssetTree>),
    Fail(EsiError),
    Wait,
    Start(u64),
}

impl Esi {
    /// `corporation-asset-places` for `corporation`, read with
    /// `character`'s tokens from `tokens`: the asked `item_ids` (checked
    /// first) answered from a read of the last hour once ESI let the
    /// character read the first page again now, or [`EsiError::Pending`]
    /// while one is under way (this call starts one if there is none), or
    /// the error the last read ended with, for a few minutes. Never waits
    /// for more than that one page.
    pub async fn corporation_asset_places(
        &self,
        tokens: TokenSource,
        corporation: i64,
        character: i64,
        params: &[(String, String)],
    ) -> Result<Response, EsiError> {
        let ids = item_ids(params)?;
        let (tree, again) = self.kept_read(tokens, corporation, character).await?;
        Ok(Response {
            body: places_in(&tree, &ids),
            pages: 1,
            refetched: again,
        })
    }

    /// `corporation-structure-assets` for `corporation`, from the same
    /// read as [`Self::corporation_asset_places`] (every page, however
    /// many): what sits in structures' slots and bays, and the skyhooks.
    /// Items in slots ships have too pass only for the corporation's own
    /// Upwell structures, read now with a fresh token when there are any.
    pub async fn corporation_structure_assets(
        &self,
        tokens: TokenSource,
        corporation: i64,
        character: i64,
    ) -> Result<Response, EsiError> {
        let (tree, mut again) = self
            .kept_read(tokens.clone(), corporation, character)
            .await?;
        let upwell = if tree.any_in_shared_slot() {
            again += 1;
            let token = tokens().await?;
            let client = self.with_token(&token)?;
            self.upwell_ids(&client, corporation).await
        } else {
            None
        };
        Ok(Response {
            body: structure_items(&tree, upwell.as_deref()),
            pages: 1,
            refetched: again,
        })
    }

    /// `corporation-hangar-assets` for `corporation`, from the same read
    /// as [`Self::corporation_asset_places`]: what's in its hangars at the
    /// asked `structure_ids` (up to 100), and the containers there.
    pub async fn corporation_hangar_assets(
        &self,
        tokens: TokenSource,
        corporation: i64,
        character: i64,
        params: &[(String, String)],
    ) -> Result<Response, EsiError> {
        let structures: Vec<i64> = params
            .iter()
            .find(|(k, _)| k == "structure_ids")
            .map(|(_, v)| {
                v.split(',')
                    .filter(|s| !s.trim().is_empty())
                    .map(|s| s.trim().parse::<i64>().ok().filter(|id| *id > 0))
                    .collect::<Option<Vec<i64>>>()
            })
            .unwrap_or(Some(Vec::new()))
            .ok_or_else(|| {
                EsiError::InvalidInput(
                    "structure_ids are positive numbers, comma-separated".to_owned(),
                )
            })?;
        if structures.is_empty() || structures.len() > 100 {
            return Err(EsiError::InvalidInput(
                "give 1 to 100 structure_ids".to_owned(),
            ));
        }
        let (tree, again) = self.kept_read(tokens, corporation, character).await?;
        Ok(Response {
            body: hangar_items(&tree, &structures),
            pages: 1,
            refetched: again,
        })
    }

    /// The read of the last hour for `corporation` with `character`'s
    /// tokens, once ESI let the character read the first page again now
    /// (with whether that was an extra request); [`EsiError::Pending`]
    /// while one is under way (this call starts one if there is none), or
    /// the error the last read ended with, for a few minutes.
    async fn kept_read(
        &self,
        tokens: TokenSource,
        corporation: i64,
        character: i64,
    ) -> Result<(Arc<AssetTree>, u32), EsiError> {
        let key = (corporation, character);
        let trees = &self.asset_trees;
        let next = {
            let now = Instant::now();
            let mut slots = trees.slots();
            let next = match slots.get(&key) {
                Some(Slot::Read { read, at, tree })
                    if now.duration_since(*at) < trees.times.fresh =>
                {
                    Next::Answer(*read, tree.clone())
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
            Next::Answer(read, tree) => {
                let again = self.still_allowed(&tokens, key, read).await?;
                Ok((tree, again))
            }
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

    /// Whether ESI still lets the character read the corporation's assets:
    /// the first page, asked now with a fresh token from `tokens` (never
    /// cached), so ESI checks its scopes and roles on every call answered
    /// from a kept read. A refusal (401 or 403: the role or scope gone)
    /// ends read number `read` for the character, as a failed read. With
    /// whether the page was read again (an extra request).
    async fn still_allowed(
        &self,
        tokens: &TokenSource,
        key: (i64, i64),
        read: u64,
    ) -> Result<u32, EsiError> {
        let (corporation, character) = key;
        let checked = async {
            let token = tokens().await?;
            let client = self.with_token(&token)?;
            self.corporation_assets_page(&client, corporation, 1, Priority::Bulk)
                .await
        }
        .await;
        match checked {
            Ok((_, _, again)) => Ok(again),
            Err(error) => {
                if matches!(error, EsiError::Status(401 | 403)) {
                    tracing::info!(
                        corporation,
                        character,
                        %error,
                        "corporation assets no longer readable: the kept read is dropped"
                    );
                    let mut slots = self.asset_trees.slots();
                    if slots.get(&key).map(Slot::read) == Some(read) {
                        slots.insert(
                            key,
                            Slot::Failed {
                                read,
                                at: Instant::now(),
                                error: error.clone(),
                            },
                        );
                    }
                }
                Err(error)
            }
        }
    }

    /// Reads every page into the slot for `key`, if it's still read
    /// number `read`, then forgets it once it's no longer answered from.
    async fn read_asset_tree(&self, tokens: TokenSource, key: (i64, i64), read: u64) {
        let (corporation, character) = key;
        let trees = &self.asset_trees;
        // Its turn, while it's still the read wanted (one taken as lost
        // meanwhile stops waiting).
        let turn = loop {
            let running = trees.running.load(Ordering::Acquire);
            if running < READS_AT_ONCE
                && trees
                    .running
                    .compare_exchange(running, running + 1, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
            {
                break Turn(&trees.running);
            }
            {
                let mut slots = trees.slots();
                match slots.get_mut(&key) {
                    Some(Slot::Reading { read: r, touched }) if *r == read => {
                        *touched = Instant::now();
                    }
                    _ => return,
                }
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        };
        let result = self.read_assets(&tokens, key, read).await;
        drop(turn);
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
            trim(&mut slots, key, MAX_KEPT_ITEMS);
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
            is_singleton: false,
        }
    }

    fn tree(assets: Vec<Asset>) -> AssetTree {
        let mut tree = AssetTree::default();
        tree.add(assets);
        tree
    }

    /// Hangar stock from a read: an office's hangars placed at its
    /// structure, loose items by flag and type, a container's contents
    /// under it; nothing at other structures.
    #[test]
    fn hangar_items_are_what_the_hangars_hold() {
        const STATION: i64 = 60003760;
        const OFFICE: i64 = 1_000_000_000_001;
        const BOX: i64 = 1_000_000_000_002;
        let mut container = asset(BOX, 17366, "CorpSAG2", OFFICE, "item");
        container.is_singleton = true;
        let tree = tree(vec![
            asset(OFFICE, 27, "OfficeFolder", STATION, "station"),
            asset(10, 34, "CorpSAG1", OFFICE, "item"),
            asset(11, 34, "CorpSAG1", OFFICE, "item"),
            container,
            asset(12, 35, "Unlocked", BOX, "item"),
            asset(13, 36, "CorpSAG1", 60008494, "station"),
        ]);
        let answer = hangar_items(&tree, &[STATION]);
        let stock = answer["stock"].as_array().unwrap();
        let tritanium = stock.iter().find(|s| s["type_id"] == 34).unwrap();
        assert_eq!(tritanium["quantity"], 2);
        assert_eq!(tritanium["location_flag"], "CorpSAG1");
        assert_eq!(tritanium["structure_id"], STATION);
        assert!(tritanium["container_id"].is_null());
        let boxed = stock.iter().find(|s| s["type_id"] == 35).unwrap();
        assert_eq!(boxed["container_id"], BOX);
        assert!(stock.iter().all(|s| s["type_id"] != 36));
        assert_eq!(answer["containers"][0]["item_id"], BOX);
        assert_eq!(answer["containers"][0]["holds_items"], true);
    }

    /// Structure assets from a read: slots and bays only structures have,
    /// skyhooks in space, and slots ships share only in the corporation's
    /// own Upwell structures; never hangars or ships' fittings.
    #[test]
    fn structure_items_are_what_sits_in_structures() {
        const FORTIZAR: i64 = 1_035_000_000_001;
        const SHIP: i64 = 1_035_000_000_002;
        let tree = tree(vec![
            asset(1, 35833, "ServiceSlot0", FORTIZAR, "item"),
            asset(2, 4247, "StructureFuel", FORTIZAR, "item"),
            asset(3, 2048, "HiSlot0", FORTIZAR, "item"),
            asset(4, 2048, "HiSlot0", SHIP, "item"),
            asset(5, 34, "Hangar", 60003760, "station"),
            asset(6, 81080, "AutoFit", 30000142, "solar_system"),
        ]);
        let ids = |v: serde_json::Value| -> Vec<i64> {
            v.as_array()
                .unwrap()
                .iter()
                .map(|i| i["item_id"].as_i64().unwrap())
                .collect()
        };
        assert_eq!(
            ids(structure_items(&tree, Some(&[FORTIZAR]))),
            vec![1, 2, 3, 6]
        );
        // The Upwell structures unread: shared slots pass for none.
        assert_eq!(ids(structure_items(&tree, None)), vec![1, 2, 6]);
        let fuel = &structure_items(&tree, None)[1];
        assert_eq!(fuel["quantity"], 1);
        assert_eq!(fuel["location_flag"], "StructureFuel");
    }

    /// Past the cap, the oldest kept reads go first, never the newest.
    #[test]
    fn the_oldest_reads_go_past_the_cap() {
        let now = Instant::now();
        let read = |n: i64, ago: u64| Slot::Read {
            read: 0,
            at: now - Duration::from_secs(ago),
            tree: Arc::new(tree(
                (0..n)
                    .map(|i| asset(i, 34, "Hangar", 60003760, "station"))
                    .collect(),
            )),
        };
        let mut slots = HashMap::new();
        slots.insert((1, 1), read(4, 30));
        slots.insert((2, 2), read(4, 20));
        slots.insert((3, 3), read(4, 10));
        slots.insert(
            (4, 4),
            Slot::Reading {
                read: 1,
                touched: now,
            },
        );
        trim(&mut slots, (3, 3), 8);
        let mut left: Vec<(i64, i64)> = slots.keys().copied().collect();
        left.sort_unstable();
        assert_eq!(left, vec![(2, 2), (3, 3), (4, 4)]);
        // The newest stays even alone past the cap.
        trim(&mut slots, (3, 3), 1);
        assert!(slots.contains_key(&(3, 3)));
        assert!(!slots.contains_key(&(2, 2)));
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
        // Each read is one page, and each answer asks for the first page
        // again (ESI checks the character's roles): the read, its answer,
        // the answer from memory, then the read again and its answer.
        Mock::given(method("GET"))
            .and(path(format!("/corporations/{CORPORATION}/assets")))
            .respond_with(page(json!([json_asset(1_040_000_000_201, 60003760)]), 1))
            .expect(5)
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

    /// ESI checks the character's roles on every call answered from a kept
    /// read: the Director role taken away in game, the next call is
    /// refused, and so are the character's calls after it, without asking
    /// ESI, until the read is tried again.
    #[tokio::test]
    async fn a_role_taken_away_ends_the_kept_read_at_the_next_call() {
        let server = MockServer::start().await;
        let mut esi = Esi::new("tether tests", Some(&server.uri())).unwrap();
        quick(&mut esi, 60_000, 60_000);
        // The read, then its answer's check; then the role is gone.
        Mock::given(method("GET"))
            .and(path(format!("/corporations/{CORPORATION}/assets")))
            .respond_with(page(json!([json_asset(1_040_000_000_201, 60003760)]), 1))
            .up_to_n_times(2)
            .with_priority(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/corporations/{CORPORATION}/assets")))
            .respond_with(ResponseTemplate::new(403).set_body_json(json!({"error": "Forbidden"})))
            .with_priority(2)
            .mount(&server)
            .await;
        let out = answer(&esi, "1040000000201").await.unwrap();
        assert_eq!(out.body[0]["place_id"], 60003760);
        for _ in 0..2 {
            let err = answer(&esi, "1040000000201").await.unwrap_err();
            assert!(matches!(err, EsiError::Status(403)), "{err:?}");
        }
        assert_eq!(server.received_requests().await.unwrap().len(), 3);
        assert!(matches!(
            esi.asset_trees.slots().get(&(CORPORATION, CHARACTER)),
            Some(Slot::Failed { .. })
        ));
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
