//! `tether doctor`: checks an instance end to end and prints a fix for
//! every problem (N3).

use std::io::Write;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
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

/// What terminates TLS in front of the app: `TETHER_PROXY` in deploy/.env,
/// chosen with deploy/install.sh.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Proxy {
    /// The bundled Caddy container, with Let's Encrypt certificates.
    Caddy,
    /// The admin's nginx on the host, proxying to 127.0.0.1.
    Nginx,
    /// The admin's Traefik, in Docker on a shared network.
    Traefik,
    /// The admin's own proxy of any kind, proxying to 127.0.0.1 (`none`).
    Own,
}

impl Proxy {
    /// `TETHER_PROXY`'s value; unset is Caddy, as installs before the
    /// choice existed. `Err` holds an unknown value.
    pub fn from_setting(value: Option<&str>) -> Result<Self, String> {
        match value.map(str::trim) {
            None | Some("" | "caddy") => Ok(Self::Caddy),
            Some("nginx") => Ok(Self::Nginx),
            Some("traefik") => Ok(Self::Traefik),
            Some("none") => Ok(Self::Own),
            Some(other) => Err(other.to_owned()),
        }
    }

    /// For sentences: "make sure {} is running".
    fn name(self) -> &'static str {
        match self {
            Self::Caddy => "Caddy",
            Self::Nginx => "nginx",
            Self::Traefik => "Traefik",
            Self::Own => "your reverse proxy",
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
    /// `TETHER_PROXY`; `Err` holds a value that isn't one of the choices.
    pub proxy: Result<Proxy, String>,
    /// Normally 80 and 443.
    pub http_port: u16,
    pub https_port: u16,
    /// The snapshots volume (`SNAPSHOT_DIR`).
    pub snapshot_dir: PathBuf,
    /// Where the Postgres client tools are (`PG_BIN_DIR`); `None` for PATH.
    pub pg_bin_dir: Option<PathBuf>,
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
    let mut checks = vec![database(&env.db).await, proxy(&env.proxy)];
    // An unknown setting fails the check above; Caddy's checks are the
    // strictest.
    let proxy = env.proxy.clone().unwrap_or(Proxy::Caddy);
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
            let why = match proxy {
                Proxy::Caddy | Proxy::Traefik => {
                    "DOMAIN is a loopback name; check from the host with `curl -k https://localhost/health`"
                }
                Proxy::Nginx | Proxy::Own => {
                    "DOMAIN is a loopback name; check from the host with `curl -k https://localhost/health`, \
                     or the app itself with `curl http://127.0.0.1:TETHER_PORT/health`"
                }
            };
            for name in ["port 80", "port 443", "https", "public url"] {
                checks.push(Check::skip(name, why));
            }
        }
        Some(ip) => {
            checks.push(http_port(ip, env.http_port, proxy).await);
            checks.push(port("port 443", ip, env.https_port, proxy).await);
            checks.push(tls(&env.public_url, proxy).await);
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
    checks.push(outbound(proxy));
    checks.push(plugin_hosts(&env.db).await);
    checks.push(snapshots(&env.db, &env.snapshot_dir, env.pg_bin_dir.clone()).await);
    checks.push(backups(&env.snapshot_dir).await);
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

/// Snapshots need the Postgres client tools, a writable volume and room
/// on it; without them the server won't migrate (N14).
pub async fn snapshots(db: &PgPool, dir: &Path, pg_bin_dir: Option<PathBuf>) -> Check {
    const NAME: &str = "snapshots";
    let volume_fix = format!(
        "Mount the snapshots volume at {} (deploy/docker-compose.yml does), writable by the \
         app's user, or set SNAPSHOT_DIR.",
        dir.display()
    );
    let server = sqlx::query_scalar!(
        r#"SELECT current_setting('server_version_num')::int / 10000 AS "major!""#
    )
    .fetch_one(db)
    .await;
    let major = match server {
        Ok(major) => u32::try_from(major).unwrap_or_default(),
        Err(err) => return Check::fail(NAME, err.to_string(), "Fix the database check first."),
    };
    if let Err(err) = tether_snapshots::Tools::new(pg_bin_dir).check(major).await {
        return Check::fail(
            NAME,
            err.to_string(),
            format!(
                "The app image includes postgresql-client-{major}. Outside Docker, install it \
                 or point PG_BIN_DIR at its bin directory."
            ),
        );
    }
    if let Err(err) = writable(dir).await {
        return Check::fail(
            NAME,
            format!("{} isn't writable: {err}", dir.display()),
            volume_fix,
        );
    }
    let free = match tether_snapshots::free_bytes(dir).await {
        Ok(free) => free,
        Err(err) => return Check::warn(NAME, err.to_string(), volume_fix),
    };
    let listed = match tether_snapshots::list(dir).await {
        Ok(listed) => listed,
        Err(err) => return Check::fail(NAME, err.to_string(), volume_fix),
    };
    let newest = listed
        .iter()
        .find(|s| s.header.reason == tether_snapshots::Reason::BeforeMigrations)
        .map(|s| {
            format!(
                ", newest {} UTC ({})",
                s.header.taken_at.format("%Y-%m-%d %H:%M"),
                s.header.kind
            )
        })
        .unwrap_or_default();
    let taken = listed
        .iter()
        .filter(|s| s.header.reason == tether_snapshots::Reason::BeforeMigrations)
        .count();
    let detail = format!(
        "{}: {} MiB free, {taken} snapshot(s) taken before migrations{newest}; Postgres {major} tools",
        dir.display(),
        free / (1024 * 1024)
    );
    match tether_snapshots::set_aside_schemas(db).await {
        Ok(aside) if !aside.is_empty() => {
            return Check::warn(
                NAME,
                format!(
                    "{detail}; an app's rollback was cut short, its earlier data is in {}",
                    aside.join(", ")
                ),
                "Stop Tether and run `tether rollback --plugin <app id>` again to finish it; \
                 the app won't load until then.",
            );
        }
        Ok(_) => {}
        Err(err) => return Check::fail(NAME, err.to_string(), "Fix the database check first."),
    }
    if free < 1024 * 1024 * 1024 {
        Check::warn(
            NAME,
            detail,
            "Under 1 GiB free: snapshots before migrations and nightly backups may not fit, and \
             the server won't migrate without a snapshot. Free some space.",
        )
    } else {
        Check::ok(NAME, detail)
    }
}

/// Creates and removes a file in `dir`.
async fn writable(dir: &Path) -> std::io::Result<()> {
    let probe = dir.join(format!(".doctor-{}", std::process::id()));
    tokio::fs::write(&probe, b"").await?;
    tokio::fs::remove_file(&probe).await
}

/// The last nightly backup should be under a day and a half old.
pub async fn backups(dir: &Path) -> Check {
    const NAME: &str = "backups";
    let fix = "The nightly backup job runs a day apart; `docker compose logs app` and \
               `tether jobs --state dead` show why it failed.";
    let listed = match tether_snapshots::list(dir).await {
        Ok(listed) => listed,
        Err(err) => return Check::fail(NAME, err.to_string(), fix),
    };
    let nightly: Vec<_> = listed
        .iter()
        .filter(|s| s.header.reason == tether_snapshots::Reason::Nightly)
        .collect();
    let Some(last) = nightly.first() else {
        return Check::warn(NAME, "no nightly backup yet", fix);
    };
    let age = chrono::Utc::now() - last.header.taken_at;
    let detail = format!(
        "last nightly backup {} UTC, {} kept (encrypted with ENCRYPTION_KEY: keep a copy of it \
         somewhere else)",
        last.header.taken_at.format("%Y-%m-%d %H:%M"),
        nightly.len()
    );
    if age > chrono::Duration::hours(36) {
        Check::warn(NAME, detail, fix)
    } else {
        Check::ok(NAME, detail)
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
pub async fn port(name: &'static str, ip: IpAddr, port: u16, proxy: Proxy) -> Check {
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
                "Open TCP {port} in every firewall in the path: the cloud provider's (Oracle security lists, Azure network security groups, AWS security groups) and the host's (iptables/nftables, ufw), and make sure {} is running and listening on it. This check runs from the server itself, so it can pass while those still block outside traffic.",
                proxy.name()
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

/// The HTTP port (normally 80). Caddy needs it, for Let's Encrypt's HTTP
/// challenge and the redirect to HTTPS. Another proxy may do without it
/// (DNS challenges, no redirect), so there it being closed is a warning.
pub async fn http_port(ip: IpAddr, port_number: u16, proxy: Proxy) -> Check {
    let check = port("port 80", ip, port_number, proxy).await;
    if check.status != Status::Fail || proxy == Proxy::Caddy {
        return check;
    }
    Check::warn(
        check.name,
        check.detail,
        format!(
            "{} Port {port_number} is only needed if {} redirects HTTP to HTTPS or gets \
             certificates with HTTP challenges (certbot's default).",
            check.fix.unwrap_or_default(),
            proxy.name()
        ),
    )
}

/// Which proxy terminates TLS, and so whose certificates they are.
pub fn proxy(setting: &Result<Proxy, String>) -> Check {
    const NAME: &str = "proxy";
    match setting {
        Ok(Proxy::Caddy) => Check::ok(
            NAME,
            "bundled Caddy terminates TLS, with certificates from Let's Encrypt",
        ),
        Ok(Proxy::Nginx) => Check::ok(
            NAME,
            "nginx on the host terminates TLS and proxies to the app on 127.0.0.1; its \
             certificates (certbot's, say) are the admin's, outside Tether",
        ),
        Ok(Proxy::Traefik) => Check::ok(
            NAME,
            "the admin's Traefik terminates TLS and reaches the app over a shared Docker network; \
             its certificates are the admin's, outside Tether",
        ),
        Ok(Proxy::Own) => Check::ok(
            NAME,
            "the admin's own reverse proxy terminates TLS and proxies to the app on 127.0.0.1; \
             its certificates are the admin's, outside Tether",
        ),
        Err(value) => Check::fail(
            NAME,
            format!("TETHER_PROXY is {value:?}, not one of caddy, nginx, traefik or none"),
            "Re-run deploy/install.sh with --proxy caddy, nginx, traefik or none: it keeps \
             deploy/.env's secrets and rewrites only the proxy settings.",
        ),
    }
}

pub async fn tls(public_url: &str, proxy: Proxy) -> Check {
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
            match proxy {
                Proxy::Caddy | Proxy::Traefik => format!(
                    "{} is up but not reaching the app: check `docker compose ps` and `docker compose logs app`.",
                    proxy.name()
                ),
                Proxy::Nginx | Proxy::Own => format!(
                    "{} is up but not reaching the app, which listens on http://127.0.0.1:TETHER_PORT \
                     (deploy/.env): check `docker compose ps` and `docker compose logs app`.",
                    proxy.name()
                ),
            },
        ),
        Err(err) => Check::fail(
            NAME,
            format!("request failed: {}", chain(&err)),
            match proxy {
                Proxy::Caddy => {
                    "Caddy gets the certificate automatically once DNS points here and \
                                 port 80 is open; `docker compose logs caddy` shows why it hasn't."
                        .to_owned()
                }
                Proxy::Nginx => {
                    "The certificate is nginx's, so yours: `sudo certbot --nginx -d DOMAIN` \
                                 gets one from Let's Encrypt and adds it to Tether's server block. \
                                 `sudo nginx -t` and nginx's error log show other problems."
                        .to_owned()
                }
                Proxy::Traefik => "The certificate is Traefik's, from the resolver named by \
                                   TRAEFIK_CERTRESOLVER in deploy/.env; Traefik's logs show why it \
                                   hasn't got one. The app must be on TRAEFIK_NETWORK too."
                    .to_owned(),
                Proxy::Own => "Your reverse proxy must serve DOMAIN over HTTPS with a valid \
                               certificate and proxy to the app: see deploy/README.md."
                    .to_owned(),
            },
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

/// The allow-list (N5): every endpoint the server is configured to call must
/// be on it. Proxy settings in the environment are flagged: every client
/// ignores them, which may not be what the operator expects.
pub fn outbound(proxy: Proxy) -> Check {
    const NAME: &str = "outbound";
    let allow = tether_net::Allowlist::production();
    let endpoints = [
        ("ESI", tether_esi::ESI_BASE_URL.to_owned()),
        ("EVE SSO keys", tether_esi::jwt::CCP_JWKS_URL.to_owned()),
        ("EVE SSO", tether_esi::sso::SsoEndpoints::ccp().token_url),
        ("EVE SSO metadata", SSO_METADATA_URL.to_owned()),
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
                "{} set, but Tether ignores proxy settings and connects directly",
                proxies.join(", ")
            ),
            "Allow direct outbound HTTPS to the hosts Tether contacts, and unset the proxy variables in the app's environment.",
        );
    }
    let hosts: Vec<&str> = allow.hosts().collect();
    let certificates = match proxy {
        Proxy::Caddy => "and Caddy talks to Let's Encrypt".to_owned(),
        other => format!(
            "and TLS certificates are {}'s business, outside Tether",
            other.name()
        ),
    };
    Check::ok(
        NAME,
        format!(
            "the server only contacts {}; browsers also load images.evetech.net, {certificates}",
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
