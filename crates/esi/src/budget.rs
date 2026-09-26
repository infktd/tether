//! Where Tether stands with ESI's limits, for the dashboard and for
//! holding bulk work back.
//!
//! ESI blocks clients that burn its error budget (`X-ESI-Error-Limit-*`,
//! HTTP 420) or overdraw a rate-limit group (`X-Ratelimit-*`, HTTP 429).
//! eve-esi-client already slows down before either, and keeps the current
//! budgets itself (`error_budget()`, `rate_budgets()`): they're read from
//! there, never from response headers, since a response answered from the
//! cache carries none. Tether adds counts of what came back, the lowest
//! error budget seen, and [`bulk_delay`] so bulk work backs off earlier
//! than interactive requests.

use std::sync::Mutex;
use std::time::Duration;

use eve_esi_client::{ErrorBudget, RateBudget};
use reqwest::StatusCode;
use reqwest::header::HeaderMap;
use serde::Serialize;

/// Bulk work pauses when fewer errors than this remain in the window,
/// leaving the rest for interactive requests (eve-esi-client itself stops
/// at 10).
pub const BULK_ERROR_RESERVE: u32 = 50;

/// How long bulk work should wait before its next request, if at all:
/// until the error window resets (and a second more), while fewer than
/// [`BULK_ERROR_RESERVE`] errors remain.
pub fn bulk_delay(error: Option<ErrorBudget>) -> Option<Duration> {
    error
        .filter(|e| e.remain < BULK_ERROR_RESERVE)
        // ESI's error window is a minute: a garbled reset value mustn't
        // park the bulk permits for longer.
        .map(|e| e.resets_in.min(Duration::from_secs(60)) + Duration::from_secs(1))
}

#[derive(Debug, Default)]
pub struct Budget {
    state: Mutex<State>,
}

#[derive(Debug, Default)]
struct State {
    lowest_error_remain: Option<u32>,
    counts: Counts,
}

/// What came back, since startup.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct Counts {
    /// Fresh successful responses from ESI.
    pub ok: u64,
    /// Answered from the cache without asking ESI.
    pub cached: u64,
    /// Revalidated: ESI answered 304 and the cached body was used.
    pub not_modified: u64,
    pub client_errors: u64,
    pub server_errors: u64,
    /// HTTP 420: the error budget ran out. Must stay zero.
    pub error_limited: u64,
    /// HTTP 429: a rate-limit group ran out.
    pub rate_limited: u64,
    /// No response at all (DNS, TLS, timeouts).
    pub transport_errors: u64,
}

/// A point-in-time view for the dashboard.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct BudgetSnapshot {
    /// Errors ESI will still accept in the current window.
    pub error_remain: Option<u32>,
    pub error_reset_in_secs: Option<u64>,
    /// The lowest `error_remain` seen since startup.
    pub lowest_error_remain: Option<u32>,
    pub groups: Vec<GroupSnapshot>,
    pub counts: Counts,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct GroupSnapshot {
    pub name: String,
    /// As ESI states it, e.g. `600/15m`.
    pub limit: String,
    /// Tokens a new request could use now (eve-esi-client's estimate).
    pub remaining: u32,
    /// Set after a 429: how much longer the group is held.
    pub held_for_secs: Option<u64>,
}

/// `600/15m`: ESI's own notation for a group's limit.
fn limit(max_tokens: u32, window: Duration) -> String {
    let secs = window.as_secs();
    let window = match secs {
        s if s > 0 && s % 3600 == 0 => format!("{}h", s / 3600),
        s if s > 0 && s % 60 == 0 => format!("{}m", s / 60),
        s => format!("{s}s"),
    };
    format!("{max_tokens}/{window}")
}

impl Budget {
    /// Counts one response. `error` is the client's error budget after it
    /// (eve-esi-client has recorded the response's headers by then).
    ///
    /// Responses eve-esi-client answered from its cache are marked
    /// (`x-esi-client-cache`): a hit is counted as such and nothing else;
    /// a revalidation (ESI said 304) as not modified.
    pub fn observe(&self, status: StatusCode, headers: &HeaderMap, error: Option<ErrorBudget>) {
        let marker = headers
            .get(eve_esi_client::cache::CACHE_STATUS_HEADER)
            .and_then(|v| v.to_str().ok());
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        let counts = &mut state.counts;
        match (marker, status.as_u16()) {
            (Some("hit"), _) => {
                counts.cached += 1;
                return;
            }
            (Some("revalidated"), _) | (_, 304) => counts.not_modified += 1,
            (_, 420) => counts.error_limited += 1,
            (_, 429) => counts.rate_limited += 1,
            (_, s) if (200..300).contains(&s) => counts.ok += 1,
            (_, s) if (400..500).contains(&s) => counts.client_errors += 1,
            _ => counts.server_errors += 1,
        }
        if let Some(error) = error {
            state.lowest_error_remain = Some(
                state
                    .lowest_error_remain
                    .map_or(error.remain, |l| l.min(error.remain)),
            );
        }
    }

    pub fn observe_transport_error(&self) {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        state.counts.transport_errors += 1;
    }

    /// The dashboard's view: the client's current budgets, and Tether's
    /// counts.
    pub fn snapshot(&self, error: Option<ErrorBudget>, rates: Vec<RateBudget>) -> BudgetSnapshot {
        let state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        BudgetSnapshot {
            error_remain: error.map(|e| e.remain),
            error_reset_in_secs: error.map(|e| e.resets_in.as_secs()),
            lowest_error_remain: state.lowest_error_remain,
            groups: rates
                .into_iter()
                .map(|g| GroupSnapshot {
                    limit: limit(g.max_tokens, g.window),
                    name: g.group,
                    remaining: g.remaining_estimate,
                    held_for_secs: g.blocked_for.map(|d| d.as_secs()),
                })
                .collect(),
            counts: state.counts.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::HeaderValue;

    fn marked(marker: &'static str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(
            eve_esi_client::cache::CACHE_STATUS_HEADER,
            HeaderValue::from_static(marker),
        );
        h
    }

    #[test]
    fn counts_what_came_back_and_cache_hits_apart() {
        let budget = Budget::default();
        let none = HeaderMap::new();
        budget.observe(StatusCode::OK, &none, None);
        budget.observe(StatusCode::OK, &marked("hit"), None);
        budget.observe(StatusCode::OK, &marked("revalidated"), None);
        budget.observe(StatusCode::NOT_FOUND, &none, None);
        budget.observe(StatusCode::from_u16(420).unwrap(), &none, None);
        budget.observe(StatusCode::TOO_MANY_REQUESTS, &none, None);
        budget.observe(StatusCode::BAD_GATEWAY, &none, None);
        budget.observe_transport_error();

        let s = budget.snapshot(None, Vec::new());
        assert_eq!(
            s.counts,
            Counts {
                ok: 1,
                cached: 1,
                not_modified: 1,
                client_errors: 1,
                server_errors: 1,
                error_limited: 1,
                rate_limited: 1,
                transport_errors: 1,
            }
        );
        assert_eq!(s.error_remain, None);
        assert_eq!(s.lowest_error_remain, None);
    }

    #[test]
    fn limits_read_like_esi_writes_them() {
        assert_eq!(limit(600, Duration::from_secs(900)), "600/15m");
        assert_eq!(limit(150, Duration::from_secs(3600)), "150/1h");
        assert_eq!(limit(10, Duration::from_secs(45)), "10/45s");
    }
}
