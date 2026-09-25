//! Static assets, embedded in the binary: the built stylesheet, htmx and
//! the Geist fonts. Nothing is fetched from a CDN at runtime.

use std::sync::OnceLock;

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
}

const ASSETS: &[Asset] = &[
    Asset {
        path: "app.css",
        content_type: "text/css; charset=utf-8",
        immutable: false,
        bytes: include_bytes!("../../../../static/app.css"),
    },
    Asset {
        path: "notifications.js",
        content_type: "text/javascript; charset=utf-8",
        immutable: false,
        bytes: include_bytes!("../../../../assets/notifications.js"),
    },
    Asset {
        path: "htmx.min.js",
        content_type: "text/javascript; charset=utf-8",
        immutable: false,
        bytes: include_bytes!("../../../../assets/vendor/htmx/htmx.min.js"),
    },
    Asset {
        path: "fonts/Geist-Variable.woff2",
        content_type: "font/woff2",
        immutable: true,
        bytes: include_bytes!("../../../../assets/vendor/fonts/Geist-Variable.woff2"),
    },
    Asset {
        path: "fonts/GeistMono-Variable.woff2",
        content_type: "font/woff2",
        immutable: true,
        bytes: include_bytes!("../../../../assets/vendor/fonts/GeistMono-Variable.woff2"),
    },
];

fn etag(index: usize) -> &'static str {
    static ETAGS: OnceLock<Vec<String>> = OnceLock::new();
    let etags = ETAGS.get_or_init(|| {
        ASSETS
            .iter()
            .map(|a| {
                let digest = Sha256::digest(a.bytes);
                let short: String = digest[..8].iter().map(|b| format!("{b:02x}")).collect();
                format!("\"{short}\"")
            })
            .collect()
    });
    &etags[index]
}

/// `GET /static/{*path}`
pub async fn serve(Path(path): Path<String>, headers: HeaderMap) -> Response {
    let Some((index, asset)) = ASSETS.iter().enumerate().find(|(_, a)| a.path == path) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let etag = etag(index);
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
            (header::ETAG, HeaderValue::from_static(etag)),
        ],
        asset.bytes,
    )
        .into_response()
}
