//! Plugin HTTP: Tether's side of the plugin `http` interface.
//!
//! A plugin reaches only the hosts an admin approved for it at install
//! (`core.plugin_http_hosts`) that its running manifest still declares:
//! over HTTPS on port 443, through a `tether_net::Outbound` whose allow
//! list is exactly those hosts (checked before sending and at DNS). Never
//! Tether's own destinations: a package declaring one is refused.
//!
//! The host, not the plugin:
//! - sets the User-Agent;
//! - adds a secret the plugin names, in the header and with the prefix the
//!   admin approved, only on requests to that secret's host; the plugin
//!   never sees its value, and can't set `Authorization`, `Cookie` or any
//!   header outside a short list;
//! - follows redirects itself, only to approved hosts, at most
//!   [`MAX_REDIRECTS`] times;
//! - caps request and response sizes, times out, and rate-limits each
//!   plugin;
//! - records every request in the plugin's HTTP log: method, host and
//!   path. The query string is dropped: it's where APIs put keys and
//!   searches (names, ids), and the path is enough to see what was
//!   reached.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use reqwest::{StatusCode, Url};
use serde_json::json;
use tether_core::Secret;
use tether_db::accounts::AccountId;
use tether_db::audit::{self, Actor};
use tether_db::plugin_http as db;
use tether_db::secrets;
use tether_net::{Allowlist, Outbound};
use tether_plugins::manifest::Manifest;
use tether_plugins::services::{HttpError, HttpMethod, HttpRequest, HttpResponse};

use crate::error::AppError;
use crate::plugins::Plugins;
use crate::ratelimit::RateLimiter;

/// How long one request (one hop of a redirect) may take.
pub const TIMEOUT: Duration = Duration::from_secs(10);
/// The largest response body handed to a plugin.
pub const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
/// Requests per plugin per minute...
pub const PER_MINUTE: usize = 60;
/// ...and per day, counted apart for page renders (which anyone who may
/// open a page can cause) and for jobs and form posts, so page views
/// can't use up what the plugin's jobs need. Every attempt counts, refused
/// ones too, so a plugin can't push a request out of its log (kept by
/// count, [`LOG_KEEP`]) in less than about five days: at most twice this
/// many a day, each with at most [`MAX_REDIRECTS`] redirect rows.
pub const PER_DAY: usize = 5_000;
/// Redirects followed (to approved hosts only) for one request.
pub const MAX_REDIRECTS: usize = 3;
/// The longest URL a plugin may ask for.
pub const MAX_URL: usize = 2048;
/// Headers a plugin may set, at most [`MAX_HEADERS`], each value at most
/// [`MAX_HEADER_VALUE`] printable ASCII characters.
pub const REQUEST_HEADERS: &[&str] = &[
    "accept",
    "accept-language",
    "content-type",
    "if-none-match",
    "if-modified-since",
];
pub const MAX_HEADERS: usize = 10;
pub const MAX_HEADER_VALUE: usize = 256;
/// Headers handed back to a plugin; never `Set-Cookie`.
pub const RESPONSE_HEADERS: &[&str] = &[
    "content-type",
    "etag",
    "last-modified",
    "cache-control",
    "expires",
    "age",
    "retry-after",
];
/// The longest secret value an admin may enter.
pub const MAX_SECRET: usize = 4096;
/// Requests are kept this long in the HTTP log...
pub const LOG_DAYS: i32 = 90;
/// ...and at most this many per plugin.
pub const LOG_KEEP: i64 = 200_000;
/// Paths are cut to this many characters in the log.
pub const LOGGED_PATH: usize = 300;

/// Tether's own destinations and their domains, which plugins can't
/// declare: they reach ESI and Discord through the host API (which checks
/// scopes and shares the error budget), and GitHub is how Tether itself
/// updates.
pub const CORE_DOMAINS: &[&str] = &[
    "evetech.net",
    "eveonline.com",
    "discord.com",
    "discordapp.com",
    "discord.gg",
    "discordapp.net",
    "github.com",
    "githubusercontent.com",
];

/// Whether a host is (under) one of Tether's own destinations.
pub fn is_core_host(host: &str) -> bool {
    let host = host.to_ascii_lowercase();
    CORE_DOMAINS
        .iter()
        .any(|d| host == *d || host.ends_with(&format!(".{d}")))
        || tether_net::ALLOWED.iter().any(|(h, _)| host == *h)
}

/// The User-Agent sent for a plugin: the software, the app, and the
/// instance's public URL as a way to reach its operator (as APIs such as
/// zKillboard ask). Not the version, which would tell every approved host
/// how up to date this server is.
fn user_agent(plugin: &str, public_url: &str) -> String {
    let url: String = public_url
        .chars()
        .filter(|c| c.is_ascii_graphic())
        .take(200)
        .collect();
    if url.is_empty() {
        format!("tether (app {plugin})")
    } else {
        format!("tether (app {plugin}; +{url})")
    }
}

/// Per-plugin clients and the rate limit.
pub struct Http {
    public_url: String,
    limiter: RateLimiter<String>,
    daily: RateLimiter<String>,
    /// Each plugin's client, with the hosts it was built for.
    clients: Mutex<HashMap<String, (Vec<String>, Outbound)>>,
    /// Tests only: where every approved host is really served (a plain
    /// HTTP `host:port`). Compiled out of release builds.
    #[cfg(feature = "plugin-http-test")]
    test_route: Mutex<Option<String>>,
}

impl std::fmt::Debug for Http {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Http").finish_non_exhaustive()
    }
}

impl Http {
    /// `public_url` is the instance's, for the User-Agent.
    pub fn new(public_url: &str) -> Self {
        Self {
            public_url: public_url.to_owned(),
            limiter: RateLimiter::new(PER_MINUTE, Duration::from_secs(60)),
            daily: RateLimiter::new(PER_DAY, Duration::from_secs(24 * 60 * 60)),
            clients: Mutex::default(),
            #[cfg(feature = "plugin-http-test")]
            test_route: Mutex::default(),
        }
    }

    /// Tests only: serves every approved host from a plain-HTTP stand-in
    /// at `host_port`, telling it which host was meant in an
    /// `x-tether-test-host` header.
    #[cfg(feature = "plugin-http-test")]
    pub fn route_to(&self, host_port: &str) {
        *self.test_route.lock().unwrap_or_else(|p| p.into_inner()) = Some(host_port.to_owned());
        self.clients
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clear();
    }

    #[cfg(feature = "plugin-http-test")]
    fn route(&self) -> Option<String> {
        self.test_route
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    #[cfg(not(feature = "plugin-http-test"))]
    fn route(&self) -> Option<String> {
        None
    }

    /// The plugin's client, allowed exactly `hosts`.
    fn client(&self, plugin: &str, hosts: &[String]) -> Result<Outbound, HttpError> {
        let mut clients = self.clients.lock().unwrap_or_else(|p| p.into_inner());
        if let Some((built_for, client)) = clients.get(plugin)
            && built_for == hosts
        {
            return Ok(client.clone());
        }
        // Public addresses only: a publisher's DNS can't point a plugin
        // at this server's own network.
        let mut allow = Allowlist::only(hosts.iter().map(String::as_str));
        if let Some(route) = self.route() {
            allow = allow.with_local(&route);
        }
        let client =
            Outbound::new(allow, &user_agent(plugin, &self.public_url), TIMEOUT).map_err(|e| {
                tracing::error!(plugin, error = %e, "building a plugin's HTTP client");
                HttpError::Unavailable
            })?;
        clients.insert(plugin.to_owned(), (hosts.to_vec(), client.clone()));
        Ok(client)
    }
}

/// A short label for the log.
fn outcome(error: &HttpError) -> &'static str {
    match error {
        HttpError::NotAllowed(_) => "not allowed",
        HttpError::TooMany => "rate limited",
        HttpError::TooLarge => "too large",
        HttpError::Timeout => "timeout",
        HttpError::Unavailable => "unavailable",
    }
}

fn method_name(method: HttpMethod) -> &'static str {
    match method {
        HttpMethod::Get => "GET",
        HttpMethod::Post => "POST",
    }
}

/// A URL a plugin may be sent to: `https`, port 443, a hostname, no
/// credentials. Returns the lowercase host.
fn check_url(url: &Url) -> Result<String, HttpError> {
    let refuse = |why: &str| Err(HttpError::NotAllowed(why.to_owned()));
    if url.scheme() != "https" {
        return refuse("only https:// URLs");
    }
    if !url.username().is_empty() || url.password().is_some() {
        return refuse("no credentials in the URL: use a secret");
    }
    // `domain` is `None` for IP addresses.
    let Some(host) = url.domain() else {
        return refuse("the URL needs a hostname, not an address");
    };
    if url.port_or_known_default() != Some(443) {
        return refuse("only port 443");
    }
    Ok(host.to_ascii_lowercase())
}

/// A request header a plugin set, checked. `secret_headers` are the
/// headers this plugin's secrets go in (lowercase).
fn check_headers(
    headers: &[(String, String)],
    secret_headers: &[String],
) -> Result<Vec<(String, String)>, HttpError> {
    if headers.len() > MAX_HEADERS {
        return Err(HttpError::NotAllowed(format!(
            "at most {MAX_HEADERS} headers"
        )));
    }
    let mut checked: Vec<(String, String)> = Vec::new();
    for (name, value) in headers {
        let lower = name.to_ascii_lowercase();
        if secret_headers.contains(&lower) {
            return Err(HttpError::NotAllowed(format!(
                "{lower} carries one of this app's secrets: name the secret instead"
            )));
        }
        if !REQUEST_HEADERS.contains(&lower.as_str()) {
            let shown: String = lower.chars().take(64).collect();
            return Err(HttpError::NotAllowed(format!(
                "header {shown:?} isn't allowed; only {}",
                REQUEST_HEADERS.join(", ")
            )));
        }
        if checked.iter().any(|(n, _)| *n == lower) {
            return Err(HttpError::NotAllowed(format!("{lower} appears twice")));
        }
        if value.len() > MAX_HEADER_VALUE
            || !value.bytes().all(|b| b == b' ' || b.is_ascii_graphic())
        {
            return Err(HttpError::NotAllowed(format!(
                "{lower} must be at most {MAX_HEADER_VALUE} printable ASCII characters"
            )));
        }
        checked.push((lower, value.clone()));
    }
    Ok(checked)
}

/// The hosts a plugin may reach now: approved by an admin, and still
/// declared by the package that is running.
fn usable_hosts(approved: &db::Approved, manifest: &Manifest) -> Vec<String> {
    approved
        .hosts
        .iter()
        .filter(|h| manifest.capabilities.http.contains(h) && !is_core_host(h))
        .cloned()
        .collect()
}

/// A secret the plugin may name: approved, still declared with the same
/// host, header and prefix.
fn usable_secret<'a>(
    approved: &'a db::Approved,
    manifest: &Manifest,
    name: &str,
) -> Option<&'a db::SecretSpec> {
    let spec = approved.secrets.iter().find(|s| s.name == name)?;
    let declared = manifest.capabilities.secrets.get(name)?;
    (declared.host == spec.host
        && declared.header.eq_ignore_ascii_case(&spec.header)
        && declared.prefix == spec.prefix)
        .then_some(spec)
}

/// Logs one request. Refusals before a URL was read log no host.
struct Logger<'a> {
    db: &'a tether_db::PgPool,
    plugin: &'a str,
    method: &'static str,
    secret: Option<&'a str>,
}

impl Logger<'_> {
    async fn write(
        &self,
        url: Option<&Url>,
        status: Option<u16>,
        outcome: &str,
        bytes: u64,
        started: Instant,
    ) {
        let (host, path) = match url {
            Some(url) => (
                url.host_str().unwrap_or("").chars().take(253).collect(),
                tether_plugins::host::printable(url.path(), LOGGED_PATH),
            ),
            None => ("(not a URL)".to_owned(), String::new()),
        };
        let entry = db::Entry {
            plugin_id: self.plugin,
            method: self.method,
            host: &host,
            path: &path,
            status,
            outcome,
            secret: self.secret,
            bytes,
            duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        };
        if let Err(err) = db::log(self.db, &entry).await {
            tracing::error!(plugin = self.plugin, error = %err, "plugin HTTP log");
        }
    }
}

/// Sends one plugin request (`from_page`: during a page render). Every
/// outcome is logged, except attempts over the rate limits (which send
/// nothing). Run it where it can't be cancelled halfway (see
/// `PluginServices::http_send`): a request that left must reach the log,
/// even if the plugin's call ran out of time.
pub async fn send(
    deps: &crate::plugin_services::Deps,
    plugins: &std::sync::Weak<Plugins>,
    http: &Http,
    plugin: &str,
    request: HttpRequest,
    from_page: bool,
) -> Result<HttpResponse, HttpError> {
    let started = Instant::now();
    // Every attempt counts, before anything else: refusals are logged too,
    // and a plugin mustn't be able to fill its log with them.
    let now = Instant::now();
    if http.limiter.check(plugin.to_owned(), now).is_err()
        || http
            .daily
            .check(
                if from_page {
                    format!("{plugin} pages")
                } else {
                    plugin.to_owned()
                },
                now,
            )
            .is_err()
    {
        tracing::warn!(plugin, "plugin HTTP rate limit reached");
        return Err(HttpError::TooMany);
    }
    let log = Logger {
        db: &deps.db,
        plugin,
        method: method_name(request.method),
        secret: None,
    };
    let url = if request.url.len() > MAX_URL {
        None
    } else {
        Url::parse(&request.url).ok()
    };
    let Some(mut url) = url else {
        let err = HttpError::NotAllowed(format!("not a URL of at most {MAX_URL} characters"));
        log.write(None, None, outcome(&err), 0, started).await;
        return Err(err);
    };
    url.set_fragment(None);
    // The name asked for, if it could be a secret's name at all (plugin
    // text otherwise, which isn't logged).
    let secret_name = request.secret.clone().filter(|n| {
        (1..=40).contains(&n.len())
            && n.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    });
    let log = Logger {
        secret: secret_name.as_deref(),
        ..log
    };
    match send_checked(deps, plugins, http, plugin, request, &url, &log, started).await {
        Ok(response) => Ok(response),
        Err(Failed::Logged(err)) => Err(err),
        Err(Failed::Unlogged(err)) => {
            log.write(Some(&url), None, outcome(&err), 0, started).await;
            Err(err)
        }
    }
}

/// A refusal or failure, and whether it's in the log yet.
enum Failed {
    Unlogged(HttpError),
    Logged(HttpError),
}

impl From<HttpError> for Failed {
    fn from(err: HttpError) -> Self {
        Self::Unlogged(err)
    }
}

#[allow(clippy::too_many_arguments)]
async fn send_checked(
    deps: &crate::plugin_services::Deps,
    plugins: &std::sync::Weak<Plugins>,
    http: &Http,
    plugin: &str,
    request: HttpRequest,
    url: &Url,
    log: &Logger<'_>,
    started: Instant,
) -> Result<HttpResponse, Failed> {
    let host = check_url(url)?;
    let running = plugins
        .upgrade()
        .and_then(|p| p.running(plugin))
        .ok_or(HttpError::Unavailable)?;
    let approved = db::approved(&deps.db, plugin).await.map_err(|e| {
        tracing::error!(plugin, error = %e, "reading a plugin's approved hosts");
        HttpError::Unavailable
    })?;
    let hosts = usable_hosts(&approved, &running.manifest);
    if !hosts.contains(&host) {
        return Err(HttpError::NotAllowed(format!(
            "{host} isn't one of this app's approved hosts"
        ))
        .into());
    }
    let secret_headers: Vec<String> = approved
        .secrets
        .iter()
        .map(|s| s.header.to_ascii_lowercase())
        .collect();
    let headers = check_headers(&request.headers, &secret_headers)?;
    let body = match (request.method, request.body) {
        (HttpMethod::Get, Some(_)) => {
            return Err(HttpError::NotAllowed("a GET has no body".to_owned()).into());
        }
        (_, Some(body)) if body.len() > tether_plugins::services::MAX_HTTP_REQUEST_BODY => {
            return Err(HttpError::TooLarge.into());
        }
        (_, body) => body,
    };
    let secret = match &request.secret {
        None => None,
        Some(name) => {
            let spec = usable_secret(&approved, &running.manifest, name).ok_or_else(|| {
                HttpError::NotAllowed("not one of this app's approved secrets".to_owned())
            })?;
            if spec.host != host {
                return Err(HttpError::NotAllowed(format!(
                    "secret {} only goes to {}",
                    spec.name, spec.host
                ))
                .into());
            }
            let value = secret_value(deps, plugin, &spec.name).await?;
            let value = Secret::new(format!(
                "{}{}",
                spec.prefix.as_deref().unwrap_or(""),
                value.expose()
            ));
            Some((spec.clone(), value))
        }
    };
    let client = http.client(plugin, &hosts)?;

    let mut method = request.method;
    let mut body = body;
    let mut url = url.clone();
    for hop in 0..=MAX_REDIRECTS {
        let hop_host = check_url(&url)?;
        // Only on requests to the secret's own host, redirects included.
        let hop_secret = secret
            .as_ref()
            .filter(|(spec, _)| spec.host == hop_host)
            .map(|(spec, value)| (spec.header.as_str(), value));
        let response = hop_send(
            http,
            &client,
            method,
            &url,
            &hop_host,
            &headers,
            body.clone(),
            hop_secret,
        )
        .await;
        let response = match response {
            Ok(response) => response,
            Err(e) => {
                let err = match e {
                    HopError::Timeout => HttpError::Timeout,
                    HopError::Failed(why) => {
                        tracing::warn!(plugin, error = why, "plugin HTTP request");
                        HttpError::Unavailable
                    }
                };
                // With the host this hop went to.
                log.write(Some(&url), None, outcome(&err), 0, started).await;
                return Err(Failed::Logged(err));
            }
        };
        let status = response.status();
        let location = response
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        if let (true, Some(location)) = (status.is_redirection(), location) {
            log.write(Some(&url), Some(status.as_u16()), "redirect", 0, started)
                .await;
            let Ok(next) = url.join(&location) else {
                return Err(Failed::Logged(HttpError::NotAllowed(
                    "redirected to something that isn't a URL".to_owned(),
                )));
            };
            let next_host = match check_url(&next) {
                Ok(host) => host,
                Err(err) => {
                    log.write(Some(&next), None, "redirect refused", 0, started)
                        .await;
                    return Err(Failed::Logged(err));
                }
            };
            if !hosts.contains(&next_host) {
                // Logged with where it pointed, which is never contacted.
                log.write(Some(&next), None, "redirect refused", 0, started)
                    .await;
                return Err(Failed::Logged(HttpError::NotAllowed(format!(
                    "redirected to {next_host}, which isn't one of this app's approved hosts"
                ))));
            }
            if hop == MAX_REDIRECTS {
                return Err(Failed::Logged(HttpError::NotAllowed(format!(
                    "more than {MAX_REDIRECTS} redirects"
                ))));
            }
            // As browsers do: 303, and 301 or 302 after a POST, become a
            // GET without the body; 307 and 308 repeat the request.
            if status == StatusCode::SEE_OTHER
                || (method == HttpMethod::Post
                    && matches!(status, StatusCode::MOVED_PERMANENTLY | StatusCode::FOUND))
            {
                method = HttpMethod::Get;
                body = None;
            }
            url = next;
            url.set_fragment(None);
            continue;
        }
        let answer = read(response).await;
        let (bytes, label) = match &answer {
            Ok(r) => (r.body.len() as u64, "ok"),
            Err(e) => (0, outcome(e)),
        };
        log.write(Some(&url), Some(status.as_u16()), label, bytes, started)
            .await;
        return answer.map_err(Failed::Logged);
    }
    Err(HttpError::NotAllowed(format!("more than {MAX_REDIRECTS} redirects")).into())
}

enum HopError {
    Timeout,
    /// Why, for Tether's log (never with the URL: its query may carry
    /// the plugin's own keys).
    Failed(String),
}

#[allow(clippy::too_many_arguments)]
async fn hop_send(
    http: &Http,
    client: &Outbound,
    method: HttpMethod,
    url: &Url,
    host: &str,
    headers: &[(String, String)],
    body: Option<Vec<u8>>,
    secret: Option<(&str, &Secret<String>)>,
) -> Result<reqwest::Response, HopError> {
    let method = match method {
        HttpMethod::Get => reqwest::Method::GET,
        HttpMethod::Post => reqwest::Method::POST,
    };
    let route = http.route();
    let target = match &route {
        // Tests: the same path and query, on the stand-in.
        Some(route) => {
            let mut local = format!("http://{route}{}", url.path());
            if let Some(query) = url.query() {
                local.push('?');
                local.push_str(query);
            }
            local
        }
        None => url.to_string(),
    };
    let mut builder = client
        .request(method, &target)
        .map_err(|e| HopError::Failed(e.to_string()))?;
    if route.is_some() {
        builder = builder.header("x-tether-test-host", host);
    }
    for (name, value) in headers {
        builder = builder.header(name, value);
    }
    if let Some((header, value)) = secret {
        builder = builder
            .sensitive_header(header, value.expose())
            .ok_or_else(|| HopError::Failed("a secret isn't a valid header".to_owned()))?;
    }
    if let Some(body) = body {
        builder = builder.body(body);
    }
    builder.send().await.map_err(|e| {
        if e.is_timeout() {
            HopError::Timeout
        } else {
            HopError::Failed(e.without_url().to_string())
        }
    })
}

/// The body (capped) and the safe headers.
async fn read(mut response: reqwest::Response) -> Result<HttpResponse, HttpError> {
    if response
        .content_length()
        .is_some_and(|n| n > MAX_RESPONSE_BYTES as u64)
    {
        return Err(HttpError::TooLarge);
    }
    let status = response.status().as_u16();
    let headers = response
        .headers()
        .iter()
        .filter(|(name, _)| RESPONSE_HEADERS.contains(&name.as_str()))
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|v| (name.as_str().to_owned(), v.to_owned()))
        })
        .collect();
    let mut body = Vec::new();
    loop {
        match response.chunk().await {
            Ok(Some(chunk)) => {
                if body.len() + chunk.len() > MAX_RESPONSE_BYTES {
                    return Err(HttpError::TooLarge);
                }
                body.extend_from_slice(&chunk);
            }
            Ok(None) => break,
            Err(e) if e.is_timeout() => return Err(HttpError::Timeout),
            Err(_) => return Err(HttpError::Unavailable),
        }
    }
    Ok(HttpResponse {
        status,
        headers,
        body,
    })
}

async fn secret_value(
    deps: &crate::plugin_services::Deps,
    plugin: &str,
    name: &str,
) -> Result<Secret<String>, HttpError> {
    let stored = db::secret_name(plugin, name);
    let sealed = secrets::get(&deps.db, &stored)
        .await
        .map_err(|e| {
            tracing::error!(plugin, error = %e, "reading a plugin secret");
            HttpError::Unavailable
        })?
        .ok_or_else(|| {
            HttpError::NotAllowed(format!("secret {name} hasn't been entered by an admin yet"))
        })?;
    deps.key
        .open(&sealed, &secrets::context(&stored))
        .map_err(|_| {
            tracing::error!(
                plugin,
                secret = name,
                "a plugin secret can't be opened with this key"
            );
            HttpError::Unavailable
        })
}

// ---- approval and secrets (admin) -------------------------------------------

/// The secrets a manifest declares, as approved rows.
pub fn declared_secrets(manifest: &Manifest) -> Vec<db::SecretSpec> {
    manifest
        .capabilities
        .secrets
        .iter()
        .map(|(name, spec)| db::SecretSpec {
            name: name.clone(),
            host: spec.host.clone(),
            header: spec.header.clone(),
            prefix: spec.prefix.clone(),
        })
        .collect()
}

/// Records the hosts and secrets an admin approved with a package, in its
/// install's transaction. An upgrade must call this again from its own
/// approval, once the admin has seen what changed; until then a new host
/// or secret is refused at runtime. Call it holding the plugin's row
/// `FOR UPDATE` (installing inserts it; an upgrade must lock it, as
/// uninstalling does), so a secret being entered meanwhile waits.
pub async fn approve(
    tx: &mut sqlx::PgConnection,
    plugin: &str,
    manifest: &Manifest,
    by: AccountId,
) -> Result<Vec<String>, sqlx::Error> {
    db::approve(
        tx,
        plugin,
        &manifest.capabilities.http,
        &declared_secrets(manifest),
        by,
    )
    .await
}

/// Whether a package asks to call one of Tether's own destinations.
pub fn declares_core_host(manifest: &Manifest) -> bool {
    manifest.capabilities.http.iter().any(|h| is_core_host(h))
}

/// Enters or replaces a plugin secret's value. The value is sealed at
/// once and never shown again; only its name is audited.
pub async fn set_secret(
    state: &crate::AppState,
    actor: AccountId,
    plugin: &str,
    name: &str,
    value: &Secret<String>,
) -> Result<(), AppError> {
    let value = Secret::new(value.expose().trim().to_owned());
    if value.expose().is_empty()
        || value.expose().len() > MAX_SECRET
        || !value
            .expose()
            .bytes()
            .all(|b| b == b' ' || b.is_ascii_graphic())
    {
        return Err(AppError::bad_request(
            "A secret is 1 to 4,096 printable ASCII characters.",
        ));
    }
    let stored = db::secret_name(plugin, name);
    let sealed = state
        .key
        .seal(&value, &secrets::context(&stored))
        .map_err(AppError::internal)?;
    let mut tx = state.db.begin().await?;
    // Checked under a lock on the plugin's row, which uninstalling and
    // approving take too: a value is never stored for a secret that's
    // gone meanwhile.
    if !db::lock_approved_secret(&mut tx, plugin, name).await? {
        return Err(AppError::not_found("This app has no such approved secret."));
    }
    secrets::put(&mut *tx, &stored, &sealed).await?;
    audit::record(
        &mut *tx,
        Actor::Account(actor),
        "plugin.secret_set",
        Some(&format!("plugin:{plugin}")),
        json!({ "secret": name }),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn core_hosts_are_refused() {
        for host in [
            "esi.evetech.net",
            "images.evetech.net",
            "login.eveonline.com",
            "discord.com",
            "cdn.discordapp.com",
            "api.github.com",
            "objects.githubusercontent.com",
            "raw.githubusercontent.com",
            "media.discordapp.net",
            "ESI.EVETECH.NET",
        ] {
            assert!(is_core_host(host), "{host}");
        }
        for host in [
            "zkillboard.com",
            "janice.e-351.com",
            "notgithub.com",
            "evetech.net.example",
        ] {
            assert!(!is_core_host(host), "{host}");
        }
    }

    #[test]
    fn urls_are_https_hostnames_on_443() {
        let ok = Url::parse("https://ZKillboard.com/api/killID/1/?x=1").unwrap();
        assert_eq!(check_url(&ok).unwrap(), "zkillboard.com");
        for bad in [
            "http://zkillboard.com/",
            "https://zkillboard.com:8443/",
            "https://user:pw@zkillboard.com/",
            "https://127.0.0.1/",
            "https://[::1]/",
            "ftp://zkillboard.com/",
        ] {
            let url = Url::parse(bad).unwrap();
            assert!(
                matches!(check_url(&url), Err(HttpError::NotAllowed(_))),
                "{bad}"
            );
        }
    }

    #[test]
    fn only_listed_headers_and_never_credentials() {
        let h = |n: &str, v: &str| vec![(n.to_owned(), v.to_owned())];
        assert!(check_headers(&h("Accept", "application/json"), &[]).is_ok());
        for name in [
            "Authorization",
            "Cookie",
            "Proxy-Authorization",
            "Host",
            "User-Agent",
            "X-Forwarded-For",
            "Content-Length",
        ] {
            assert!(check_headers(&h(name, "x"), &[]).is_err(), "{name}");
        }
        // A header a secret goes in, even an allowed one.
        assert!(check_headers(&h("accept", "x"), &["accept".to_owned()]).is_err());
        assert!(check_headers(&h("accept", "a\r\nX-Evil: 1"), &[]).is_err());
        let twice = vec![
            ("accept".to_owned(), "a".to_owned()),
            ("Accept".to_owned(), "b".to_owned()),
        ];
        assert!(check_headers(&twice, &[]).is_err());
    }
}
