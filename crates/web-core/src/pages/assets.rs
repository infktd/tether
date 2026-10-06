//! Static assets, embedded in the binary: the built stylesheet, htmx and
//! the Archivo and IBM Plex Mono fonts. Nothing is fetched from a CDN at runtime.
//!
//! Debug builds read the stylesheet and Tether's own scripts from the
//! checkout on every request instead, so a change to them shows on a
//! reload without rebuilding the server (`scripts/css.sh --watch` keeps
//! the stylesheet built). Release builds only ever serve what they embed.

use std::sync::OnceLock;

use axum::body::Body;
use axum::extract::Path;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use sha2::{Digest, Sha256};

struct Asset {
    path: &'static str,
    content_type: &'static str,
    /// Fonts never change under the same name; everything else revalidates.
    immutable: bool,
    bytes: &'static [u8],
    /// Where it lives in the checkout, for debug builds to read it fresh
    /// (`None`: always the embedded copy).
    source: Option<&'static str>,
}

const ASSETS: &[Asset] = &[
    Asset {
        path: "app.css",
        content_type: "text/css; charset=utf-8",
        immutable: false,
        bytes: include_bytes!("../../../../static/app.css"),
        source: Some("static/app.css"),
    },
    Asset {
        path: "notifications.js",
        content_type: "text/javascript; charset=utf-8",
        immutable: false,
        bytes: include_bytes!("../../../../assets/notifications.js"),
        source: Some("assets/notifications.js"),
    },
    Asset {
        path: "live.js",
        content_type: "text/javascript; charset=utf-8",
        immutable: false,
        bytes: include_bytes!("../../../../assets/live.js"),
        source: Some("assets/live.js"),
    },
    Asset {
        path: "htmx.min.js",
        content_type: "text/javascript; charset=utf-8",
        immutable: false,
        bytes: include_bytes!("../../../../assets/vendor/htmx/htmx.min.js"),
        source: None,
    },
    Asset {
        path: "fonts/Archivo-Variable.woff2",
        content_type: "font/woff2",
        immutable: true,
        bytes: include_bytes!("../../../../assets/vendor/fonts/Archivo-Variable.woff2"),
        source: None,
    },
    Asset {
        path: "fonts/IBMPlexMono-Variable.woff2",
        content_type: "font/woff2",
        immutable: true,
        bytes: include_bytes!("../../../../assets/vendor/fonts/IBMPlexMono-Variable.woff2"),
        source: None,
    },
];

fn etag_of(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let short: String = digest[..8].iter().map(|b| format!("{b:02x}")).collect();
    format!("\"{short}\"")
}

fn etag(index: usize) -> &'static str {
    static ETAGS: OnceLock<Vec<String>> = OnceLock::new();
    let etags = ETAGS.get_or_init(|| ASSETS.iter().map(|a| etag_of(a.bytes)).collect());
    &etags[index]
}

/// A debug build's fresh copy of an asset from the checkout, if it has
/// one and it can be read.
#[cfg(debug_assertions)]
fn fresh(asset: &Asset) -> Option<Vec<u8>> {
    let source = asset.source?;
    std::fs::read(format!("{}/../../{source}", env!("CARGO_MANIFEST_DIR"))).ok()
}

#[cfg(not(debug_assertions))]
fn fresh(_asset: &Asset) -> Option<Vec<u8>> {
    None
}

/// `GET /static/{*path}`
pub async fn serve(Path(path): Path<String>, headers: HeaderMap) -> Response {
    let Some((index, asset)) = ASSETS.iter().enumerate().find(|(_, a)| a.path == path) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let fresh = fresh(asset);
    let etag = match &fresh {
        Some(bytes) => etag_of(bytes),
        None => etag(index).to_owned(),
    };
    let etag = etag.as_str();
    let cache = if asset.immutable {
        "public, max-age=31536000, immutable"
    } else {
        "public, no-cache"
    };
    if headers
        .get(header::IF_NONE_MATCH)
        .is_some_and(|v| v.as_bytes() == etag.as_bytes())
    {
        return (
            StatusCode::NOT_MODIFIED,
            [(header::ETAG, etag), (header::CACHE_CONTROL, cache)],
        )
            .into_response();
    }
    (
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static(asset.content_type),
            ),
            (header::CACHE_CONTROL, HeaderValue::from_static(cache)),
            (
                header::ETAG,
                HeaderValue::from_str(etag).unwrap_or(HeaderValue::from_static("\"0\"")),
            ),
        ],
        // The embedded copy is served without copying it.
        fresh.map_or_else(|| Body::from(asset.bytes), Body::from),
    )
        .into_response()
}
