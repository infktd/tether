//! `/metrics`: an optional Prometheus endpoint, off unless
//! `METRICS_ENABLED` turns it on, and then only for a scraper sending
//! `Authorization: Bearer <METRICS_TOKEN>`. Anything else (off, no token,
//! a wrong one) gets the same 404 as an address that doesn't exist, so
//! nothing tells whether it's on. It isn't in the API docs, and every
//! response carries `X-Robots-Tag: noindex` like the rest.
//!
//! The text format is written by hand (no new crate). Only counts and
//! states of the instance: no account, character or state names, nothing
//! about any one person.

use std::fmt::Write as _;

use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use tether_core::{Secret, hash_token};
use tether_jobs::JobState;

use crate::AppState;
use crate::error::AppError;

/// The shortest token accepted: install-style random tokens are 64.
pub const MIN_TOKEN_LEN: usize = 32;

/// Whether `/metrics` answers, and to which token. Off by default.
#[derive(Default)]
pub struct Metrics {
    /// The SHA-256 of `METRICS_TOKEN` while on; the token itself isn't
    /// kept.
    token_hash: Option<Vec<u8>>,
}

impl std::fmt::Debug for Metrics {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Metrics")
            .field("enabled", &self.enabled())
            .finish()
    }
}

impl Metrics {
    /// Off: `/metrics` answers 404.
    pub fn off() -> Self {
        Self::default()
    }

    /// On, for scrapers sending `token`. At least [`MIN_TOKEN_LEN`]
    /// printable characters with no spaces, and not an access token's
    /// shape. The error never repeats the token.
    pub fn on(token: &Secret<String>) -> Result<Self, String> {
        let token = token.expose().trim();
        if token.len() < MIN_TOKEN_LEN {
            return Err(format!(
                "METRICS_TOKEN must be at least {MIN_TOKEN_LEN} characters (got {}); \
                 `openssl rand -hex 32` makes a suitable one",
                token.len()
            ));
        }
        if !token.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(
                "METRICS_TOKEN must be printable ASCII with no spaces (it goes in a header)"
                    .to_owned(),
            );
        }
        if token.starts_with(crate::auth::PAT_PREFIX) {
            return Err(
                "METRICS_TOKEN must not be a personal access token; make a new random one"
                    .to_owned(),
            );
        }
        Ok(Self {
            token_hash: Some(hash_token(token)),
        })
    }

    /// From the environment: `METRICS_ENABLED` (true, 1, yes or on; unset,
    /// empty, false, 0, no or off is off) and, when on, `METRICS_TOKEN`.
    /// Anything else is refused rather than guessed at.
    pub fn from_env() -> Result<Self, String> {
        let var = |name: &str| {
            std::env::var(name)
                .ok()
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
        };
        Self::from_values(
            var("METRICS_ENABLED").as_deref(),
            var("METRICS_TOKEN").map(Secret::new).as_ref(),
        )
    }

    /// [`Self::from_env`] with the values given.
    pub fn from_values(
        enabled: Option<&str>,
        token: Option<&Secret<String>>,
    ) -> Result<Self, String> {
        match enabled.map(str::to_ascii_lowercase).as_deref() {
            None | Some("false" | "0" | "no" | "off") => Ok(Self::off()),
            Some("true" | "1" | "yes" | "on") => match token {
                Some(token) => Self::on(token),
                None => Err("METRICS_ENABLED is on but METRICS_TOKEN isn't set; \
                     set one (`openssl rand -hex 32`) or turn metrics off"
                    .to_owned()),
            },
            Some(_) => Err("METRICS_ENABLED must be true or false".to_owned()),
        }
    }

    pub fn enabled(&self) -> bool {
        self.token_hash.is_some()
    }

    /// The request carries the token: compared as SHA-256 digests, in
    /// constant time.
    fn admits(&self, headers: &HeaderMap) -> bool {
        let Some(expected) = &self.token_hash else {
            return false;
        };
        let Some(presented) = headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
        else {
            return false;
        };
        constant_time_eq(&hash_token(presented.trim()), expected)
    }
}

/// Equal slices, in time that depends only on their length.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// `GET /metrics`
pub async fn endpoint(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if !state.metrics.admits(&headers) {
        if state.metrics.enabled() {
            tracing::info!("metrics: refused a request without the right token");
        }
        return crate::pages::not_found().await;
    }
    match render(&state).await {
        Ok(text) => {
            let mut response = (StatusCode::OK, text).into_response();
            let headers = response.headers_mut();
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/plain; version=0.0.4; charset=utf-8"),
            );
            headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            response
        }
        Err(err) => err.into_response(),
    }
}

/// The exposition text, written family by family.
#[derive(Default)]
struct Text(String);

impl Text {
    fn family(&mut self, name: &str, kind: &str, help: &str) {
        let _ = writeln!(self.0, "# HELP {name} {help}");
        let _ = writeln!(self.0, "# TYPE {name} {kind}");
    }

    fn sample(&mut self, name: &str, labels: &[(&str, &str)], value: impl std::fmt::Display) {
        self.0.push_str(name);
        if !labels.is_empty() {
            self.0.push('{');
            for (i, (key, value)) in labels.iter().enumerate() {
                if i > 0 {
                    self.0.push(',');
                }
                let _ = write!(self.0, "{key}=\"{}\"", escape(value));
            }
            self.0.push('}');
        }
        let _ = writeln!(self.0, " {value}");
    }

    /// A family of one unlabelled sample.
    fn single(&mut self, name: &str, kind: &str, help: &str, value: impl std::fmt::Display) {
        self.family(name, kind, help);
        self.sample(name, &[], value);
    }
}

/// A label value as the format wants it.
fn escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

fn flag(on: bool) -> u8 {
    u8::from(on)
}

async fn render(state: &AppState) -> Result<String, AppError> {
    let mut out = Text::default();

    let version = crate::updates::CURRENT;
    let revision = state.updater.revision.as_deref().unwrap_or("");
    out.family(
        "tether_build_info",
        "gauge",
        "Always 1: the running build's version and commit in its labels.",
    );
    out.sample(
        "tether_build_info",
        &[("version", version), ("revision", revision)],
        1,
    );

    out.family(
        "tether_jobs",
        "gauge",
        "Background jobs in the queue by state (dead: gave up after retrying).",
    );
    for (job_state, n) in tether_jobs::counts(&state.db).await? {
        out.sample("tether_jobs", &[("state", job_state.as_str())], n);
    }

    let (running, failed) = state.plugins.counts();
    out.family(
        "tether_apps",
        "gauge",
        "Enabled apps: running, or failed to load.",
    );
    out.sample("tether_apps", &[("status", "running")], running);
    out.sample("tether_apps", &[("status", "failed")], failed);

    let budget = state.esi.budget();
    if let Some(remain) = budget.error_remain {
        out.single(
            "tether_esi_error_limit_remain",
            "gauge",
            "Errors ESI still accepts in its current window.",
            remain,
        );
    }
    if let Some(reset) = budget.error_reset_in_secs {
        out.single(
            "tether_esi_error_limit_reset_seconds",
            "gauge",
            "Seconds until ESI's error window resets.",
            reset,
        );
    }
    if let Some(lowest) = budget.lowest_error_remain {
        out.single(
            "tether_esi_error_limit_lowest_remain",
            "gauge",
            "The fewest errors ESI had left at any point since Tether started.",
            lowest,
        );
    }
    out.single(
        "tether_esi_rate_limit_groups_held",
        "gauge",
        "ESI rate-limit groups waiting out a 429.",
        budget
            .groups
            .iter()
            .filter(|g| g.held_for_secs.is_some())
            .count(),
    );
    out.single(
        "tether_esi_throttled",
        "gauge",
        "1 while background syncs wait for ESI's budget (errors or rate limits low).",
        flag(!state.esi.has_room()),
    );
    out.family(
        "tether_esi_responses_total",
        "counter",
        "ESI responses since Tether started, by outcome.",
    );
    let c = &budget.counts;
    for (outcome, n) in [
        ("ok", c.ok),
        ("cached", c.cached),
        ("not_modified", c.not_modified),
        ("client_error", c.client_errors),
        ("server_error", c.server_errors),
        ("error_limited", c.error_limited),
        ("rate_limited", c.rate_limited),
        ("transport_error", c.transport_errors),
    ] {
        out.sample("tether_esi_responses_total", &[("outcome", outcome)], n);
    }

    let discord = crate::discord::is_configured(state).await?;
    let last_sync = tether_jobs::schedule::last_runs(&state.db)
        .await?
        .into_iter()
        .find(|run| run.schedule == crate::discord_sync::SYNC_ALL_JOB);
    out.single(
        "tether_discord_configured",
        "gauge",
        "1 when the Discord application and bot are set up.",
        flag(discord),
    );
    out.single(
        "tether_discord_sync_failing",
        "gauge",
        "1 when the last full Discord role sync gave up.",
        flag(
            discord
                && last_sync
                    .as_ref()
                    .is_some_and(|r| r.state == JobState::Dead),
        ),
    );
    if let Some(at) = last_sync
        .as_ref()
        .filter(|r| r.state == JobState::Succeeded)
        .and_then(|r| r.finished_at)
    {
        out.single(
            "tether_discord_last_sync_timestamp_seconds",
            "gauge",
            "When the last full Discord role sync finished (Unix time).",
            at.timestamp(),
        );
    }

    out.family(
        "tether_accounts",
        "gauge",
        "Active accounts by state (built-in states by kind, others as custom; by id, never name).",
    );
    for row in tether_db::metrics::accounts_by_state(&state.db).await? {
        let id = row.state_id.to_string();
        out.sample(
            "tether_accounts",
            &[
                ("state_id", &id),
                ("kind", row.builtin.as_deref().unwrap_or("custom")),
            ],
            row.accounts,
        );
    }
    out.single(
        "tether_accounts_deactivated",
        "gauge",
        "Deactivated accounts.",
        tether_db::metrics::deactivated_accounts(&state.db).await?,
    );

    let size = state.db.size();
    let idle = u32::try_from(state.db.num_idle()).unwrap_or(u32::MAX);
    out.family(
        "tether_db_pool_connections",
        "gauge",
        "The core database pool's open connections, in use or idle.",
    );
    out.sample(
        "tether_db_pool_connections",
        &[("state", "in_use")],
        size.saturating_sub(idle),
    );
    out.sample("tether_db_pool_connections", &[("state", "idle")], idle);
    out.single(
        "tether_db_pool_max_connections",
        "gauge",
        "The core database pool's size limit (DATABASE_MAX_CONNECTIONS).",
        state.db.options().get_max_connections(),
    );

    Ok(out.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn off_unless_turned_on_with_a_good_token() {
        let good = Secret::new("a".repeat(64));
        for enabled in [None, Some("false"), Some("0"), Some("OFF"), Some("no")] {
            assert!(
                !Metrics::from_values(enabled, Some(&good))
                    .unwrap()
                    .enabled()
            );
        }
        for enabled in ["true", "1", "Yes", "on"] {
            assert!(
                Metrics::from_values(Some(enabled), Some(&good))
                    .unwrap()
                    .enabled()
            );
        }
        assert!(Metrics::from_values(Some("maybe"), Some(&good)).is_err());
        assert!(Metrics::from_values(Some("true"), None).is_err());
        for bad in [
            "hunter2".to_owned(),
            format!("{}hunter2 with spaces", "x".repeat(32)),
            format!("tether_pat_{}", "0".repeat(64)),
        ] {
            let err =
                Metrics::from_values(Some("true"), Some(&Secret::new(bad.clone()))).unwrap_err();
            assert!(!err.contains("hunter2"), "never echoed: {err}");
            assert!(!err.contains("0000"), "never echoed: {err}");
        }
        assert!(!format!("{:?}", Metrics::on(&good).unwrap()).contains("aaaa"));
    }

    #[test]
    fn only_the_exact_bearer_token_is_admitted() {
        let metrics = Metrics::on(&Secret::new("s".repeat(40))).unwrap();
        let with = |value: &str| {
            let mut headers = HeaderMap::new();
            headers.insert(header::AUTHORIZATION, value.parse().unwrap());
            headers
        };
        assert!(metrics.admits(&with(&format!("Bearer {}", "s".repeat(40)))));
        assert!(!metrics.admits(&with(&format!("Bearer {}", "s".repeat(39)))));
        assert!(!metrics.admits(&with(&format!("Bearer {}t", "s".repeat(40)))));
        assert!(!metrics.admits(&with(&format!("Basic {}", "s".repeat(40)))));
        assert!(!metrics.admits(&HeaderMap::new()));
        assert!(!Metrics::off().admits(&with(&format!("Bearer {}", "s".repeat(40)))));
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
    }

    #[test]
    fn label_values_are_escaped() {
        let mut text = Text::default();
        text.sample("m", &[("a", "x\"y\\z\nw")], 1);
        assert_eq!(text.0, "m{a=\"x\\\"y\\\\z\\nw\"} 1\n");
    }
}
