//! A small in-memory sliding-window rate limiter, keyed by client IP (or
//! anything else hashable).
//!
//! In-memory is enough: there is one host process, and limits only need to
//! slow down guessing, not survive restarts.

use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, SocketAddr};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::extract::ConnectInfo;
use axum::http::request::Parts;

#[derive(Debug)]
pub struct RateLimiter<K = IpAddr> {
    limit: usize,
    window: Duration,
    hits: Mutex<HashMap<K, VecDeque<Instant>>>,
}

impl<K: std::hash::Hash + Eq> RateLimiter<K> {
    pub fn new(limit: usize, window: Duration) -> Self {
        Self {
            limit,
            window,
            hits: Mutex::new(HashMap::new()),
        }
    }

    /// Records an attempt. `Err(retry_after)` when over the limit; rejected
    /// attempts are not counted.
    pub fn check(&self, key: K, now: Instant) -> Result<(), Duration> {
        let mut hits = self
            .hits
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // Forget idle clients so the map can't grow without bound.
        hits.retain(|_, times| {
            times
                .back()
                .is_some_and(|t| now.duration_since(*t) < self.window)
        });
        let times = hits.entry(key).or_default();
        while times
            .front()
            .is_some_and(|t| now.duration_since(*t) >= self.window)
        {
            times.pop_front();
        }
        if times.len() >= self.limit {
            let oldest = times.front().copied().unwrap_or(now);
            return Err(self.window.saturating_sub(now.duration_since(oldest)));
        }
        times.push_back(now);
        Ok(())
    }
}

/// The client's IP. Behind Caddy (the only way in for a deployment) that is
/// the last X-Forwarded-For entry, which Caddy sets from the connection and
/// doesn't take from the client. Otherwise the peer address.
pub fn client_ip(parts: &Parts) -> Option<IpAddr> {
    let forwarded = parts
        .headers
        .get_all("x-forwarded-for")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .filter_map(|ip| ip.trim().parse::<IpAddr>().ok())
        .next_back();
    forwarded.or_else(|| {
        parts
            .extensions
            .get::<ConnectInfo<SocketAddr>>()
            .map(|info| info.0.ip())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_up_to_the_limit_per_window() {
        let limiter = RateLimiter::new(5, Duration::from_secs(60));
        let (a, b): (IpAddr, IpAddr) = ("10.0.0.1".parse().unwrap(), "10.0.0.2".parse().unwrap());
        let t0 = Instant::now();
        for i in 0..5 {
            assert!(limiter.check(a, t0 + Duration::from_secs(i)).is_ok());
        }
        let retry = limiter.check(a, t0 + Duration::from_secs(10)).unwrap_err();
        assert_eq!(retry, Duration::from_secs(50));
        // Other clients are unaffected.
        assert!(limiter.check(b, t0 + Duration::from_secs(10)).is_ok());
        // The window slides.
        assert!(limiter.check(a, t0 + Duration::from_secs(61)).is_ok());
    }

    #[test]
    fn uses_the_last_forwarded_for_entry() {
        let (mut parts, ()) = axum::http::Request::builder()
            .header("x-forwarded-for", "1.2.3.4, 5.6.7.8")
            .body(())
            .unwrap()
            .into_parts();
        assert_eq!(client_ip(&parts), Some("5.6.7.8".parse().unwrap()));
        parts.headers.remove("x-forwarded-for");
        assert_eq!(client_ip(&parts), None);
    }
}
