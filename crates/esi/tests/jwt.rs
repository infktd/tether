#![allow(clippy::unwrap_used, clippy::expect_used)] // test code

use std::time::{SystemTime, UNIX_EPOCH};

use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use serde_json::{Value, json};
use tether_esi::jwt::{JwtError, JwtVerifier};
use tether_net::Allowlist;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const CLIENT: &str = "0123456789abcdef0123456789abcdef";
const RSA_KID: &str = "JWT-Signature-Key";
const EC_KID: &str = "8878a23f-b40b-4cda-8e1e-2a1c7d2b0e3e";

fn fixture(name: &str) -> String {
    std::fs::read_to_string(format!(
        "{}/tests/fixtures/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// Claims shaped like CCP's, valid unless overridden.
fn claims(overrides: Value) -> Value {
    let mut c = json!({
        "scp": ["esi-skills.read_skills.v1", "esi-wallet.read_character_wallet.v1"],
        "jti": "4b3b1a9e-0000-0000-0000-000000000000",
        "kid": RSA_KID,
        "sub": "CHARACTER:EVE:2118174283",
        "azp": CLIENT,
        "tenant": "tranquility",
        "tier": "live",
        "region": "world",
        "aud": [CLIENT, "EVE Online"],
        "name": "Unpercieved",
        "owner": "Ld8qQh5nEXAMPLEownerHASH=",
        "exp": now() + 1200,
        "iat": now(),
        "iss": "https://login.eveonline.com"
    });
    for (k, v) in overrides.as_object().unwrap() {
        if v.is_null() {
            c.as_object_mut().unwrap().remove(k);
        } else {
            c[k] = v.clone();
        }
    }
    c
}

fn sign_rsa(claims: &Value, kid: &str) -> String {
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some(kid.into());
    encode(
        &header,
        claims,
        &EncodingKey::from_rsa_pem(fixture("test_rsa.pem").as_bytes()).unwrap(),
    )
    .unwrap()
}

fn sign_ec(claims: &Value) -> String {
    let mut header = Header::new(Algorithm::ES256);
    header.kid = Some(EC_KID.into());
    encode(
        &header,
        claims,
        &EncodingKey::from_ec_pem(fixture("test_ec.pem").as_bytes()).unwrap(),
    )
    .unwrap()
}

async fn ccp() -> (MockServer, JwtVerifier) {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/oauth/jwks"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(fixture("jwks.json"), "application/json"),
        )
        .mount(&server)
        .await;
    let verifier = JwtVerifier::new(
        outbound(Allowlist::production().with_local(&server.address().to_string())),
        format!("{}/oauth/jwks", server.uri()),
    )
    .unwrap();
    (server, verifier)
}

#[tokio::test]
async fn accepts_valid_rs256_and_es256_tokens() {
    let (_ccp, verifier) = ccp().await;

    let rsa = verifier
        .verify(&sign_rsa(&claims(json!({})), RSA_KID), CLIENT)
        .await
        .unwrap();
    assert_eq!(rsa.character_id, 2118174283);
    assert_eq!(rsa.character_name, "Unpercieved");
    assert_eq!(rsa.owner_hash, "Ld8qQh5nEXAMPLEownerHASH=");
    assert_eq!(
        rsa.scopes,
        [
            "esi-skills.read_skills.v1",
            "esi-wallet.read_character_wallet.v1"
        ]
    );

    let ec = verifier
        .verify(
            &sign_ec(&claims(json!({"iss": "login.eveonline.com"}))),
            CLIENT,
        )
        .await
        .unwrap();
    assert_eq!(ec.character_id, 2118174283);
}

#[tokio::test]
async fn scopes_come_as_a_string_an_array_or_not_at_all() {
    let (_ccp, verifier) = ccp().await;
    let one = verifier
        .verify(
            &sign_rsa(
                &claims(json!({"scp": "esi-skills.read_skills.v1"})),
                RSA_KID,
            ),
            CLIENT,
        )
        .await
        .unwrap();
    assert_eq!(one.scopes, ["esi-skills.read_skills.v1"]);
    let none = verifier
        .verify(&sign_rsa(&claims(json!({"scp": null})), RSA_KID), CLIENT)
        .await
        .unwrap();
    assert!(none.scopes.is_empty());
}

#[tokio::test]
async fn rejects_tokens_that_are_not_for_us() {
    let (_ccp, verifier) = ccp().await;
    let check = |overrides: Value| {
        let token = sign_rsa(&claims(overrides), RSA_KID);
        let verifier = &verifier;
        async move { verifier.verify(&token, CLIENT).await.unwrap_err() }
    };

    assert!(matches!(
        check(json!({"aud": ["someone-else", "EVE Online"]})).await,
        JwtError::Invalid(_)
    ));
    assert!(matches!(
        check(json!({"aud": CLIENT})).await,
        JwtError::NotEveAudience
    ));
    assert!(matches!(
        check(json!({"iss": "https://evil.example"})).await,
        JwtError::Invalid(_)
    ));
    assert!(matches!(
        check(json!({"exp": now() - 3600})).await,
        JwtError::Invalid(_)
    ));
    assert!(matches!(
        check(json!({"sub": "CORPORATION:EVE:1"})).await,
        JwtError::NotACharacter(_)
    ));
    assert!(matches!(
        check(json!({"exp": null})).await,
        JwtError::Invalid(_)
    ));
}

#[tokio::test]
async fn rejects_forged_and_algorithm_swapped_tokens() {
    let (_ccp, verifier) = ccp().await;

    // Signed with the RSA key but claiming to be the EC key's.
    let wrong_key = sign_rsa(&claims(json!({})), EC_KID);
    assert!(verifier.verify(&wrong_key, CLIENT).await.is_err());

    // A valid token with one byte of the signature changed.
    let mut forged = sign_rsa(&claims(json!({})), RSA_KID).into_bytes();
    let last = forged.len() - 2;
    forged[last] = if forged[last] == b'A' { b'B' } else { b'A' };
    let forged = String::from_utf8(forged).unwrap();
    assert!(matches!(
        verifier.verify(&forged, CLIENT).await,
        Err(JwtError::Invalid(_))
    ));

    // alg "none", unsigned.
    use base64_url_encode as b64;
    let none = format!(
        "{}.{}.",
        b64(br#"{"alg":"none","kid":"JWT-Signature-Key","typ":"JWT"}"#),
        b64(claims(json!({})).to_string().as_bytes())
    );
    assert!(verifier.verify(&none, CLIENT).await.is_err());
}

#[tokio::test]
async fn unknown_keys_trigger_one_refetch_then_fail() {
    let (ccp, verifier) = ccp().await;
    verifier
        .verify(&sign_rsa(&claims(json!({})), RSA_KID), CLIENT)
        .await
        .unwrap();

    let err = verifier
        .verify(&sign_rsa(&claims(json!({})), "rotated-away"), CLIENT)
        .await
        .unwrap_err();
    assert!(matches!(err, JwtError::UnknownKey(_)), "{err:?}");
    // Keys were just fetched, so the unknown kid doesn't cause another
    // request.
    let requests = ccp.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
}

#[tokio::test]
async fn jwks_outage_is_reported() {
    let verifier = JwtVerifier::new(
        outbound(Allowlist::production().with_local("127.0.0.1:1")),
        "http://127.0.0.1:1/oauth/jwks",
    )
    .unwrap();
    let err = verifier
        .verify(&sign_rsa(&claims(json!({})), RSA_KID), CLIENT)
        .await
        .unwrap_err();
    assert!(matches!(err, JwtError::Keys(_)), "{err:?}");
}

/// Minimal unpadded base64url, to hand-build an unsigned token.
fn base64_url_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..=chunk.len() {
            out.push(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize] as char);
        }
    }
    out
}

fn outbound(allow: Allowlist) -> tether_net::Outbound {
    tether_net::Outbound::new(allow, "tether tests", std::time::Duration::from_secs(5)).unwrap()
}

#[test]
fn the_jwks_url_must_be_allowed() {
    let err = JwtVerifier::new(
        outbound(Allowlist::production()),
        "https://evil.example/oauth/jwks",
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("isn't an allowed destination"),
        "{err}"
    );
}

// EveSso end to end, against a mock EVE SSO: the token exchange goes
// through Tether's allow-listed client, and the token it returns is
// verified with the keys above.

mod eve_sso {
    use super::*;
    use tether_core::Secret;
    use tether_esi::sso::{EveSso, Sso, SsoConfig, SsoEndpoints, SsoError};
    use wiremock::matchers::body_string_contains;

    fn config() -> SsoConfig {
        SsoConfig {
            client_id: CLIENT.into(),
            redirect_uri: "https://tether.test/auth/callback".into(),
        }
    }

    /// A mock SSO (token endpoint and keys on one server) and an EveSso
    /// pointed at it.
    async fn sso() -> (MockServer, EveSso) {
        let (server, verifier) = ccp().await;
        let sso = EveSso::with_endpoints(
            verifier,
            Allowlist::production().with_local(&server.address().to_string()),
            "tether tests",
            SsoEndpoints {
                authorize_url: format!("{}/v2/oauth/authorize", server.uri()),
                token_url: format!("{}/v2/oauth/token", server.uri()),
            },
        )
        .unwrap();
        (server, sso)
    }

    fn tokens(refresh: &str) -> Value {
        json!({
            "access_token": sign_rsa(&claims(json!({})), RSA_KID),
            "token_type": "Bearer",
            "expires_in": 1199,
            "refresh_token": refresh,
        })
    }

    #[tokio::test]
    async fn logs_in_and_refreshes_through_the_mock_sso() {
        let (server, sso) = sso().await;
        Mock::given(method("POST"))
            .and(path("/v2/oauth/token"))
            .and(body_string_contains("grant_type=authorization_code"))
            .and(body_string_contains("code=the-code"))
            .respond_with(ResponseTemplate::new(200).set_body_json(tokens("refresh-1")))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v2/oauth/token"))
            .and(body_string_contains("grant_type=refresh_token"))
            .and(body_string_contains("refresh_token=refresh-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(tokens("refresh-2")))
            .mount(&server)
            .await;

        let pending = sso
            .begin(&config(), &["esi-skills.read_skills.v1".into()])
            .unwrap();
        assert!(
            pending
                .authorize_url
                .starts_with(&format!("{}/v2/oauth/authorize?", server.uri())),
            "{}",
            pending.authorize_url
        );
        assert!(pending.authorize_url.contains("code_challenge="));
        assert!(pending.authorize_url.contains(&pending.state));

        let identity = sso
            .finish(&config(), "the-code".into(), pending.pkce_verifier)
            .await
            .unwrap();
        assert_eq!(identity.character_id, 2118174283);
        assert_eq!(identity.character_name, "Unpercieved");
        assert_eq!(
            identity.tokens.refresh_token.as_ref().unwrap().expose(),
            "refresh-1"
        );
        // The PKCE verifier went to the token endpoint, which checks it.
        let exchange = &server.received_requests().await.unwrap();
        let exchange = exchange
            .iter()
            .find(|r| r.url.path() == "/v2/oauth/token")
            .unwrap();
        assert!(
            String::from_utf8_lossy(&exchange.body).contains("code_verifier="),
            "no PKCE verifier sent"
        );

        let refreshed = sso
            .refresh(&config(), Secret::new("refresh-1".into()))
            .await
            .unwrap();
        assert_eq!(
            refreshed.refresh_token.as_ref().unwrap().expose(),
            "refresh-2"
        );
        assert_eq!(
            refreshed.owner_hash.as_deref(),
            Some("Ld8qQh5nEXAMPLEownerHASH=")
        );
    }

    #[tokio::test]
    async fn a_dead_refresh_token_is_a_revocation_and_an_outage_is_not() {
        let (server, sso) = sso().await;
        Mock::given(method("POST"))
            .and(path("/v2/oauth/token"))
            .and(body_string_contains("refresh_token=dead"))
            .respond_with(ResponseTemplate::new(400).set_body_json(json!({
                "error": "invalid_grant",
                "error_description": "Invalid refresh token. Token missing/expired."
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v2/oauth/token"))
            .and(body_string_contains("refresh_token=unlucky"))
            .respond_with(ResponseTemplate::new(502).set_body_string("<html>Bad gateway</html>"))
            .mount(&server)
            .await;

        let dead = sso
            .refresh(&config(), Secret::new("dead".into()))
            .await
            .unwrap_err();
        assert!(matches!(dead, SsoError::Revoked(_)), "{dead:?}");
        let outage = sso
            .refresh(&config(), Secret::new("unlucky".into()))
            .await
            .unwrap_err();
        assert!(matches!(outage, SsoError::Unavailable(_)), "{outage:?}");
    }

    #[tokio::test]
    async fn a_redirecting_token_endpoint_is_not_followed() {
        let (server, sso) = sso().await;
        let elsewhere = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v2/oauth/token"))
            .respond_with(
                ResponseTemplate::new(307)
                    .insert_header("Location", format!("{}/steal", elsewhere.uri())),
            )
            .mount(&server)
            .await;

        let err = sso
            .refresh(&config(), Secret::new("refresh-1".into()))
            .await
            .unwrap_err();
        assert!(matches!(err, SsoError::Unavailable(_)), "{err:?}");
        assert!(
            elsewhere.received_requests().await.unwrap().is_empty(),
            "the refresh token followed the redirect"
        );
    }

    #[test]
    fn the_token_endpoint_must_be_allowed() {
        let verifier = JwtVerifier::new(outbound(Allowlist::production()), CCP_JWKS).unwrap();
        let err = EveSso::with_endpoints(
            verifier,
            Allowlist::production(),
            "tether tests",
            SsoEndpoints {
                authorize_url: "https://login.eveonline.com/v2/oauth/authorize".into(),
                token_url: "https://evil.example/v2/oauth/token".into(),
            },
        )
        .unwrap_err();
        assert!(matches!(err, SsoError::Config(_)), "{err:?}");
        // CCP's own endpoints are on the list.
        let verifier = JwtVerifier::new(outbound(Allowlist::production()), CCP_JWKS).unwrap();
        EveSso::new(verifier, Allowlist::production(), "tether tests").unwrap();
    }

    const CCP_JWKS: &str = tether_esi::jwt::CCP_JWKS_URL;
}
