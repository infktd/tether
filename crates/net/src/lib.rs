//! Outbound HTTP (N5): the one way Tether's own code talks to the internet.
//!
//! [`Outbound`] only reaches hosts (and ports) on the [`Allowlist`]. A
//! request anywhere else fails before a connection is made, and its DNS
//! resolver won't even look other names up. Redirects aren't followed
//! unless a client opts in, and then only to allowed hosts. Requests come
//! back wrapped ([`Request`]), so the underlying client can't be pulled out
//! and pointed elsewhere. Proxy settings from the environment are ignored,
//! no Referer is sent, so traffic goes where the list says and nothing
//! more leaves with it.
//!
//! Two libraries make their own connections and can't take this client:
//! eve-esi-client (ESI and EVE SSO, endpoints fixed in the library) and
//! twilight (Discord, whose endpoint is checked against this list where it
//! is set up). `doctor` checks the configured endpoints.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use reqwest::{Method, RequestBuilder, Url};

pub use reqwest::Response;

/// Every host Tether's server itself may contact, and why. Browsers also
/// load images from `images.evetech.net`, and Caddy talks to Let's Encrypt;
/// neither goes through this code.
pub const ALLOWED: &[(&str, &str)] = &[
    ("esi.evetech.net", "ESI"),
    ("login.eveonline.com", "EVE SSO"),
    ("discord.com", "Discord API"),
    ("api.github.com", "update checks and plugin installs"),
    ("github.com", "plugin downloads"),
    ("objects.githubusercontent.com", "plugin release assets"),
];

#[derive(Debug, thiserror::Error)]
pub enum OutboundError {
    #[error("{0} isn't an allowed destination")]
    Blocked(String),
    #[error("not a valid URL: {0}")]
    BadUrl(String),
    #[error("building the HTTP client: {0}")]
    Build(String),
}

/// Hosts that may be contacted, as exact `host:port` pairs: HTTPS (port
/// 443 unless the operator's own URL says otherwise), plus plain-HTTP
/// stand-ins for tests.
#[derive(Debug, Clone)]
pub struct Allowlist {
    https: BTreeSet<String>,
    /// `host:port` pairs reachable over plain HTTP (tests only).
    http: BTreeSet<String>,
}

impl Allowlist {
    /// The hosts in [`ALLOWED`], on port 443.
    pub fn production() -> Self {
        Self {
            https: ALLOWED.iter().map(|(h, _)| format!("{h}:443")).collect(),
            http: BTreeSet::new(),
        }
    }

    /// Adds a host reachable over HTTPS on port 443: the instance's own
    /// domain, which the setup wizard and `doctor` check answers.
    pub fn with_host(self, host: &str) -> Self {
        self.with_https(host, 443)
    }

    fn with_https(mut self, host: &str, port: u16) -> Self {
        self.https
            .insert(format!("{}:{port}", host.to_ascii_lowercase()));
        self
    }

    /// Adds the host of an address the operator configured (the instance's
    /// own public URL): HTTPS by host, or plain HTTP by exact host and
    /// port (a local instance). Unparseable input adds nothing.
    pub fn with_url(self, url: &str) -> Self {
        let Ok(parsed) = Url::parse(url) else {
            return self;
        };
        let Some(host) = parsed.host_str().map(str::to_owned) else {
            return self;
        };
        match parsed.scheme() {
            "https" => {
                let port = parsed.port_or_known_default().unwrap_or(443);
                self.with_https(&host, port)
            }
            "http" => {
                let port = parsed.port_or_known_default().unwrap_or(80);
                self.with_local(&format!("{host}:{port}"))
            }
            _ => self,
        }
    }

    /// Adds a plain-HTTP `host:port` stand-in, for tests.
    pub fn with_local(mut self, host_port: &str) -> Self {
        self.http.insert(host_port.to_ascii_lowercase());
        self
    }

    pub fn allows(&self, url: &Url) -> bool {
        let Some(host) = url.host_str().map(str::to_ascii_lowercase) else {
            return false;
        };
        let Some(port) = url.port_or_known_default() else {
            return false;
        };
        let pair = format!("{host}:{port}");
        match url.scheme() {
            "https" => self.https.contains(&pair),
            "http" => self.http.contains(&pair),
            _ => false,
        }
    }

    /// Whether a name may be looked up at all.
    fn resolvable(&self, name: &str) -> bool {
        let name = name.to_ascii_lowercase();
        self.https
            .iter()
            .chain(&self.http)
            .any(|hp| hp.rsplit_once(':').is_some_and(|(h, _)| h == name))
    }

    /// The HTTPS hosts, without ports.
    pub fn hosts(&self) -> impl Iterator<Item = &str> {
        self.https
            .iter()
            .filter_map(|hp| hp.rsplit_once(':').map(|(h, _)| h))
    }

    /// Checks a URL string, for endpoints set up outside this client.
    pub fn check(&self, url: &str) -> Result<Url, OutboundError> {
        let url = Url::parse(url).map_err(|_| OutboundError::BadUrl(url.to_owned()))?;
        if self.allows(&url) {
            Ok(url)
        } else {
            Err(OutboundError::Blocked(describe(&url)))
        }
    }
}

fn describe(url: &Url) -> String {
    format!(
        "{}://{}",
        url.scheme(),
        url.host_str().unwrap_or("(no host)")
    )
}

/// Refuses to look up names that aren't on the list.
struct AllowResolver(Arc<Allowlist>);

impl Resolve for AllowResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let allow = self.0.clone();
        Box::pin(async move {
            let host = name.as_str().to_owned();
            if !allow.resolvable(&host) {
                return Err(format!("{host} isn't an allowed destination").into());
            }
            let addrs = tokio::net::lookup_host((host.as_str(), 0)).await?;
            Ok(Box::new(addrs.collect::<Vec<_>>().into_iter()) as Addrs)
        })
    }
}

/// Whether a client follows redirects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Redirects {
    /// A redirect is returned as the response. The default: a 307 would
    /// re-send a POST body (tokens, codes) to wherever it points.
    Refuse,
    /// Followed only to allowed hosts, at most five times. For downloads
    /// that hop between GitHub's hosts; never for requests carrying secrets.
    WithinAllowlist,
}

/// An HTTP client that only reaches allowed hosts.
#[derive(Clone)]
pub struct Outbound {
    client: reqwest::Client,
    allow: Arc<Allowlist>,
}

impl std::fmt::Debug for Outbound {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Outbound")
            .field("allow", &self.allow)
            .finish_non_exhaustive()
    }
}

impl Outbound {
    /// A client that doesn't follow redirects.
    pub fn new(
        allow: Allowlist,
        user_agent: &str,
        timeout: Duration,
    ) -> Result<Self, OutboundError> {
        Self::with_redirects(allow, user_agent, timeout, Redirects::Refuse)
    }

    pub fn with_redirects(
        allow: Allowlist,
        user_agent: &str,
        timeout: Duration,
        redirects: Redirects,
    ) -> Result<Self, OutboundError> {
        let allow = Arc::new(allow);
        let policy = match redirects {
            Redirects::Refuse => reqwest::redirect::Policy::none(),
            Redirects::WithinAllowlist => {
                let for_redirects = allow.clone();
                reqwest::redirect::Policy::custom(move |attempt| {
                    if attempt.previous().len() >= 5 {
                        attempt.error("too many redirects")
                    } else if for_redirects.allows(attempt.url()) {
                        attempt.follow()
                    } else {
                        let to = describe(attempt.url());
                        attempt.error(format!(
                            "redirect to {to} blocked: not an allowed destination"
                        ))
                    }
                })
            }
        };
        let client = reqwest::Client::builder()
            .user_agent(user_agent)
            .timeout(timeout)
            .no_proxy()
            .referer(false)
            .dns_resolver(Arc::new(AllowResolver(allow.clone())))
            .redirect(policy)
            .build()
            .map_err(|err| OutboundError::Build(err.without_url().to_string()))?;
        Ok(Self { client, allow })
    }

    pub fn allowlist(&self) -> &Allowlist {
        &self.allow
    }

    /// A request, if `url` is allowed.
    pub fn request(&self, method: Method, url: &str) -> Result<Request, OutboundError> {
        let url = self.allow.check(url)?;
        Ok(Request(self.client.request(method, url)))
    }

    pub fn get(&self, url: &str) -> Result<Request, OutboundError> {
        self.request(Method::GET, url)
    }

    pub fn post(&self, url: &str) -> Result<Request, OutboundError> {
        self.request(Method::POST, url)
    }
}

/// A request to an allowed URL. Only what callers need is exposed: the
/// URL and client can't be changed once checked.
#[must_use]
pub struct Request(RequestBuilder);

impl Request {
    pub fn header(self, name: &str, value: &str) -> Self {
        Self(self.0.header(name, value))
    }

    pub fn basic_auth(self, user: impl std::fmt::Display, password: Option<&str>) -> Self {
        Self(self.0.basic_auth(user, password))
    }

    pub fn form<T: serde::Serialize + ?Sized>(self, form: &T) -> Self {
        Self(self.0.form(form))
    }

    pub async fn send(self) -> Result<reqwest::Response, reqwest::Error> {
        self.0.send().await
    }
}

/// Proxy variables in the environment. [`Outbound`] ignores them, but the
/// libraries with their own clients may not, so `doctor` warns.
pub fn proxy_variables() -> Vec<String> {
    [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
    ]
    .iter()
    .filter(|name| std::env::var_os(name).is_some_and(|v| !v.is_empty()))
    .map(|name| (*name).to_owned())
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_listed_hosts_over_https() {
        let allow = Allowlist::production();
        for ok in [
            "https://esi.evetech.net/status",
            "https://login.eveonline.com/oauth/jwks",
            "https://discord.com/api/v10/oauth2/token",
            "https://API.GITHUB.COM/repos/x/y",
        ] {
            assert!(allow.check(ok).is_ok(), "{ok}");
        }
        for blocked in [
            "http://esi.evetech.net/status",
            "https://api.github.com:4444/",
            "https://ESI.EVETECH.NET./status",
            "https://esi.evetech.net@evil.example/",
            "https://example.com/",
            "https://esi.evetech.net.evil.example/",
            "https://evil.example/?u=https://esi.evetech.net",
            "https://10.0.0.1/",
            "https://169.254.169.254/latest/meta-data",
            "ftp://esi.evetech.net/",
            "file:///etc/passwd",
        ] {
            assert!(
                matches!(
                    allow.check(blocked),
                    Err(OutboundError::Blocked(_) | OutboundError::BadUrl(_))
                ),
                "{blocked}"
            );
        }
    }

    #[test]
    fn local_stand_ins_are_host_and_port_exact() {
        let allow = Allowlist::production().with_local("127.0.0.1:4000");
        assert!(allow.check("http://127.0.0.1:4000/x").is_ok());
        assert!(allow.check("http://127.0.0.1:4001/x").is_err());
        assert!(allow.check("https://127.0.0.1:4000/x").is_err());
        let own = Allowlist::production().with_host("auth.example.com");
        assert!(own.check("https://auth.example.com/health").is_ok());
        let local = Allowlist::production().with_url("http://localhost:18080");
        assert!(
            local
                .check("http://localhost:18080/api/setup/probe")
                .is_ok()
        );
        assert!(local.check("http://localhost:18081/").is_err());
        let public = Allowlist::production().with_url("https://auth.example.com/");
        assert!(public.check("https://auth.example.com/x").is_ok());
        assert!(public.check("https://auth.example.com:8443/x").is_err());
        let odd_port = Allowlist::production().with_url("https://auth.example.com:8443");
        assert!(odd_port.check("https://auth.example.com:8443/x").is_ok());
        assert!(odd_port.check("https://auth.example.com/x").is_err());
    }
}
