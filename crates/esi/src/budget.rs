//! What ESI says about our limits, from the headers on every response.
//!
//! ESI blocks clients that burn its error budget (`X-ESI-Error-Limit-*`,
//! HTTP 420) or overdraw a rate-limit group (`X-Ratelimit-*`, HTTP 429).
//! eve-esi-client already slows down before either; this records the state
//! so the dashboard can show it and bulk work can back off earlier than
//! interactive requests.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use reqwest::StatusCode;
use reqwest::header::HeaderMap;
use serde::Serialize;

/// Bulk work pauses when fewer errors than this remain in the window,
/// leaving the rest for interactive requests (eve-esi-client itself stops
/// at 10).
pub const BULK_ERROR_RESERVE: u32 = 50;

#[derive(Debug, Default)]
pub struct Budget {
    state: Mutex<State>,
}

#[derive(Debug, Default)]
struct State {
    error_remain: Option<u32>,
    error_reset_at: Option<SystemTime>,
    lowest_error_remain: Option<u32>,
    groups: BTreeMap<String, Group>,
    counts: Counts,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct Counts {
    pub ok: u64,
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

#[derive(Debug, Clone)]
struct Group {
    limit: String,
    remaining: Option<u64>,
    observed_at: SystemTime,
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
    pub remaining: Option<u64>,
    pub observed_secs_ago: u64,
}

fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|v| v.to_str().ok())
}

impl Budget {
    pub fn observe(&self, status: StatusCode, headers: &HeaderMap) {
        let now = SystemTime::now();
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        let counts = &mut state.counts;
        match status.as_u16() {
            304 => counts.not_modified += 1,
            420 => counts.error_limited += 1,
            429 => counts.rate_limited += 1,
            s if (200..300).contains(&s) => counts.ok += 1,
            s if (400..500).contains(&s) => counts.client_errors += 1,
            _ => counts.server_errors += 1,
        }
        if let Some(remain) =
            header(headers, "x-esi-error-limit-remain").and_then(|v| v.parse().ok())
        {
            state.error_remain = Some(remain);
            state.lowest_error_remain = Some(
                state
                    .lowest_error_remain
                    .map_or(remain, |l: u32| l.min(remain)),
            );
        }
        if let Some(reset) =
            header(headers, "x-esi-error-limit-reset").and_then(|v| v.parse::<u64>().ok())
        {
            state.error_reset_at = Some(now + Duration::from_secs(reset));
        }
        if let Some(group) = header(headers, "x-ratelimit-group") {
            let entry = Group {
                limit: header(headers, "x-ratelimit-limit")
                    .unwrap_or("")
                    .to_owned(),
                remaining: header(headers, "x-ratelimit-remaining").and_then(|v| v.parse().ok()),
                observed_at: now,
            };
            state.groups.insert(group.to_owned(), entry);
        }
    }

    pub fn observe_transport_error(&self) {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        state.counts.transport_errors += 1;
    }

    /// How long bulk work should wait before its next request, if at all.
    pub fn bulk_delay(&self, now: SystemTime) -> Option<Duration> {
        let state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        match (state.error_remain, state.error_reset_at) {
            (Some(remain), Some(reset_at)) if remain < BULK_ERROR_RESERVE => reset_at
                .duration_since(now)
                .ok()
                .map(|wait| wait + Duration::from_secs(1)),
            _ => None,
        }
    }

    pub fn snapshot(&self) -> BudgetSnapshot {
        let now = SystemTime::now();
        let state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        BudgetSnapshot {
            error_remain: state.error_remain,
            error_reset_in_secs: state
                .error_reset_at
                .and_then(|r| r.duration_since(now).ok())
                .map(|d| d.as_secs()),
            lowest_error_remain: state.lowest_error_remain,
            groups: state
                .groups
                .iter()
                .map(|(name, g)| GroupSnapshot {
                    name: name.clone(),
                    limit: g.limit.clone(),
                    remaining: g.remaining,
                    observed_secs_ago: now.duration_since(g.observed_at).map_or(0, |d| d.as_secs()),
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

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(*k, HeaderValue::from_str(v).unwrap());
        }
        h
    }

    #[test]
    fn records_error_and_rate_budgets_from_esi_headers() {
        let budget = Budget::default();
        budget.observe(
            StatusCode::OK,
            &headers(&[
                ("x-esi-error-limit-remain", "97"),
                ("x-esi-error-limit-reset", "40"),
                ("x-ratelimit-group", "status"),
                ("x-ratelimit-limit", "600/15m"),
                ("x-ratelimit-remaining", "598"),
            ]),
        );
        budget.observe(
            StatusCode::NOT_FOUND,
            &headers(&[("x-esi-error-limit-remain", "96")]),
        );
        budget.observe(StatusCode::from_u16(420).unwrap(), &headers(&[]));
        budget.observe(StatusCode::NOT_MODIFIED, &headers(&[]));
        budget.observe_transport_error();

        let s = budget.snapshot();
        assert_eq!(s.error_remain, Some(96));
        assert_eq!(s.lowest_error_remain, Some(96));
        assert!(matches!(s.error_reset_in_secs, Some(38..=40)));
        assert_eq!(s.groups[0].name, "status");
        assert_eq!(s.groups[0].limit, "600/15m");
        assert_eq!(s.groups[0].remaining, Some(598));
        assert_eq!(
            s.counts,
            Counts {
                ok: 1,
                not_modified: 1,
                client_errors: 1,
                error_limited: 1,
                transport_errors: 1,
                ..Counts::default()
            }
        );
    }

    #[test]
    fn bulk_waits_for_the_window_to_reset_when_the_budget_is_low() {
        let budget = Budget::default();
        let now = SystemTime::now();
        assert_eq!(budget.bulk_delay(now), None);

        budget.observe(
            StatusCode::OK,
            &headers(&[
                ("x-esi-error-limit-remain", "80"),
                ("x-esi-error-limit-reset", "30"),
            ]),
        );
        assert_eq!(budget.bulk_delay(now), None, "plenty left");

        budget.observe(
            StatusCode::OK,
            &headers(&[
                ("x-esi-error-limit-remain", "20"),
                ("x-esi-error-limit-reset", "30"),
            ]),
        );
        let wait = budget.bulk_delay(SystemTime::now()).unwrap();
        assert!((29..=31).contains(&wait.as_secs()), "{wait:?}");
        // After the reset time, no wait.
        assert_eq!(
            budget.bulk_delay(SystemTime::now() + Duration::from_secs(60)),
            None
        );
    }
}
