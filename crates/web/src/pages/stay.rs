//! Staying on the page (DESIGN.md, Page hygiene and state): after an
//! action the viewer is still on the same page, tab, query and scroll
//! position, with a toast saying it's done or why not.
//!
//! Handlers keep answering as they always have, so pages work without
//! JavaScript: a redirect back to the page, an error page, or a page. For
//! a boosted form post (htmx sends `HX-Boosted`), [`in_place`] turns that
//! answer into what htmx needs to stay put:
//!
//! - a redirect to a page of this site becomes `HX-Location`, which htmx
//!   follows without a full load. Back to the page the form was on (the
//!   same path), it keeps the page's own query (its tab, search and
//!   filters, from `HX-Current-URL`) and scroll position, and adds no
//!   history entry; to another page (a new group), it's a navigation;
//! - an error page becomes a problem toast, and what's shown stays;
//! - a page is swapped in place without touching the address (the post's
//!   own address isn't a page to go back to), keeping the scroll position,
//!   unless the handler marks it as a [`NewPage`] (a confirmation).
//!
//! A handler asks for a toast by putting a [`Toast`] in the response's
//! extensions ([`with_toast`], [`back`]); for any htmx request it goes out
//! as an `HX-Trigger` event that `assets/live.js` draws. Without htmx
//! there's no toast: the page itself says what it has to say.

use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Redirect, Response};

use crate::AppState;
use crate::auth::safe_path;
use crate::error::Problem;

/// A toast to show after an action.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Toast {
    message: String,
    problem: bool,
}

impl Toast {
    /// It worked: "Saved.", "Accepted Rifter Pilot."
    pub fn done(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            problem: false,
        }
    }

    /// It didn't, and why.
    pub fn problem(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            problem: true,
        }
    }

    pub fn is_problem(&self) -> bool {
        self.problem
    }

    /// The `HX-Trigger` value: `{"toast": {"message": ..., "tone": ...}}`,
    /// in ASCII (header values are bytes, which browsers read as Latin-1).
    fn trigger(&self) -> Option<HeaderValue> {
        let event = serde_json::json!({
            "toast": {
                "message": self.message,
                "tone": if self.problem { "problem" } else { "done" },
            }
        });
        HeaderValue::from_str(&ascii_json(&event)).ok()
    }
}

/// Marks a page answering a post as a page of its own (a confirmation to
/// read from the top), not the page the form was on shown again.
#[derive(Clone, Copy, Debug)]
pub struct NewPage;

/// Adds a toast to a response.
pub fn with_toast(mut response: Response, toast: Toast) -> Response {
    response.extensions_mut().insert(toast);
    response
}

/// Back to `to` (the page the form was on) with a toast saying it's done.
/// Without JavaScript it's the plain redirect it always was.
pub fn back(to: &str, message: impl Into<String>) -> Response {
    with_toast(Redirect::to(to).into_response(), Toast::done(message))
}

/// The message on the page (inline) for a browser without htmx, or as a
/// toast for one with it: never both.
pub fn notice(headers: &HeaderMap, message: &str) -> (Option<String>, Option<Toast>) {
    if super::is_htmx(headers) {
        (None, Some(Toast::done(message)))
    } else {
        (Some(message.to_owned()), None)
    }
}

/// JSON with everything outside ASCII escaped (`é`), so it fits in a
/// header value unchanged.
fn ascii_json(value: &serde_json::Value) -> String {
    let text = value.to_string();
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if c.is_ascii() {
            out.push(c);
        } else {
            let mut units = [0u16; 2];
            for unit in c.encode_utf16(&mut units) {
                out.push_str(&format!("\\u{unit:04x}"));
            }
        }
    }
    out
}

fn header_is(headers: &HeaderMap, name: &str, value: &str) -> bool {
    headers
        .get(name)
        .is_some_and(|v| v.as_bytes() == value.as_bytes())
}

/// The page the browser is on (htmx's `HX-Current-URL`), as a path with
/// its query, when it's this site and a safe path.
pub(crate) fn current_page(origin: &str, headers: &HeaderMap) -> Option<String> {
    let url = headers.get("hx-current-url")?.to_str().ok()?;
    let rest = url.strip_prefix(origin)?;
    let rest = rest.split('#').next().unwrap_or(rest);
    safe_path(rest).map(str::to_owned)
}

fn path_of(url: &str) -> &str {
    url.split('?').next().unwrap_or(url)
}

/// Where a redirect after an action should take the browser: the page it
/// was on, with that page's query, when the redirect is back to the same
/// page and names no query of its own; otherwise where the handler said.
pub(crate) fn destination(location: &str, current: Option<&str>) -> String {
    match current {
        Some(current) if !location.contains('?') && path_of(current) == location => {
            current.to_owned()
        }
        _ => location.to_owned(),
    }
}

/// `HX-Location` for a boosted post's redirect to `location`.
fn follow(location: &str, current: Option<&str>) -> Option<HeaderValue> {
    let to = destination(location, current);
    let same_page = current.is_some_and(|c| path_of(c) == path_of(&to));
    let spec = if same_page {
        serde_json::json!({
            "path": to,
            "target": "body",
            "swap": "innerHTML show:none",
            // Reloading the address that's shown adds no history entry.
            "push": if current == Some(to.as_str()) { "false" } else { "true" },
        })
    } else {
        serde_json::json!({ "path": to, "target": "body", "swap": "innerHTML show:top" })
    };
    HeaderValue::from_str(&ascii_json(&spec)).ok()
}

/// A 204 in place of `original`, keeping every header it set (cookies set
/// or cleared, `Retry-After`) but those of its body and its redirect.
fn no_content(
    original: &Response,
    headers: impl IntoIterator<Item = (&'static str, HeaderValue)>,
) -> Response {
    let mut response = StatusCode::NO_CONTENT.into_response();
    for (name, value) in original.headers() {
        if name != header::LOCATION
            && name != header::CONTENT_TYPE
            && name != header::CONTENT_LENGTH
        {
            response.headers_mut().append(name, value.clone());
        }
    }
    for (name, value) in headers {
        response.headers_mut().insert(name, value);
    }
    response
}

/// A path on this site that a script's request can't be sent to (logging
/// in): one leading `/`, nothing that a browser could read as another
/// host.
fn this_site(location: &str) -> bool {
    location.starts_with('/')
        && !location.contains("//")
        && !location.contains('\\')
        && location.bytes().all(|b| b.is_ascii_graphic())
}

/// What a boosted post's answer becomes (see the module docs), and the
/// toast it should carry.
fn stay(
    response: Response,
    current: Option<&str>,
    toast: Option<Toast>,
) -> (Response, Option<Toast>) {
    let status = response.status();
    if status.is_redirection() {
        let Some(location) = response
            .headers()
            .get(header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
        else {
            return (response, toast);
        };
        if let Some(path) = safe_path(&location) {
            return match follow(path, current) {
                Some(value) => (no_content(&response, [("hx-location", value)]), toast),
                None => (response, toast),
            };
        }
        // Logging in, or off this site (EVE's login): a navigation, which
        // a script's request can't follow.
        if this_site(&location)
            || location.starts_with("https://")
            || location.starts_with("http://")
        {
            return match HeaderValue::from_str(&location) {
                Ok(value) => (no_content(&response, [("hx-redirect", value)]), toast),
                Err(_) => (response, toast),
            };
        }
        return (response, toast);
    }
    if let Some(Problem(message)) = response.extensions().get::<Problem>().cloned()
        && status != StatusCode::UNAUTHORIZED
    {
        // What's shown stays; the toast says why nothing happened.
        return (no_content(&response, []), Some(Toast::problem(message)));
    }
    let html = response
        .headers()
        .get(header::CONTENT_TYPE)
        .is_some_and(|v| v.as_bytes().starts_with(b"text/html"));
    let mut response = response;
    if html && response.status() != StatusCode::NO_CONTENT {
        let new_page = response.extensions().get::<NewPage>().is_some();
        let headers = response.headers_mut();
        if !headers.contains_key("hx-reswap") {
            headers.insert(
                "hx-reswap",
                HeaderValue::from_static(if new_page {
                    "innerHTML show:top"
                } else {
                    "innerHTML show:none"
                }),
            );
        }
        if !headers.contains_key("hx-push-url") {
            headers.insert("hx-push-url", HeaderValue::from_static("false"));
        }
    }
    (response, toast)
}

/// The middleware (module docs). Innermost, so it sees handlers' answers
/// as they gave them.
pub async fn in_place(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let headers = request.headers();
    let htmx = super::is_htmx(headers);
    let boosted_post = htmx
        && header_is(headers, "hx-boosted", "true")
        && !matches!(
            *request.method(),
            Method::GET | Method::HEAD | Method::OPTIONS
        );
    let current = if boosted_post {
        current_page(state.site.origin(), headers)
    } else {
        None
    };
    let mut response = next.run(request).await;
    let toast = response.extensions_mut().remove::<Toast>();
    if !htmx {
        return response;
    }
    let (mut response, toast) = if boosted_post {
        stay(response, current.as_deref(), toast)
    } else {
        (response, toast)
    };
    if let Some(value) = toast.as_ref().and_then(Toast::trigger)
        && !response.headers().contains_key("hx-trigger")
    {
        response.headers_mut().insert("hx-trigger", value);
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toasts_are_ascii_json() {
        let toast = Toast::done("Accepted Élise \"Q\" · 3");
        let value = toast.trigger().unwrap();
        let text = value.to_str().unwrap();
        assert!(text.is_ascii());
        let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
        assert_eq!(parsed["toast"]["message"], "Accepted Élise \"Q\" · 3");
        assert_eq!(parsed["toast"]["tone"], "done");
        let problem = Toast::problem("No 😀").trigger().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(problem.to_str().unwrap()).unwrap();
        assert_eq!(parsed["toast"]["message"], "No 😀");
        assert_eq!(parsed["toast"]["tone"], "problem");
    }

    #[test]
    fn back_to_the_same_page_keeps_its_query() {
        let here = Some("/admin/users?q=pilot&page=2");
        assert_eq!(
            destination("/admin/users", here),
            "/admin/users?q=pilot&page=2"
        );
        // A query the handler chose wins, and so does another page.
        assert_eq!(destination("/admin/users?q=x", here), "/admin/users?q=x");
        assert_eq!(destination("/admin/users/5", here), "/admin/users/5");
        assert_eq!(destination("/admin/users", None), "/admin/users");
    }

    #[test]
    fn answers_in_place_keep_the_handlers_headers() {
        let mut original =
            Redirect::to("https://login.eveonline.com/v2/oauth/authorize").into_response();
        let headers = original.headers_mut();
        headers.append(header::SET_COOKIE, HeaderValue::from_static("a=1"));
        headers.append(header::SET_COOKIE, HeaderValue::from_static("b=2"));
        let (answer, _) = stay(original, Some("/register"), None);
        assert_eq!(answer.status(), StatusCode::NO_CONTENT);
        assert_eq!(
            answer.headers().get_all(header::SET_COOKIE).iter().count(),
            2
        );
        assert!(answer.headers().get(header::LOCATION).is_none());
        assert_eq!(
            answer.headers()["hx-redirect"],
            "https://login.eveonline.com/v2/oauth/authorize"
        );
        // Nothing a browser could read as another host goes out as this
        // site's.
        for location in ["/\\evil.test", "//evil.test", "/\\/evil"] {
            let (answer, _) = stay(Redirect::to(location).into_response(), None, None);
            assert!(answer.headers().get("hx-redirect").is_none(), "{location}");
            assert!(answer.headers().get("hx-location").is_none(), "{location}");
        }
        // Logging in is a navigation.
        let (answer, _) = stay(Redirect::to("/login").into_response(), None, None);
        assert_eq!(answer.headers()["hx-redirect"], "/login");
    }

    #[test]
    fn only_this_sites_safe_paths_count_as_the_current_page() {
        let mut headers = HeaderMap::new();
        let origin = "https://tether.test";
        headers.insert(
            "hx-current-url",
            HeaderValue::from_static("https://tether.test/groups?tab=1#x"),
        );
        assert_eq!(
            current_page(origin, &headers).as_deref(),
            Some("/groups?tab=1")
        );
        for bad in [
            "https://evil.test/groups",
            "https://tether.test.evil.test/groups",
            "https://tether.test//evil.test",
            "https://tether.test",
            "/groups",
        ] {
            headers.insert("hx-current-url", HeaderValue::from_str(bad).unwrap());
            assert_eq!(current_page(origin, &headers), None, "{bad}");
        }
    }
}
