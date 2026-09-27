//! The accent colour (DESIGN.md): one per instance, amber unless an admin
//! picks another. Served as a small stylesheet after the built one, since
//! the CSP allows no inline styles.

use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::json;
use tether_db::accounts::AccountId;
use tether_db::audit::{self, Actor};

use crate::AppState;
use crate::error::AppError;

pub const DEFAULT: &str = "#f59e0b";

/// DESIGN.md's suggestions, all of a similar lightness.
pub const PRESETS: &[(&str, &str)] = &[
    ("Amber", "#f59e0b"),
    ("Orange", "#fb923c"),
    ("Violet", "#a78bfa"),
    ("Emerald", "#34d399"),
];

/// The page background, which the soft accent is mixed into.
const BACKGROUND: (u8, u8, u8) = (0x09, 0x09, 0x0b);

fn rgb(hex: &str) -> Option<(u8, u8, u8)> {
    let hex = hex.strip_prefix('#')?;
    if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let channel = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
    Some((channel(0)?, channel(2)?, channel(4)?))
}

fn luminance((r, g, b): (u8, u8, u8)) -> f64 {
    let linear = |c: u8| {
        let c = f64::from(c) / 255.0;
        if c <= 0.039_28 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b)
}

/// Checks an admin's colour: `#rrggbb`, light enough to read on the dark
/// background (and for dark text on it: WCAG AA, 4.5:1). Lowercased.
pub fn check(hex: &str) -> Result<String, AppError> {
    let hex = hex.trim().to_ascii_lowercase();
    let color =
        rgb(&hex).ok_or_else(|| AppError::bad_request("A colour is # and six hex digits."))?;
    let contrast = (luminance(color) + 0.05) / (luminance(BACKGROUND) + 0.05);
    if contrast < 4.5 {
        return Err(AppError::bad_request(
            "That colour is too dark to read on the dark background: pick a lighter one.",
        ));
    }
    Ok(hex)
}

/// The accent mixed 14% into the background, for accent badge fills.
fn soft(color: (u8, u8, u8)) -> String {
    let mix = |a: u8, b: u8| {
        let mixed = f64::from(a) * 0.14 + f64::from(b) * 0.86;
        // Two u8s mixed stay within 0..=255.
        mixed.round().clamp(0.0, 255.0) as u8
    };
    format!(
        "#{:02x}{:02x}{:02x}",
        mix(color.0, BACKGROUND.0),
        mix(color.1, BACKGROUND.1),
        mix(color.2, BACKGROUND.2)
    )
}

pub fn css(accent: &str) -> String {
    let color = rgb(accent).or_else(|| rgb(DEFAULT)).unwrap_or(BACKGROUND);
    format!(
        ":root,.dark{{--accent:#{:02x}{:02x}{:02x};--accent-soft:{}}}\n",
        color.0,
        color.1,
        color.2,
        soft(color)
    )
}

/// The instance's accent: the setting if it's valid, else amber.
pub async fn accent(db: &tether_db::PgPool) -> Result<String, sqlx::Error> {
    Ok(
        tether_db::settings::get_string(db, tether_db::settings::THEME_ACCENT)
            .await?
            .filter(|c| rgb(c).is_some())
            .unwrap_or_else(|| DEFAULT.to_owned()),
    )
}

pub async fn set_accent(state: &AppState, actor: AccountId, hex: &str) -> Result<(), AppError> {
    let hex = check(hex)?;
    let mut tx = state.db.begin().await?;
    tether_db::settings::set(&mut *tx, tether_db::settings::THEME_ACCENT, json!(hex)).await?;
    audit::record(
        &mut *tx,
        Actor::Account(actor),
        "theme.accent",
        None,
        json!({ "accent": hex }),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// `GET /theme.css`: public, as the sign-in page uses it too. Revalidated
/// on every load (a 304 while the colour is the same).
pub async fn stylesheet(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let accent = match accent(&state.db).await {
        Ok(accent) => accent,
        Err(err) => {
            tracing::warn!(error = %err, "reading the accent failed; serving the default");
            DEFAULT.to_owned()
        }
    };
    let etag = format!("\"{}\"", accent.trim_start_matches('#'));
    let Ok(etag_value) = HeaderValue::from_str(&etag) else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let cache = HeaderValue::from_static("public, no-cache");
    if headers
        .get(header::IF_NONE_MATCH)
        .is_some_and(|v| v.as_bytes() == etag.as_bytes())
    {
        return (
            StatusCode::NOT_MODIFIED,
            [(header::ETAG, etag_value), (header::CACHE_CONTROL, cache)],
        )
            .into_response();
    }
    (
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/css; charset=utf-8"),
            ),
            (header::CACHE_CONTROL, cache),
            (header::ETAG, etag_value),
        ],
        css(&accent),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_matches_design_md() {
        assert_eq!(
            css(DEFAULT),
            ":root,.dark{--accent:#f59e0b;--accent-soft:#2a1e0b}\n"
        );
        for (_, preset) in PRESETS {
            assert!(check(preset).is_ok(), "{preset}");
        }
    }

    #[test]
    fn colours_are_checked() {
        assert_eq!(check(" #34D399 ").unwrap(), "#34d399");
        assert!(check("#123").is_err());
        assert!(check("34d399").is_err());
        assert!(check("#1e3a8a").is_err(), "too dark");
        assert!(check("#ffffff").is_ok());
    }
}
