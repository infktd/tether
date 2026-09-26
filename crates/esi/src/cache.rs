//! eve-esi-client's response cache, kept in Postgres (`core.esi_cache`):
//! shared by the whole process and kept across restarts, so a restart
//! doesn't re-request every route before its Expires.
//!
//! Entries are keyed by URL and principal (the `sub` of the bearer token
//! the library saw on the request). The library never caches a request
//! whose token it can't read, but a token sent as a `reqwest::Client`
//! default header is invisible to it: requests carrying a character's
//! token must use a client without a cache (`EsiInner::without_cache`),
//! as `Esi`'s token-bearing calls do. As a second line, a response ESI
//! marks `private` or `no-store` is never stored without a principal.
//!
//! A cache must never fail a request: database errors are logged and
//! treated as a miss (or a skipped write).

use std::time::SystemTime;

use eve_esi_client::cache::{
    Bytes, CacheKey, CachedResponse, EsiCache, HeaderMap, HeaderName, HeaderValue, StatusCode,
};
use tether_db::PgPool;
use tether_db::esi_cache::{self, Entry};

/// Bodies larger than this aren't stored (an ESI page is at most a few
/// hundred KiB).
pub const MAX_BODY: usize = 1024 * 1024;

/// Entries are pruned this long after they expired (or, never fresh, were
/// last stored or revalidated): a stale entry is only worth keeping while
/// its ETag may still save a download.
pub const PRUNE_AFTER: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);

/// At most this many entries are kept, the most recently stored first.
/// Together with [`MAX_BODY`] that bounds the table; the host's own
/// public requests (statuses, tickers, birthdays) fill far fewer.
pub const MAX_ENTRIES: i64 = 20_000;

/// Removes what [`PRUNE_AFTER`] and [`MAX_ENTRIES`] say to (the hourly
/// `maintenance.prune` job). Returns how many entries went.
pub async fn prune(db: &PgPool) -> Result<u64, sqlx::Error> {
    esi_cache::prune(db, PRUNE_AFTER.as_secs_f64(), MAX_ENTRIES).await
}

/// The Postgres-backed [`EsiCache`].
#[derive(Debug, Clone)]
pub struct PgCache {
    db: PgPool,
}

impl PgCache {
    pub fn new(db: PgPool) -> Self {
        Self { db }
    }
}

/// `''` stands for "no token" in the table's key.
fn principal(key: &CacheKey) -> &str {
    key.principal.as_deref().unwrap_or("")
}

/// Whether ESI said the response is only for the requester, or not to be
/// stored at all.
fn private(headers: &HeaderMap) -> bool {
    headers
        .get_all("cache-control")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(|d| d.trim().to_ascii_lowercase())
        .any(|d| d == "private" || d == "no-store" || d.starts_with("private="))
}

fn to_entry(response: &CachedResponse) -> Entry {
    let (header_names, header_values) = response
        .headers
        .iter()
        .map(|(name, value)| (name.as_str().to_owned(), value.as_bytes().to_vec()))
        .unzip();
    Entry {
        status: i16::try_from(response.status.as_u16()).unwrap_or(i16::MAX),
        header_names,
        header_values,
        body: response.body.to_vec(),
        etag: response
            .etag
            .as_ref()
            .and_then(|e| e.to_str().ok())
            .map(str::to_owned),
        expires_at: response.expires_at.map(chrono::DateTime::from),
    }
}

/// The stored entry, or `None` if any part of it no longer parses (then
/// it's refetched and overwritten).
fn from_entry(entry: Entry) -> Option<CachedResponse> {
    if entry.header_names.len() != entry.header_values.len() {
        return None;
    }
    let mut headers = HeaderMap::new();
    for (name, value) in entry.header_names.iter().zip(&entry.header_values) {
        // `append`: a header may repeat.
        headers.append(
            HeaderName::from_bytes(name.as_bytes()).ok()?,
            HeaderValue::from_bytes(value).ok()?,
        );
    }
    Some(CachedResponse {
        status: StatusCode::from_u16(u16::try_from(entry.status).ok()?).ok()?,
        headers,
        body: Bytes::from(entry.body),
        etag: entry
            .etag
            .map(|e| HeaderValue::from_str(&e))
            .transpose()
            .ok()?,
        expires_at: entry.expires_at.map(SystemTime::from),
    })
}

#[eve_esi_client::async_trait]
impl EsiCache for PgCache {
    async fn get(&self, key: &CacheKey) -> Option<CachedResponse> {
        // `Some("")` would read the unauthenticated entry.
        if key.principal.as_deref() == Some("") {
            return None;
        }
        match esi_cache::get(&self.db, &key.url, principal(key)).await {
            Ok(entry) => entry.and_then(from_entry),
            Err(err) => {
                tracing::warn!(error = %err, "reading the ESI cache failed; asking ESI");
                None
            }
        }
    }

    async fn put(&self, key: &CacheKey, response: CachedResponse) {
        // `Some("")` would share the unauthenticated key.
        if key.principal.as_deref() == Some("") {
            return;
        }
        if key.principal.is_none() && private(&response.headers) {
            return;
        }
        if response.body.len() > MAX_BODY {
            return;
        }
        if let Err(err) =
            esi_cache::put(&self.db, &key.url, principal(key), &to_entry(&response)).await
        {
            tracing::warn!(error = %err, "writing the ESI cache failed");
        }
    }

    async fn remove(&self, key: &CacheKey) {
        if let Err(err) = esi_cache::remove(&self.db, &key.url, principal(key)).await {
            tracing::warn!(error = %err, "removing from the ESI cache failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_and_no_store_responses_are_recognised() {
        let with = |value: &'static str| {
            let mut h = HeaderMap::new();
            h.insert("cache-control", HeaderValue::from_static(value));
            h
        };
        assert!(private(&with("private")));
        assert!(private(&with("max-age=300, Private")));
        assert!(private(&with("no-store")));
        assert!(!private(&with("public, max-age=300")));
        assert!(!private(&HeaderMap::new()));
    }

    #[test]
    fn entries_round_trip() {
        let mut headers = HeaderMap::new();
        headers.insert("content-type", HeaderValue::from_static("application/json"));
        headers.append("x-thing", HeaderValue::from_static("a"));
        headers.append("x-thing", HeaderValue::from_static("b"));
        let expires_at = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_900_000_000);
        let response = CachedResponse {
            status: StatusCode::OK,
            headers,
            body: Bytes::from_static(b"[1,2]"),
            etag: Some(HeaderValue::from_static("\"abc\"")),
            expires_at: Some(expires_at),
        };
        let back = from_entry(to_entry(&response)).unwrap();
        assert_eq!(back.status, StatusCode::OK);
        assert_eq!(back.headers, response.headers);
        assert_eq!(back.body, response.body);
        assert_eq!(back.etag, response.etag);
        assert_eq!(back.expires_at, Some(expires_at));
    }
}
