//! Structure names any member could read (Jay, 2026-10-05): ESI names an
//! Upwell structure only to a character that may dock there, so an app's
//! data source (a Director alt, say) may be refused a structure its
//! corporation keeps blueprints in. When that happens, Tether asks through
//! other members' characters that granted `esi-universe.read_structures.v1`
//! and keeps the name, system and type for every app, a week
//! (`core.structure_names`). Nothing else is read with those tokens, and
//! apps never see one.
//!
//! Each refusal spends ESI's error budget, so: only while the budget has
//! room ([`tether_esi::Esi::has_room`]), at most [`ASK_AT_ONCE`]
//! characters a lookup and [`MISSES_PER_HOUR`] refusals a structure an
//! hour, and a character refused a structure isn't asked it again for a
//! week.

use std::time::Duration;

use tether_db::PgPool;
use tether_db::structure_names::{self as db, Named};
use tether_esi::Esi;
use tether_esi::plugin::Target;

use tether_esi::vault::TokenVault;

/// The scope ESI names structures with.
pub const SCOPE: &str = "esi-universe.read_structures.v1";
/// How long a name is kept, and a refusal remembered.
pub const KEEP: Duration = Duration::from_secs(7 * 24 * 60 * 60);
/// Characters asked in one lookup.
pub const ASK_AT_ONCE: i64 = 3;
/// Refusals of one structure an hour before Tether stops asking.
pub const MISSES_PER_HOUR: i64 = 3;

/// A name an app's own read got: kept for the rest.
pub async fn remember(db: &PgPool, body: &serde_json::Value) {
    let Some(named) = parse(body) else { return };
    if let Err(err) = db::store(db, &named).await {
        tracing::warn!(structure = named.structure_id, error = %err, "keeping a structure's name");
    }
}

fn parse(body: &serde_json::Value) -> Option<Named> {
    let name = body["name"].as_str().filter(|n| !n.is_empty())?;
    Some(Named {
        structure_id: body["structure_id"].as_i64()?,
        name: name.chars().take(200).collect(),
        solar_system_id: body["solar_system_id"].as_i64()?,
        type_id: body["type_id"].as_i64(),
    })
}

/// The answer an app's `source-structure` or `universe-structure` gets:
/// the structure's name, system and type only.
pub fn body(named: &Named) -> serde_json::Value {
    serde_json::json!({
        "structure_id": named.structure_id,
        "name": named.name,
        "solar_system_id": named.solar_system_id,
        "type_id": named.type_id,
    })
}

/// The name kept for `structure_id`, if read in the last week: answered
/// before an app's own read, so asking again costs ESI nothing.
pub async fn kept(db: &PgPool, structure_id: i64) -> Option<Named> {
    db::get(db, structure_id, KEEP.as_secs_f64())
        .await
        .inspect_err(
            |err| tracing::warn!(structure_id, error = %err, "reading a kept structure name"),
        )
        .ok()
        .flatten()
}

/// What a lookup through members found, and what it spent: ESI calls, and
/// of them the refusals (each spends ESI's error budget, so each counts
/// against the app that asked).
#[derive(Debug, Default)]
pub struct Lookup {
    pub named: Option<Named>,
    pub calls: usize,
    pub refused: usize,
}

/// `structure_id`'s name, read now through members who may dock there.
/// One lookup of a structure at a time: one that waited finds the name, or
/// the hour's refusals spent.
pub async fn through_members(
    db: &PgPool,
    esi: &Esi,
    vault: &TokenVault,
    structure_id: i64,
) -> Lookup {
    let mut tx = match db.begin().await {
        Ok(tx) => tx,
        Err(err) => {
            tracing::warn!(structure_id, error = %err, "structure name lookup");
            return Lookup::default();
        }
    };
    if let Err(err) = db::lock(&mut tx, structure_id).await {
        tracing::warn!(structure_id, error = %err, "structure name lookup");
        return Lookup::default();
    }
    let lookup = look_up(db, esi, vault, structure_id).await;
    if let Err(err) = tx.commit().await {
        tracing::warn!(structure_id, error = %err, "structure name lookup");
    }
    lookup
}

async fn look_up(db: &PgPool, esi: &Esi, vault: &TokenVault, structure_id: i64) -> Lookup {
    if let Some(named) = kept(db, structure_id).await {
        return Lookup {
            named: Some(named),
            ..Lookup::default()
        };
    }
    if !esi.has_room() {
        return Lookup::default();
    }
    let keep = KEEP.as_secs_f64();
    let spent = db::recent_misses(db, structure_id, 3600.0)
        .await
        .unwrap_or(MISSES_PER_HOUR);
    let left = (MISSES_PER_HOUR - spent).min(ASK_AT_ONCE);
    if left <= 0 {
        return Lookup::default();
    }
    let candidates = db::candidates(db, structure_id, SCOPE, keep, left)
        .await
        .unwrap_or_default();
    let Some(endpoint) = tether_esi::plugin::endpoint("universe-structure") else {
        return Lookup::default();
    };
    let params = [("structure_id".to_owned(), structure_id.to_string())];
    let mut lookup = Lookup::default();
    for character in candidates {
        let Ok(token) = vault.access_token(character, &[SCOPE]).await else {
            continue;
        };
        let target = Target {
            character_id: character,
            corporation_id: 0,
            alliance_id: None,
        };
        lookup.calls += 1;
        match esi
            .plugin_get(endpoint, &token, target, &params, None)
            .await
        {
            Ok(answer) => {
                if let Some(named) = parse(&answer.body) {
                    if let Err(err) = db::store(db, &named).await {
                        tracing::warn!(structure_id, error = %err, "keeping a structure's name");
                    }
                    tracing::info!(structure_id, "structure named through a member");
                    lookup.named = Some(named);
                    return lookup;
                }
            }
            Err(tether_esi::EsiError::Status(401 | 403 | 404)) => {
                lookup.refused += 1;
                if let Err(err) = db::miss(db, structure_id, character).await {
                    tracing::warn!(structure_id, error = %err, "noting a refused structure");
                }
            }
            // ESI trouble: not this character's doing; stop for now.
            Err(err) => {
                tracing::info!(structure_id, error = %err, "structure name not read");
                break;
            }
        }
    }
    lookup
}
