//! Entity names (alliances, corporations, characters, ...) cached in
//! Postgres: looked up constantly, and they rarely change.

use std::collections::HashMap;
use std::time::Duration;

use tether_db::PgPool;

use crate::{Esi, EsiError, NamedEntity, Priority};

/// Names older than this are fetched again.
pub const MAX_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);

#[derive(Debug, thiserror::Error)]
pub enum NamesError {
    #[error(transparent)]
    Esi(#[from] EsiError),
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

/// Names for `ids`, from the cache where fresh, otherwise from ESI (and
/// then cached). Ids ESI doesn't know are simply absent.
pub async fn resolve(
    db: &PgPool,
    esi: &Esi,
    ids: &[i64],
    priority: Priority,
) -> Result<HashMap<i64, NamedEntity>, NamesError> {
    let cached = sqlx::query!(
        r#"
        SELECT id, name, category FROM core.entity_names
        WHERE id = ANY($1) AND fetched_at > now() - make_interval(secs => $2)
        "#,
        ids,
        MAX_AGE.as_secs_f64(),
    )
    .fetch_all(db)
    .await?;
    let mut found: HashMap<i64, NamedEntity> = cached
        .into_iter()
        .map(|r| {
            (
                r.id,
                NamedEntity {
                    id: r.id,
                    name: r.name,
                    category: r.category,
                },
            )
        })
        .collect();

    let mut missing: Vec<i64> = ids
        .iter()
        .copied()
        .filter(|id| !found.contains_key(id))
        .collect();
    missing.sort_unstable();
    missing.dedup();
    if missing.is_empty() {
        return Ok(found);
    }
    let fresh = match esi.names(&missing, priority).await {
        Ok(fresh) => fresh,
        // One unknown id makes ESI reject the whole batch.
        Err(EsiError::Status(404)) if missing.len() > 1 => {
            let mut some = Vec::new();
            for id in &missing {
                match esi.names(&[*id], priority).await {
                    Ok(one) => some.extend(one),
                    Err(EsiError::Status(404)) => {}
                    Err(err) => return Err(err.into()),
                }
            }
            some
        }
        Err(EsiError::Status(404)) => Vec::new(),
        Err(err) => return Err(err.into()),
    };
    for entity in &fresh {
        sqlx::query!(
            r#"
            INSERT INTO core.entity_names (id, name, category) VALUES ($1, $2, $3)
            ON CONFLICT (id) DO UPDATE
            SET name = EXCLUDED.name, category = EXCLUDED.category, fetched_at = now()
            "#,
            entity.id,
            entity.name,
            entity.category,
        )
        .execute(db)
        .await?;
    }
    found.extend(fresh.into_iter().map(|e| (e.id, e)));
    Ok(found)
}

/// Tickers are refetched after this.
pub const TICKER_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);

/// A corporation's or alliance's ticker, cached for a day.
pub async fn ticker(
    db: &PgPool,
    esi: &Esi,
    id: i64,
    kind: tether_core::states::EntityKind,
    priority: Priority,
) -> Result<String, NamesError> {
    let cached = sqlx::query_scalar!(
        r#"
        SELECT ticker FROM core.entity_tickers
        WHERE id = $1 AND fetched_at > now() - make_interval(secs => $2)
        "#,
        id,
        TICKER_MAX_AGE.as_secs_f64(),
    )
    .fetch_optional(db)
    .await?;
    if let Some(ticker) = cached {
        return Ok(ticker);
    }
    let fetched = match kind {
        tether_core::states::EntityKind::Corporation => esi.corporation_ticker(id, priority).await,
        tether_core::states::EntityKind::Alliance => esi.alliance_ticker(id, priority).await,
        tether_core::states::EntityKind::Character => {
            return Err(EsiError::InvalidInput("characters have no ticker".to_owned()).into());
        }
    };
    let ticker = match fetched {
        Ok(ticker) => ticker,
        // ESI is down (daily downtime, say): an old ticker beats none.
        Err(err) => {
            let stale =
                sqlx::query_scalar!("SELECT ticker FROM core.entity_tickers WHERE id = $1", id)
                    .fetch_optional(db)
                    .await?;
            return stale.ok_or(NamesError::Esi(err));
        }
    };
    sqlx::query!(
        r#"
        INSERT INTO core.entity_tickers (id, ticker) VALUES ($1, $2)
        ON CONFLICT (id) DO UPDATE SET ticker = EXCLUDED.ticker, fetched_at = now()
        "#,
        id,
        ticker,
    )
    .execute(db)
    .await?;
    Ok(ticker)
}
