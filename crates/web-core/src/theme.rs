//! The accent colour (DESIGN.md: the signal): one per instance, signal
//! orange unless an admin picks another. Served as a small stylesheet
//! after the built one, since the CSP allows no inline styles.

use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::json;
use tether_db::accounts::AccountId;
use tether_db::audit::{self, Actor};

use crate::AppState;
use crate::error::AppError;

/// Signal orange (DESIGN.md).
pub const DEFAULT: &str = "#ff7a1a";

/// The suggestions, all of a similar lightness: signal orange first.
pub const PRESETS: &[(&str, &str)] = &[
    ("Signal orange", "#ff7a1a"),
    ("Amber", "#f59e0b"),
    ("Violet", "#a78bfa"),
    ("Emerald", "#34d399"),
];

/// The default's own colours, as `assets/app.css` defines them (tuned by
/// hand in the design): the soft fill, a notice's border and its fill.
const DESIGN: [(&str, &str); 4] = [
    ("--accent", DEFAULT),
    ("--accent-soft", "#2a1608"),
    ("--accent-line", "#5a3417"),
    ("--accent-wash", "#170f09"),
];

/// How much of another colour each of those is, mixed into the page
/// background: close to the design's own for signal orange.
const MIXES: [(&str, f64); 3] = [
    ("--accent-soft", 0.14),
    ("--accent-line", 0.35),
    ("--accent-wash", 0.06),
];

/// The page background (`--background`), which the accent must read on
/// and its other colours are mixed into.
const BACKGROUND: (u8, u8, u8) = (0x07, 0x09, 0x0c);

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

/// `share` of the colour mixed into the background.
fn mixed(color: (u8, u8, u8), share: f64) -> String {
    let mix = |a: u8, b: u8| {
        let mixed = f64::from(a) * share + f64::from(b) * (1.0 - share);
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

/// The signal's colours for `accent`: the design's own for the default,
/// else the accent and its others mixed from it (a badge's soft fill, a
/// notice's and the save bar's border, a notice's fill).
pub fn css(accent: &str) -> String {
    let color = rgb(accent).or_else(|| rgb(DEFAULT)).unwrap_or(BACKGROUND);
    let hex = format!("#{:02x}{:02x}{:02x}", color.0, color.1, color.2);
    let tokens: Vec<String> = if hex == DEFAULT {
        DESIGN
            .iter()
            .map(|(name, value)| format!("{name}:{value}"))
            .collect()
    } else {
        std::iter::once(format!("--accent:{hex}"))
            .chain(
                MIXES
                    .iter()
                    .map(|(name, share)| format!("{name}:{}", mixed(color, *share))),
            )
            .collect()
    };
    format!(":root,.dark{{{}}}\n", tokens.join(";"))
}

/// The instance's accent: the setting if it's valid, else signal orange.
pub async fn accent<'e>(executor: impl sqlx::PgExecutor<'e>) -> Result<String, sqlx::Error> {
    Ok(
        tether_db::settings::get_string(executor, tether_db::settings::THEME_ACCENT)
            .await?
            .filter(|c| rgb(c).is_some())
            .unwrap_or_else(|| DEFAULT.to_owned()),
    )
}

/// Sets the accent, as [`check`] gave it, audited.
pub async fn set_accent(
    conn: &mut sqlx::PgConnection,
    actor: AccountId,
    hex: &str,
) -> Result<(), sqlx::Error> {
    tether_db::settings::set(&mut *conn, tether_db::settings::THEME_ACCENT, json!(hex)).await?;
    audit::record(
        &mut *conn,
        Actor::Account(actor),
        "theme.accent",
        None,
        json!({ "accent": hex }),
    )
    .await?;
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
    let body = css(&accent);
    let etag = etag(&body);
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
        body,
    )
        .into_response()
}

/// The stylesheet's ETag, from what it says: a new colour, or new colours
/// for the same one (a Tether that derives more of them), is fetched anew.
fn etag(body: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(body.as_bytes());
    let hex: String = digest.iter().take(8).map(|b| format!("{b:02x}")).collect();
    format!("\"{hex}\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_is_the_designs_own() {
        assert_eq!(
            css(DEFAULT),
            ":root,.dark{--accent:#ff7a1a;--accent-soft:#2a1608;--accent-line:#5a3417;\
             --accent-wash:#170f09}\n"
        );
        // As assets/app.css defines them, background included.
        let app = include_str!("../../../assets/app.css");
        for (name, value) in DESIGN {
            assert!(app.contains(&format!("  {name}: {value};")), "{name}");
        }
        let (r, g, b) = BACKGROUND;
        assert!(app.contains(&format!("  --background: #{r:02x}{g:02x}{b:02x};")));
        for (_, preset) in PRESETS {
            assert!(check(preset).is_ok(), "{preset}");
        }
    }

    #[test]
    fn another_colour_brings_its_own_line_and_wash() {
        assert_eq!(
            css("#34d399"),
            ":root,.dark{--accent:#34d399;--accent-soft:#0d2520;--accent-line:#17503d;\
             --accent-wash:#0a1514}\n"
        );
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
