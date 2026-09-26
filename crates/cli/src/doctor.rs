//! `tether doctor`: checks an instance end to end and prints a fix for
//! every problem (N3).

use std::io::Write;
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use tether_core::crypto::EncryptionKey;
use tether_db::PgPool;
use tether_db::settings;
use tether_discord::store::StoreError;
use tether_discord::{Discord, DiscordError};
use tether_esi::Esi;

const TIMEOUT: Duration = Duration::from_secs(10);
pub const SSO_METADATA_URL: &str =
    "https://login.eveonline.com/.well-known/oauth-authorization-server";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Ok,
    Warn,
    Fail,
    Skip,
}

impl Status {
    fn label(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warn => "WARN",
            Self::Fail => "FAIL",
            Self::Skip => "skip",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Check {
    pub name: &'static str,
    pub status: Status,
    pub detail: String,
    pub fix: Option<String>,
}

impl Check {
    fn ok(name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            name,
            status: Status::Ok,
            detail: detail.into(),
            fix: None,
        }
    }

    fn warn(name: &'static str, detail: impl Into<String>, fix: impl Into<String>) -> Self {
        Self {
            name,
            status: Status::Warn,
            detail: detail.into(),
            fix: Some(fix.into()),
        }
    }

    fn fail(name: &'static str, detail: impl Into<String>, fix: impl Into<String>) -> Self {
        Self {
            name,
            status: Status::Fail,
            detail: detail.into(),
            fix: Some(fix.into()),
        }
    }

    fn skip(name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            name,
            status: Status::Skip,
            detail: detail.into(),
            fix: None,
        }
    }
}

/// Everything the checks talk to, so tests can point them at local servers.
pub struct Env {
    pub db: PgPool,
    pub esi: Esi,
    /// `None` when ENCRYPTION_KEY is unset or invalid for this command.
    pub key: Option<EncryptionKey>,
    pub discord: Discord,
    /// Normally `https://api.github.com`.
    pub github_api_url: String,
    pub domain: String,
    pub public_url: String,
    pub sso_metadata_url: String,
    /// Normally 80 and 443.
    pub http_port: u16,
    pub https_port: u16,
}

/// Runs every check, prints them, and returns whether all passed (no FAIL).
pub async fn run(env: &Env, out: &mut dyn Write) -> std::io::Result<bool> {
    let checks = checks(env).await;
    for check in &checks {
        writeln!(
            out,
            "[{:>4}] {}: {}",
            check.status.label(),
            check.name,
            check.detail
        )?;
        if let Some(fix) = &check.fix {
            writeln!(out, "       fix: {fix}")?;
        }
    }
    let failed = checks.iter().filter(|c| c.status == Status::Fail).count();
    let warned = checks.iter().filter(|c| c.status == Status::Warn).count();
    writeln!(out)?;
    writeln!(
        out,
        "{failed} failed, {warned} warning(s), {} checks",
        checks.len()
    )?;
    Ok(failed == 0)
}

pub async fn checks(env: &Env) -> Vec<Check> {
    let mut checks = vec![database(&env.db).await];
    let ip = match dns(&env.domain).await {
        Ok((check, ip)) => {
            checks.push(check);
            ip
        }
        Err(check) => {
            checks.push(check);
            None
        }
    };
    match ip {
        // DOMAIN=localhost is a local test install: inside the container,
        // localhost is the app itself, so these checks can't mean anything.
        Some(ip) if ip.is_loopback() => {
            let why = "DOMAIN is a loopback name; check from the host with `curl -k https://localhost/health`";
            for name in ["port 80", "port 443", "https", "public url"] {
                checks.push(Check::skip(name, why));
            }
        }
        Some(ip) => {
            checks.push(port("port 80", ip, env.http_port).await);
            checks.push(port("port 443", ip, env.https_port).await);
            checks.push(tls(&env.public_url).await);
            checks.push(reachable(&env.public_url).await);
        }
        None => {
            for name in ["port 80", "port 443", "https", "public url"] {
                checks.push(Check::skip(name, "needs DNS"));
            }
        }
    }
    checks.push(esi(&env.esi).await);
    checks.push(sso(&env.db, &env.sso_metadata_url, &env.public_url).await);
    checks.push(setup(&env.db, &env.public_url).await);
    checks.push(jobs(&env.db).await);
    checks.push(ownership(&env.db).await);
    checks.push(discord(env).await);
    checks.push(updates(&env.db, &env.github_api_url).await);
    checks.push(outbound());
    checks.push(plugin_hosts(&env.db).await);
    checks
}

/// The hosts each enabled app may call over HTTPS, as its admin approved
/// them at install: beyond Tether's own list, per instance.
pub async fn plugin_hosts(db: &PgPool) -> Check {
    const NAME: &str = "app hosts";
    match tether_db::plugin_http::enabled_hosts(db).await {
        Ok(rows) if rows.is_empty() => Check::ok(NAME, "no app may make HTTPS requests"),
        Ok(rows) => {
            let mut by_plugin: Vec<(String, Vec<String>)> = Vec::new();
            for (plugin, host) in rows {
                match by_plugin.last_mut() {
                    Some((p, hosts)) if *p == plugin => hosts.push(host),
                    _ => by_plugin.push((plugin, vec![host])),
                }
            }
            let listed: Vec<String> = by_plugin
                .iter()
                .map(|(plugin, hosts)| format!("{plugin} -> {}", hosts.join(", ")))
                .collect();
            Check::ok(NAME, format!("approved for apps: {}", listed.join("; ")))
        }
        Err(err) => Check::fail(NAME, err.to_string(), "Fix the database check first."),
    }
}

pub async fn database(db: &PgPool) -> Check {
    const NAME: &str = "database";
    let applied =
        sqlx::query_scalar!(r#"SELECT count(*) AS "n!" FROM _sqlx_migrations WHERE success"#)
            .fetch_one(db)
            .await;
    let embedded = tether_db::MIGRATOR.iter().count();
    match applied {
        Ok(n) if usize::try_from(n).is_ok_and(|n| n >= embedded) => Check::ok(
            NAME,
            format!("connected, {n}/{embedded} migrations applied"),
        ),
        Ok(n) => Check::fail(
            NAME,
            format!("only {n}/{embedded} migrations applied"),
            "Restart the app container; migrations run automatically at startup. If it keeps failing, `docker compose logs app` shows why.",
        ),
        Err(err) => Check::fail(
            NAME,
            format!("can't query Postgres: {err}"),
            "Check the db container is running (`docker compose ps`) and POSTGRES_PASSWORD in .env hasn't changed since the volume was created.",
        ),
    }
}

/// On success, returns the address the ports are checked against.
pub async fn dns(domain: &str) -> Result<(Check, Option<IpAddr>), Check> {
    const NAME: &str = "dns";
    let lookup = tokio::time::timeout(TIMEOUT, tokio::net::lookup_host((domain, 443))).await;
    let addrs: Vec<SocketAddr> = match lookup {
        Ok(Ok(addrs)) => addrs.collect(),
        Ok(Err(err)) => {
            return Err(Check::fail(
                NAME,
                format!("{domain} does not resolve: {err}"),
                format!(
                    "Create an A (and optionally AAAA) record for {domain} pointing at this server's public IP."
                ),
            ));
        }
        Err(_) => {
            return Err(Check::fail(
                NAME,
                format!("looking up {domain} timed out"),
                "Check this server's DNS resolver.",
            ));
        }
    };
    let Some(first) = addrs.first() else {
        return Err(Check::fail(
            NAME,
            format!("{domain} has no addresses"),
            format!("Add an A record for {domain}."),
        ));
    };
    let ips: Vec<String> = addrs.iter().map(|a| a.ip().to_string()).collect();
    let check = if first.ip().is_loopback() {
        Check::warn(
            NAME,
            format!(
                "{domain} resolves to {} (this machine only)",
                ips.join(", ")
            ),
            "Fine for local testing. For a real install, set DOMAIN to a public name with an A record.",
        )
    } else {
        Check::ok(NAME, format!("{domain} resolves to {}", ips.join(", ")))
    };
    Ok((check, Some(first.ip())))
}

/// Connects to `ip:port` from this server. A true outside check would need a
/// third-party service, which the opsec rules forbid; most hosts route this
/// through the same public path.
pub async fn port(name: &'static str, ip: IpAddr, port: u16) -> Check {
    let addr = SocketAddr::new(ip, port);
    match tokio::time::timeout(TIMEOUT, tokio::net::TcpStream::connect(addr)).await {
        Ok(Ok(_)) => Check::ok(
            name,
            format!(
                "{addr} accepts connections (checked from this server; cloud firewalls can still block outside traffic)"
            ),
        ),
        Ok(Err(err)) => Check::fail(
            name,
            format!("{addr} refused: {err}"),
            format!(
                "Open TCP {port} in every firewall in the path: the cloud provider's (Oracle security lists, Azure network security groups, AWS security groups) and the host's (iptables/nftables, ufw), and make sure Caddy is running. This check runs from the server itself, so it can pass while those still block outside traffic."
            ),
        ),
        Err(_) => Check::fail(
            name,
            format!("{addr} timed out"),
            format!(
                "Something is dropping TCP {port}. Check every firewall in the path: the cloud provider's (Oracle security lists, Azure network security groups, AWS security groups) and the host's (iptables/nftables, ufw)."
            ),
        ),
    }
}

pub async fn tls(public_url: &str) -> Check {
    const NAME: &str = "https";
    if !public_url.starts_with("https://") {
        return Check::warn(
            NAME,
            format!("{public_url} is not HTTPS"),
            "Unset PUBLIC_URL so it defaults to https://DOMAIN.",
        );
    }
    let request = match get(&format!("{public_url}/health")) {
        Ok(request) => request,
        Err(err) => return Check::fail(NAME, err, "This is a bug; please report it."),
    };
    match request.send().await {
        Ok(r) if r.status().is_success() => Check::ok(NAME, "valid certificate, /health answers"),
        Ok(r) => Check::fail(
            NAME,
            format!("/health answered HTTP {}", r.status()),
            "Caddy is up but not reaching the app: check `docker compose ps` and `docker compose logs app`.",
        ),
        Err(err) => Check::fail(
            NAME,
            format!("request failed: {}", chain(&err)),
            "Caddy gets the certificate automatically once DNS points here and port 80 is open; `docker compose logs caddy` shows why it hasn't.",
        ),
    }
}

pub async fn reachable(public_url: &str) -> Check {
    const NAME: &str = "public url";
    match tether_web::setup::check_public_url(public_url).await {
        Ok((true, detail)) => Check::ok(NAME, detail),
        Ok((false, detail)) => Check::fail(
            NAME,
            detail,
            "EVE SSO redirects browsers to this URL after login, so it must reach this instance. Fix DNS, ports and TLS above first.",
        ),
        Err(err) => Check::fail(NAME, err.to_string(), "This is a bug; please report it."),
    }
}

pub async fn discord(env: &Env) -> Check {
    const NAME: &str = "discord";
    let page = format!("{}/admin/discord", env.public_url);
    let Some(key) = &env.key else {
        return Check::warn(
            NAME,
            "ENCRYPTION_KEY isn't set (or isn't valid) for this command, so the Discord secrets can't be checked",
            "Run doctor inside the container, where .env is loaded: `docker compose exec tether tether doctor`.",
        );
    };
    let stored = match tether_discord::store::stored(&env.db, key).await {
        Ok(stored) => stored,
        Err(StoreError::Crypto(_)) => {
            return Check::fail(
                NAME,
                "the stored bot token and client secret can't be decrypted",
                format!(
                    "ENCRYPTION_KEY in .env changed since they were saved. Put the old key back, or enter the secret and token again on {page}."
                ),
            );
        }
        Err(err) => return Check::fail(NAME, err.to_string(), "Fix the database check first."),
    };
    let Some(config) = stored.config() else {
        return Check::warn(
            NAME,
            "Discord isn't set up; members can't link or get roles",
            format!("Open {page} and follow the steps."),
        );
    };
    match env.discord.check(&config).await {
        Ok(check) if !check.missing_permissions.is_empty() => Check::fail(
            NAME,
            format!(
                "bot {} is in {} but is missing {}",
                check.bot_name,
                check.guild_name,
                check.missing_permissions.join(", ")
            ),
            "Give the bot's role those permissions in Server Settings → Roles, or re-invite it with the link on the Discord admin page.",
        ),
        Ok(check) => Check::ok(
            NAME,
            format!("bot {} is in {}", check.bot_name, check.guild_name),
        ),
        Err(DiscordError::BadBotToken) => Check::fail(
            NAME,
            "Discord rejected the bot token",
            format!(
                "Reset the token under Bot in the Discord developer portal and save it on {page}."
            ),
        ),
        Err(err) if err.is_transient() => Check::fail(
            NAME,
            err.to_string(),
            "Allow outbound HTTPS to discord.com. If Discord itself is down, try again later.",
        ),
        Err(err) => Check::fail(
            NAME,
            err.to_string(),
            format!("Check the settings on {page}."),
        ),
    }
}

pub const GITHUB_API_URL: &str = "https://api.github.com";

/// Update checks are the one reason the host itself talks to GitHub (until
/// plugins): say whether they're on, and if so whether GitHub answers.
pub async fn updates(db: &PgPool, api_url: &str) -> Check {
    const NAME: &str = "update checks";
    match tether_web::updates::enabled(db).await {
        Ok(false) => {
            return Check::ok(NAME, "off: Tether doesn't contact GitHub for updates");
        }
        Ok(true) => {}
        Err(err) => return Check::fail(NAME, err.to_string(), "Fix the database check first."),
    }
    let request =
        tether_web::updates::http_client(tether_net::Allowlist::production().with_url(api_url))
            .map_err(|e| e.to_string())
            .and_then(|client| client.get(api_url).map_err(|e| e.to_string()));
    let request = match request {
        Ok(request) => request,
        Err(err) => return Check::fail(NAME, err, "This is a bug; please report it."),
    };
    match request.send().await {
        // Any answer means it's reachable; the daily check reports the rest.
        Ok(_) => Check::ok(NAME, "on: api.github.com answers"),
        Err(err) => Check::warn(
            NAME,
            format!("on, but GitHub didn't answer: {}", chain(&err)),
            "Allow outbound HTTPS to api.github.com, or switch update checks off on the System admin page.",
        ),
    }
}

/// eve-esi-client's fixed base URL (it makes its own connections).
const ESI_BASE_URL: &str = "https://esi.evetech.net";

/// The allow-list (N5): every endpoint the server is configured to call must
/// be on it, and proxy settings that the libraries with their own HTTP
/// clients honour are flagged.
pub fn outbound() -> Check {
    const NAME: &str = "outbound";
    let allow = tether_net::Allowlist::production();
    let endpoints = [
        ("ESI", ESI_BASE_URL.to_owned()),
        ("EVE SSO keys", tether_esi::jwt::CCP_JWKS_URL.to_owned()),
        ("EVE SSO", SSO_METADATA_URL.to_owned()),
        ("Discord", tether_discord::Endpoints::discord().api_base()),
        ("GitHub", GITHUB_API_URL.to_owned()),
    ];
    let off_list: Vec<String> = endpoints
        .iter()
        .filter(|(_, url)| allow.check(url).is_err())
        .map(|(what, url)| format!("{what} ({url})"))
        .collect();
    if !off_list.is_empty() {
        return Check::fail(
            NAME,
            format!("configured but not allowed: {}", off_list.join(", ")),
            "This is a bug; please report it.",
        );
    }
    let proxies = tether_net::proxy_variables();
    if !proxies.is_empty() {
        return Check::warn(
            NAME,
            format!(
                "{} set: ESI and EVE SSO requests may go through a proxy",
                proxies.join(", ")
            ),
            "Unset them in the app's environment, unless routing Tether's traffic through that proxy is intended.",
        );
    }
    let hosts: Vec<&str> = allow.hosts().collect();
    Check::ok(
        NAME,
        format!(
            "the server only contacts {}; browsers also load images.evetech.net, and Caddy talks to Let's Encrypt",
            hosts.join(", ")
        ),
    )
}

pub async fn esi(esi: &Esi) -> Check {
    const NAME: &str = "esi";
    match esi.players_online().await {
        Ok(players) => Check::ok(NAME, format!("ESI answers ({players} pilots online)")),
        Err(err) => Check::fail(
            NAME,
            format!("ESI request failed: {err}"),
            "Allow outbound HTTPS to esi.evetech.net. If ESI itself is down (downtime is 11:00 EVE), try again later.",
        ),
    }
}

pub async fn sso(db: &PgPool, metadata_url: &str, public_url: &str) -> Check {
    const NAME: &str = "eve sso";
    let callback = format!("{public_url}/auth/callback");
    let client_id = match settings::get_string(db, settings::SSO_CLIENT_ID).await {
        Ok(Some(id)) => id,
        Ok(None) => {
            return Check::fail(
                NAME,
                "no SSO client id",
                format!("Open {public_url}/ and finish the setup wizard."),
            );
        }
        Err(err) => return Check::fail(NAME, err.to_string(), "Fix the database check first."),
    };
    let reachable = match get(metadata_url) {
        Ok(request) => request.send().await.map_err(|e| chain(&e)).and_then(|r| {
            if r.status().is_success() {
                Ok(())
            } else {
                Err(format!("HTTP {}", r.status()))
            }
        }),
        Err(err) => Err(err),
    };
    if let Err(err) = reachable {
        return Check::fail(
            NAME,
            format!("can't reach EVE SSO: {err}"),
            "Allow outbound HTTPS to login.eveonline.com.",
        );
    }
    let success = settings::get(db, settings::SSO_LAST_SUCCESS)
        .await
        .ok()
        .flatten();
    let error = settings::get(db, settings::SSO_LAST_ERROR)
        .await
        .ok()
        .flatten();
    let at = |v: &serde_json::Value| v["at"].as_str().unwrap_or("?").to_owned();
    let newer_error = match (&success, &error) {
        (Some(s), Some(e)) => at(e) > at(s),
        (None, Some(_)) => true,
        _ => false,
    };
    if newer_error {
        let e = error.unwrap_or_default();
        Check::fail(
            NAME,
            format!(
                "client {client_id}: the last login failed at {}: {}",
                at(&e),
                e["error"].as_str().unwrap_or("unknown error")
            ),
            format!(
                "At developers.eveonline.com, check the application's client id is {client_id} and its callback URL is exactly {callback}."
            ),
        )
    } else if let Some(s) = success {
        Check::ok(
            NAME,
            format!(
                "client {client_id}; callback confirmed by a login at {}",
                at(&s)
            ),
        )
    } else {
        Check::warn(
            NAME,
            format!("client {client_id} set, but no login has confirmed the callback yet"),
            format!(
                "Log in once. The callback URL registered with CCP must be exactly {callback}."
            ),
        )
    }
}

pub async fn setup(db: &PgPool, public_url: &str) -> Check {
    const NAME: &str = "setup";
    let owner = tether_db::accounts::owner_exists(db).await.unwrap_or(false);
    if !owner {
        return Check::warn(
            NAME,
            "no owner yet",
            format!(
                "Open {public_url}/ and complete the setup wizard with the SETUP_TOKEN from .env."
            ),
        );
    }
    let covered = tether_db::states::covered(db).await.unwrap_or_default();
    let member = tether_db::states::builtin(db, tether_core::states::Builtin::Member)
        .await
        .ok()
        .flatten();
    match member {
        None => Check::ok(
            NAME,
            format!(
                "owner exists; the Member state was deleted; states cover {} entities",
                covered.len()
            ),
        ),
        Some(m) if covered.iter().any(|c| c.state == m.id) => Check::ok(
            NAME,
            format!("owner exists, states cover {} entities", covered.len()),
        ),
        Some(m) => Check::warn(
            NAME,
            format!("{} covers nobody yet, so everyone is Guest", m.name),
            format!(
                "Choose your alliance in the setup wizard, on Admin → States, or with `tether states add \"{}\" <id>`.",
                m.name
            ),
        ),
    }
}

/// Whether the ownership check stopped taking characters away because too
/// many tokens died at once.
pub async fn ownership(db: &PgPool) -> Check {
    const NAME: &str = "ownership";
    match tether_db::settings::get(db, tether_web::ownership::BREAKER_SETTING).await {
        Ok(Some(tripped)) => Check::warn(
            NAME,
            format!(
                "stopped: {} of {} tokens dead at once (since {})",
                tripped["dead"], tripped["total"], tripped["at"]
            ),
            "Check EVE SSO and the application at developers.eveonline.com. If the revocations \
             are real, run `tether ownership sweep --force`.",
        ),
        Ok(None) => Check::ok(NAME, "characters with dead tokens are handled"),
        Err(err) => Check::fail(NAME, err.to_string(), "Check the database."),
    }
}

pub async fn jobs(db: &PgPool) -> Check {
    const NAME: &str = "jobs";
    match tether_jobs::counts(db).await {
        Ok(counts) => {
            let dead = counts
                .iter()
                .find(|(s, _)| *s == tether_jobs::JobState::Dead)
                .map_or(0, |(_, n)| *n);
            let queued = counts
                .iter()
                .find(|(s, _)| *s == tether_jobs::JobState::Queued)
                .map_or(0, |(_, n)| *n);
            if dead > 0 {
                Check::warn(
                    NAME,
                    format!("{dead} dead job(s), {queued} queued"),
                    "See why with `tether jobs --state dead`; retry with `tether jobs retry <id>`.",
                )
            } else {
                Check::ok(NAME, format!("no dead jobs, {queued} queued"))
            }
        }
        Err(err) => Check::fail(NAME, err.to_string(), "Fix the database check first."),
    }
}

/// A GET through the allow-listed client, which may also reach the one
/// address being checked (the instance itself, or a test stand-in).
fn get(url: &str) -> Result<tether_net::Request, String> {
    let client = tether_net::Outbound::new(
        tether_net::Allowlist::production().with_url(url),
        concat!("tether/", env!("CARGO_PKG_VERSION")),
        TIMEOUT,
    )
    .map_err(|e| e.to_string())?;
    client.get(url).map_err(|e| e.to_string())
}

fn chain(err: &dyn std::error::Error) -> String {
    let mut out = err.to_string();
    let mut source = err.source();
    while let Some(s) = source {
        out.push_str(": ");
        out.push_str(&s.to_string());
        source = s.source();
    }
    out
}
