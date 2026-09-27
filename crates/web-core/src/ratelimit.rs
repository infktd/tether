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

/// The client's IP, `None` if the connection's peer is unknown (only in
/// tests that skip `ConnectInfo`).
///
/// A deployment is always behind a reverse proxy (bundled Caddy, the
/// admin's nginx or Traefik, or their own), and the app's port is reachable
/// only from it: Caddy's Docker network, a Traefik network, or 127.0.0.1
/// on the host, which Docker forwards from a private bridge address. So
/// X-Forwarded-For is trusted only when the peer is a loopback or private
/// address (the proxy), and then only its last entry, the one the nearest
/// proxy added from the connection it saw: entries a client sent come
/// before it. A connection from a public address is a client talking to
/// the app directly, and its own headers mean nothing.
pub fn client_ip(parts: &Parts) -> Option<IpAddr> {
    let peer = parts
        .extensions
        .get::<ConnectInfo<SocketAddr>>()
        .map(|info| info.0.ip().to_canonical())?;
    if !is_proxy_address(peer) {
        return Some(peer);
    }
    // Bytes, not text: a proxy that appends to what the client sent keeps
    // the client's bytes in the same value, and a non-ASCII one must not
    // hide the entry the proxy added after it.
    let forwarded = parts
        .headers
        .get_all("x-forwarded-for")
        .iter()
        .next_back()
        .and_then(|value| value.as_bytes().split(|b| *b == b',').next_back())
        .and_then(|entry| std::str::from_utf8(entry).ok())
        .and_then(|entry| entry.trim().parse::<IpAddr>().ok());
    Some(forwarded.map_or(peer, |ip| ip.to_canonical()))
}

/// Addresses a reverse proxy in front of the app connects from: loopback
/// and private ranges (RFC 1918, IPv6 unique local), which Docker's
/// networks use. Never a public address.
fn is_proxy_address(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_loopback() || v4.is_private(),
        IpAddr::V6(v6) => v6.is_loopback() || v6.is_unique_local(),
    }
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

    fn parts(peer: &str, forwarded: &[&str]) -> Parts {
        let mut request = axum::http::Request::builder();
        for value in forwarded {
            request = request.header("x-forwarded-for", *value);
        }
        let (mut parts, ()) = request.body(()).unwrap().into_parts();
        let peer: SocketAddr = peer.parse().unwrap();
        parts.extensions.insert(ConnectInfo(peer));
        parts
    }

    fn ip(s: &str) -> Option<IpAddr> {
        Some(s.parse().unwrap())
    }

    #[test]
    fn behind_a_proxy_uses_the_last_forwarded_for_entry() {
        // Caddy or Traefik on a Docker network, nginx through a port
        // published on 127.0.0.1 (Docker's bridge gateway), a local proxy.
        for proxy in [
            "172.18.0.3:40000",
            "10.0.0.2:40000",
            "192.168.1.5:40000",
            "127.0.0.1:40000",
            "[::1]:40000",
            "[fd00::3]:40000",
            "[::ffff:172.18.0.1]:40000",
        ] {
            assert_eq!(
                client_ip(&parts(proxy, &["1.2.3.4, 5.6.7.8"])),
                ip("5.6.7.8"),
                "{proxy}"
            );
        }
        // Repeated headers: the last one's last entry.
        assert_eq!(
            client_ip(&parts("172.18.0.3:1", &["1.2.3.4", "9.9.9.9, 2001:db8::1"])),
            ip("2001:db8::1")
        );
        // No header: the proxy itself.
        assert_eq!(client_ip(&parts("172.18.0.3:1", &[])), ip("172.18.0.3"));
    }

    #[test]
    fn a_client_cannot_choose_its_address() {
        // Talking to the app directly from a public address: the header is
        // the client's own, so it is ignored.
        assert_eq!(
            client_ip(&parts("203.0.113.9:5000", &["5.6.7.8"])),
            ip("203.0.113.9")
        );
        assert_eq!(
            client_ip(&parts("[2001:db8::9]:5000", &["5.6.7.8"])),
            ip("2001:db8::9")
        );
        // Through a proxy, only the entry the proxy added counts: an
        // unparseable one doesn't fall back to what the client sent.
        assert_eq!(
            client_ip(&parts("172.18.0.3:1", &["5.6.7.8, unknown"])),
            ip("172.18.0.3")
        );
        // Nor can bytes that aren't text, sent ahead of it, hide it.
        let mut merged = parts("172.18.0.3:1", &[]);
        merged.headers.insert(
            "x-forwarded-for",
            axum::http::HeaderValue::from_bytes(b"\xff\xfe, 5.6.7.8").unwrap(),
        );
        assert_eq!(client_ip(&merged), ip("5.6.7.8"));
        // Unknown peer (tests without ConnectInfo): unknown client.
        let (bare, ()) = axum::http::Request::builder()
            .header("x-forwarded-for", "5.6.7.8")
            .body(())
            .unwrap()
            .into_parts();
        assert_eq!(client_ip(&bare), None);
    }
}
