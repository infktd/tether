//! Running the Postgres client tools: where they are, which version, and
//! how they connect (environment variables, never arguments: other users
//! on the host can read a process's arguments). They start with an empty
//! environment plus what [`BASE_ENV`] lets through, so they never see
//! `ENCRYPTION_KEY`, `SETUP_TOKEN` or `DATABASE_URL`.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use sqlx::postgres::{PgConnectOptions, PgSslMode};
use tether_core::Secret;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;

use crate::SnapshotError;

/// Where `pg_dump`, `pg_restore` and `psql` are: a directory, or `PATH`.
#[derive(Debug, Clone, Default)]
pub struct Tools {
    bin_dir: Option<PathBuf>,
}

impl Tools {
    pub fn new(bin_dir: Option<PathBuf>) -> Self {
        Self { bin_dir }
    }

    pub(crate) fn command(&self, tool: &'static str) -> Command {
        let program = match &self.bin_dir {
            Some(dir) => dir.join(tool),
            None => PathBuf::from(tool),
        };
        let mut command = clean_command(program);
        command.kill_on_drop(true);
        command
    }

    /// A tool's major version, from `--version`.
    pub async fn major_version(&self, tool: &'static str) -> Result<u32, SnapshotError> {
        let output = self
            .command(tool)
            .arg("--version")
            .stdin(Stdio::null())
            .output()
            .await
            .map_err(|e| spawn_error(tool, e))?;
        let text = String::from_utf8_lossy(&output.stdout);
        parse_major(&text).ok_or_else(|| SnapshotError::Tool {
            tool,
            detail: format!("unexpected version output {:?}", text.trim()),
        })
    }

    /// Checks every tool is there and of the server's major version:
    /// `pg_restore` and `psql` of another version can't be trusted to
    /// restore what this one dumped (a newer one's scripts don't run on
    /// Postgres 16, for one). Returns that version.
    pub async fn check(&self, server_major: u32) -> Result<u32, SnapshotError> {
        for tool in ["pg_dump", "pg_restore", "psql"] {
            let major = self.major_version(tool).await?;
            if major != server_major {
                return Err(SnapshotError::ToolVersion {
                    tool,
                    major,
                    server_major,
                });
            }
        }
        Ok(server_major)
    }
}

/// Inherited variables a child may keep: finding programs (Debian's
/// `pg_dump` is a Perl wrapper) and locale. `HOME` only locates tool
/// configuration; nothing secret lives in any of these.
const BASE_ENV: [&str; 6] = ["PATH", "HOME", "LANG", "LC_ALL", "LC_CTYPE", "TMPDIR"];

fn clean_command(program: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut command = Command::new(program);
    command.env_clear();
    for var in BASE_ENV {
        if let Some(value) = std::env::var_os(var) {
            command.env(var, value);
        }
    }
    command
}

pub(crate) fn spawn_error(tool: &'static str, err: std::io::Error) -> SnapshotError {
    if err.kind() == std::io::ErrorKind::NotFound {
        SnapshotError::ToolMissing(tool)
    } else {
        SnapshotError::io(format!("running {tool}"), err)
    }
}

/// `pg_dump (PostgreSQL) 16.15 (Debian 16.15-1.pgdg13+2)` gives 16.
fn parse_major(version: &str) -> Option<u32> {
    let rest = version.split_once("(PostgreSQL)")?.1.trim_start();
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// How the tools connect: the same database as `DATABASE_URL`.
#[derive(Clone)]
pub(crate) struct Conn {
    host: String,
    port: u16,
    user: String,
    database: String,
    password: Option<Secret<String>>,
    ssl_mode: &'static str,
}

impl std::fmt::Debug for Conn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Conn")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("user", &self.user)
            .field("database", &self.database)
            .finish_non_exhaustive()
    }
}

impl Conn {
    pub(crate) fn from_url(url: &Secret<String>) -> Result<Self, SnapshotError> {
        let options: PgConnectOptions = url.expose().parse().map_err(|_| SnapshotError::BadUrl)?;
        let host = match options.get_socket() {
            Some(dir) => dir.display().to_string(),
            None => options.get_host().to_owned(),
        };
        let user = options.get_username().to_owned();
        // The URL's TLS mode carries over. (Tether's sqlx is built without
        // TLS, so only modes that allow plaintext can connect at all.)
        let ssl_mode = match options.get_ssl_mode() {
            PgSslMode::Disable => "disable",
            PgSslMode::Allow => "allow",
            PgSslMode::Prefer => "prefer",
            PgSslMode::Require => "require",
            PgSslMode::VerifyCa => "verify-ca",
            PgSslMode::VerifyFull => "verify-full",
        };
        Ok(Self {
            host,
            port: options.get_port(),
            database: options.get_database().unwrap_or(&user).to_owned(),
            user,
            password: url_password(url.expose())?.map(Secret::new),
            ssl_mode,
        })
    }

    /// Points `command` at the database as `DATABASE_URL`'s user.
    pub(crate) fn apply(&self, command: &mut Command) {
        self.apply_as(command, &self.user, self.password.as_ref());
    }

    /// Points `command` at the database as another role (a plugin's).
    pub(crate) fn apply_as(
        &self,
        command: &mut Command,
        user: &str,
        password: Option<&Secret<String>>,
    ) {
        command
            .env("PGHOST", &self.host)
            .env("PGPORT", self.port.to_string())
            .env("PGUSER", user)
            .env("PGDATABASE", &self.database)
            .env("PGSSLMODE", self.ssl_mode)
            .env("PGAPPNAME", "tether snapshots")
            .env("PGCONNECT_TIMEOUT", "10");
        if let Some(password) = password {
            command.env("PGPASSWORD", password.expose());
        }
    }
}

/// The password in a `postgres://user:password@host/db` URL (or its
/// `password` query parameter), percent-decoded. sqlx parses the rest but
/// has no getter for this.
fn url_password(url: &str) -> Result<Option<String>, SnapshotError> {
    let Some((_, rest)) = url.split_once("://") else {
        return Ok(None);
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if let Some((userinfo, _)) = authority.rsplit_once('@')
        && let Some((_, password)) = userinfo.split_once(':')
    {
        return percent_decode(password).map(Some);
    }
    let query = rest.split_once('?').map(|(_, q)| q).unwrap_or_default();
    let query = query.split('#').next().unwrap_or_default();
    for pair in query.split('&') {
        if let Some(("password", value)) = pair.split_once('=') {
            return percent_decode(value).map(Some);
        }
    }
    Ok(None)
}

fn percent_decode(text: &str) -> Result<String, SnapshotError> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = text.get(i + 1..i + 3).ok_or(SnapshotError::BadUrl)?;
            out.push(u8::from_str_radix(hex, 16).map_err(|_| SnapshotError::BadUrl)?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).map_err(|_| SnapshotError::BadUrl)
}

/// Free bytes on the filesystem holding `dir`, from POSIX `df -Pk`
/// (statvfs needs unsafe code or a new crate).
pub async fn free_bytes(dir: &Path) -> Result<u64, SnapshotError> {
    let output = clean_command("df")
        .arg("-Pk")
        .arg(dir)
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|e| SnapshotError::io("running df", e))?;
    if !output.status.success() {
        return Err(SnapshotError::Tool {
            tool: "df",
            detail: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }
    parse_df(&String::from_utf8_lossy(&output.stdout)).ok_or_else(|| SnapshotError::Tool {
        tool: "df",
        detail: "unexpected output".to_owned(),
    })
}

/// The "Available" column (KiB) of `df -Pk`'s one line: the field before
/// the capacity percentage, counted from there because filesystem names
/// and mount points can contain spaces.
fn parse_df(output: &str) -> Option<u64> {
    let line = output.lines().nth(1)?;
    let fields: Vec<&str> = line.split_whitespace().collect();
    let capacity = fields
        .iter()
        .position(|f| f.ends_with('%') && f[..f.len() - 1].parse::<u64>().is_ok())?;
    let available: u64 = fields.get(capacity.checked_sub(1)?)?.parse().ok()?;
    available.checked_mul(1024)
}

/// The last few KiB of a tool's stderr, for the error when it fails. Read
/// to the end, so the tool never blocks on a full pipe.
pub(crate) async fn read_tail<R: AsyncRead + Unpin>(mut stderr: R) -> String {
    const KEEP: usize = 4096;
    let mut kept: Vec<u8> = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        match stderr.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                kept.extend_from_slice(&buf[..n]);
                if kept.len() > 2 * KEEP {
                    kept.drain(..kept.len() - KEEP);
                }
            }
        }
    }
    if kept.len() > KEEP {
        kept.drain(..kept.len() - KEEP);
    }
    String::from_utf8_lossy(&kept).trim().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_parse() {
        assert_eq!(
            parse_major("pg_dump (PostgreSQL) 16.15 (Debian 16.15-1.pgdg13+2)\n"),
            Some(16)
        );
        assert_eq!(parse_major("psql (PostgreSQL) 18.3\n"), Some(18));
        assert_eq!(parse_major("something else"), None);
    }

    #[test]
    fn passwords_come_from_the_url() {
        let pw = |url: &str| url_password(url).unwrap();
        assert_eq!(
            pw("postgres://tether:hunter2@db:5432/tether").as_deref(),
            Some("hunter2")
        );
        assert_eq!(
            pw("postgres://u:p%40ss%3Aw%2Fd@db/x").as_deref(),
            Some("p@ss:w/d")
        );
        assert_eq!(pw("postgres://u@db/x"), None);
        assert_eq!(
            pw("postgres://db/x?user=u&password=s%20t").as_deref(),
            Some("s t")
        );
        assert!(url_password("postgres://u:%zz@db/x").is_err());

        let conn = Conn::from_url(&Secret::new(
            "postgres://tether:hunter2@127.0.0.1:5433/tether".to_owned(),
        ))
        .unwrap();
        assert_eq!(conn.host, "127.0.0.1");
        assert_eq!(conn.port, 5433);
        assert_eq!(conn.user, "tether");
        assert_eq!(conn.database, "tether");
        assert_eq!(conn.ssl_mode, "prefer");
        assert!(!format!("{conn:?}").contains("hunter2"));
        let disabled = Conn::from_url(&Secret::new(
            "postgres://u:p@db/x?sslmode=disable".to_owned(),
        ))
        .unwrap();
        assert_eq!(disabled.ssl_mode, "disable");
    }

    #[tokio::test]
    async fn children_get_only_the_base_environment() {
        // Cargo gives the test process CARGO_* variables (and .env may
        // give it DATABASE_URL); none may reach the child.
        let mut command = clean_command("env");
        let conn = Conn::from_url(&Secret::new("postgres://u:hunter2@db/x".to_owned())).unwrap();
        conn.apply(&mut command);
        let out = command.output().await.unwrap();
        let env = String::from_utf8_lossy(&out.stdout);
        let names: Vec<&str> = env.lines().filter_map(|l| l.split('=').next()).collect();
        for name in &names {
            assert!(
                BASE_ENV.contains(name) || name.starts_with("PG"),
                "{name} leaked into a child's environment"
            );
        }
        assert!(env.contains("PGPASSWORD=hunter2"));
        assert!(!names.contains(&"DATABASE_URL") && !names.contains(&"CARGO_PKG_NAME"));
    }

    #[test]
    fn df_output_parses() {
        let linux = "Filesystem     1024-blocks     Used Available Capacity Mounted on\n\
                     overlay          954976684 31255640 875137212       4% /\n";
        assert_eq!(parse_df(linux), Some(875137212 * 1024));
        let spaces = "Filesystem 1024-blocks Used Available Capacity Mounted on\n\
                      map auto home 100 50 50 50% /System/Volumes/Data/home dir\n";
        assert_eq!(parse_df(spaces), Some(50 * 1024));
        assert_eq!(parse_df("nonsense"), None);
    }

    #[tokio::test]
    async fn df_reads_this_machine() {
        let free = free_bytes(&std::env::temp_dir()).await.unwrap();
        assert!(free > 0);
    }
}
