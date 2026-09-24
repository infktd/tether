#![allow(clippy::unwrap_used, clippy::expect_used)] // test code

//! The allow-list holds for real requests, including redirects.

use std::time::Duration;

use tether_net::{Allowlist, Outbound, OutboundError};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn outbound(allow: Allowlist) -> Outbound {
    Outbound::new(allow, "tether tests", Duration::from_secs(5)).unwrap()
}

#[tokio::test]
async fn requests_to_other_hosts_never_leave() {
    let net = outbound(Allowlist::production());
    assert!(matches!(
        net.get("https://example.com/"),
        Err(OutboundError::Blocked(_))
    ));
    assert!(matches!(
        net.get("http://169.254.169.254/latest/meta-data/"),
        Err(OutboundError::Blocked(_))
    ));
}

#[tokio::test]
async fn redirects_are_not_followed_by_default() {
    let first = MockServer::start().await;
    let second = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&second)
        .await;
    Mock::given(method("POST"))
        .and(path("/oauth2/token/revoke"))
        .respond_with(
            ResponseTemplate::new(307).insert_header("location", format!("{}/steal", second.uri())),
        )
        .mount(&first)
        .await;
    // Both are allowed, and the redirect still isn't followed: a 307
    // would re-send the body (a token) to the other host.
    let net = outbound(
        Allowlist::production()
            .with_local(&first.address().to_string())
            .with_local(&second.address().to_string()),
    );
    let response = net
        .post(&format!("{}/oauth2/token/revoke", first.uri()))
        .unwrap()
        .form(&[("token", "secret")])
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 307);
}

#[tokio::test]
async fn redirects_only_go_to_allowed_hosts() {
    let allowed = MockServer::start().await;
    let elsewhere = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&elsewhere)
        .await;
    Mock::given(method("GET"))
        .and(path("/away"))
        .respond_with(
            ResponseTemplate::new(302).insert_header("location", format!("{}/x", elsewhere.uri())),
        )
        .mount(&allowed)
        .await;
    Mock::given(method("GET"))
        .and(path("/here"))
        .respond_with(ResponseTemplate::new(302).insert_header("location", "/done"))
        .mount(&allowed)
        .await;
    Mock::given(method("GET"))
        .and(path("/done"))
        .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
        .mount(&allowed)
        .await;

    let net = Outbound::with_redirects(
        Allowlist::production().with_local(&allowed.address().to_string()),
        "tether tests",
        Duration::from_secs(5),
        tether_net::Redirects::WithinAllowlist,
    )
    .unwrap();
    let err = net
        .get(&format!("{}/away", allowed.uri()))
        .unwrap()
        .send()
        .await
        .unwrap_err();
    assert!(
        err.is_redirect() || err.to_string().contains("redirect"),
        "{err}"
    );

    let ok = net
        .get(&format!("{}/here", allowed.uri()))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(ok.text().await.unwrap(), "ok");
}
