#![allow(clippy::unwrap_used, clippy::expect_used)] // test code

//! The v1 host API end to end: real plugins built with the SDK, rendered
//! through the host, and their pages checked.

mod common;

use std::sync::{Arc, OnceLock};

use common::build_guest;
use tether_plugins::host::{Host, LoadedPlugin, Page, PageError, RenderError, Request, Section};
use tether_plugins::{CallError, PluginLimits, Runtime, RuntimeError};

fn component(package: &str) -> Vec<u8> {
    static HELLO: OnceLock<Vec<u8>> = OnceLock::new();
    static PAGES: OnceLock<Vec<u8>> = OnceLock::new();
    match package {
        "hello-plugin" => HELLO.get_or_init(|| build_guest(package)).clone(),
        "tether-plugins-test-guest-pages" => PAGES.get_or_init(|| build_guest(package)).clone(),
        other => build_guest(other),
    }
}

async fn load(package: &str) -> (Host, LoadedPlugin) {
    let host = Host::new(Arc::new(Runtime::new().unwrap().with_call_limit(8))).unwrap();
    let plugin = host.load(package, component(package)).await.unwrap();
    (host, plugin)
}

fn request(path: &str) -> Request {
    Request {
        path: path.to_owned(),
        query: Vec::new(),
    }
}

async fn render(host: &Host, plugin: &LoadedPlugin, path: &str) -> Result<Page, RenderError> {
    host.render(plugin, request(path), &PluginLimits::default())
        .await
        .map(|r| r.page)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_example_plugin_renders_its_pages() {
    let (host, hello) = load("hello-plugin").await;
    let rendered = host
        .render(&hello, request(""), &PluginLimits::default())
        .await
        .unwrap();
    assert_eq!(rendered.page.title, "Hello");
    assert_eq!(rendered.page.sections.len(), 3);
    assert!(matches!(rendered.page.sections[1], Section::Table(_)));
    assert_eq!(rendered.page.tabs.len(), 1);
    assert_eq!(rendered.logs.len(), 1);
    assert!(rendered.logs[0].message.contains("rendering"));

    assert_eq!(render(&host, &hello, "about").await.unwrap().title, "About");
    assert!(matches!(
        render(&host, &hello, "nope").await,
        Err(RenderError::Plugin(PageError::NotFound))
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bad_pages_are_refused_before_anyone_draws_them() {
    let (host, pages) = load("tether-plugins-test-guest-pages").await;
    assert_eq!(render(&host, &pages, "").await.unwrap().title, "Fine");

    let too_big = render(&host, &pages, "too-big").await.unwrap_err();
    assert!(
        matches!(&too_big, RenderError::Invalid(p) if p.0.contains("600 rows")),
        "{too_big:?}"
    );
    let bad_link = render(&host, &pages, "bad-link").await.unwrap_err();
    assert!(
        matches!(&bad_link, RenderError::Invalid(p) if p.0.contains("isn't a path")),
        "{bad_link:?}"
    );
    assert!(matches!(
        render(&host, &pages, "forbidden").await,
        Err(RenderError::Plugin(PageError::Forbidden))
    ));
    assert!(matches!(
        render(&host, &pages, "failed").await,
        Err(RenderError::Plugin(PageError::Failed(_)))
    ));
    assert!(matches!(
        render(&host, &pages, "crash").await,
        Err(RenderError::Call(CallError::Trap(_)))
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn logs_are_capped_and_printable() {
    let (host, pages) = load("tether-plugins-test-guest-pages").await;
    let rendered = host
        .render(&pages, request("chatty"), &PluginLimits::default())
        .await
        .unwrap();
    assert_eq!(rendered.logs.len(), tether_plugins::host::MAX_LOGS);
    assert!(
        rendered
            .logs
            .iter()
            .all(|l| !l.message.chars().any(char::is_control))
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_request_reaches_the_plugin() {
    let (host, pages) = load("tether-plugins-test-guest-pages").await;
    let page = host
        .render(
            &pages,
            Request {
                path: "query".to_owned(),
                query: vec![("sort".to_owned(), "value".to_owned())],
            },
            &PluginLimits::default(),
        )
        .await
        .unwrap()
        .page;
    assert_eq!(page.title, r#"[("sort", "value")]"#);
}

#[tokio::test]
async fn a_component_that_isnt_a_plugin_is_refused_at_load() {
    // The limits test guest exports its own world, not `render`.
    let host = Host::new(Arc::new(Runtime::new().unwrap())).unwrap();
    let err = host
        .load("not-a-plugin", component("tether-plugins-test-guest"))
        .await
        .unwrap_err();
    assert!(matches!(err, RuntimeError::Rejected(_)), "{err}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_plugin_cant_make_the_host_copy_too_much() {
    let (host, pages) = load("tether-plugins-test-guest-pages").await;
    // 20 MiB in one log call is over the host's per-call copy limit.
    let err = render(&host, &pages, "huge-log").await.unwrap_err();
    assert!(
        matches!(err, RenderError::Call(CallError::Trap(_))),
        "{err:?}"
    );
    // 10,500 values: over the page's value cap.
    let err = render(&host, &pages, "many-cells").await.unwrap_err();
    assert!(
        matches!(&err, RenderError::Invalid(p) if p.0.contains("values")),
        "{err:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_plugins_failure_text_is_bounded_and_clean() {
    let (host, pages) = load("tether-plugins-test-guest-pages").await;
    let Err(RenderError::Plugin(PageError::Failed(text))) =
        render(&host, &pages, "long-failure").await
    else {
        panic!("expected a failure");
    };
    assert_eq!(text.chars().count(), tether_plugins::host::MAX_LOG_TEXT);
    assert!(!text.contains('\u{202E}'));
}
