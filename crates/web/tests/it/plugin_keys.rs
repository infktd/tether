//! Pinned plugin publisher keys: first install, rotation and admin re-pin,
//! each audited, none able to overwrite a pin that changed meanwhile.

use axum::http::StatusCode;
use sqlx::PgPool;
use tether_db::audit::Actor;
use tether_plugins::package::{self, Trust};
use tether_plugins::testing::{self, Key};
use tether_web::plugins::{record_trust, repin_key};

const ID: &str = "nmu.test";

/// Installs a package signed by `key` as the install flow will: lock the
/// pin, verify against it, record the trust. `pinned_seen` stands in for
/// the pin as read by an install that checked earlier (a race).
async fn install_seeing(
    db: &PgPool,
    key: &Key,
    extra: &[(&str, &[u8])],
    pinned_seen: Option<String>,
) -> Result<Trust, StatusCode> {
    let (bytes, signature) = testing::package(ID, key, extra);
    let verified = package::read(&bytes)
        .unwrap()
        .verify(&signature, pinned_seen.as_deref())
        .map_err(|_| StatusCode::UNPROCESSABLE_ENTITY)?;
    let mut tx = db.begin().await.unwrap();
    match record_trust(&mut tx, Actor::System, &verified).await {
        Ok(()) => {
            tx.commit().await.unwrap();
            Ok(verified.trust().clone())
        }
        Err(e) => Err(e.status()),
    }
}

async fn install(db: &PgPool, key: &Key, extra: &[(&str, &[u8])]) -> Result<Trust, StatusCode> {
    let pinned = tether_db::plugin_keys::get(db, ID).await.unwrap();
    install_seeing(db, key, extra, pinned).await
}

/// The rotation files for moving from `old` to `new`.
fn rotation(old: &Key, new: &Key) -> (String, String) {
    let statement = package::rotation_statement(ID, &old.public(), &new.public());
    let signature = old.sign(statement.as_bytes());
    (statement, signature)
}

async fn pinned(db: &PgPool) -> Option<(String, String)> {
    sqlx::query_as("SELECT public_key, pinned_by FROM core.plugin_keys WHERE plugin_id = $1")
        .bind(ID)
        .fetch_optional(db)
        .await
        .unwrap()
}

async fn audited(db: &PgPool) -> Vec<(String, serde_json::Value)> {
    sqlx::query_as("SELECT action, details FROM core.audit_log WHERE target = $1 ORDER BY id")
        .bind(format!("plugin:{ID}"))
        .fetch_all(db)
        .await
        .unwrap()
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn first_install_pins_once(db: PgPool) {
    let (a, b) = (Key::new(1), Key::new(2));
    assert_eq!(install(&db, &a, &[]).await, Ok(Trust::FirstInstall));
    assert_eq!(
        pinned(&db).await,
        Some((a.public(), "first_install".to_owned()))
    );
    assert_eq!(
        audited(&db).await,
        [(
            "plugin.key_pinned".to_owned(),
            serde_json::json!({ "key": a.public() })
        )]
    );

    // Another publisher's package under the same id is refused...
    assert_eq!(
        install(&db, &b, &[]).await,
        Err(StatusCode::UNPROCESSABLE_ENTITY)
    );
    // ...including one checked before the first install committed.
    assert_eq!(
        install_seeing(&db, &b, &[], None).await,
        Err(StatusCode::BAD_REQUEST)
    );
    assert_eq!(pinned(&db).await.unwrap().0, a.public());

    assert_eq!(install(&db, &a, &[]).await, Ok(Trust::Pinned));
    assert_eq!(audited(&db).await.len(), 1, "an upgrade isn't a key change");
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn rotation_moves_the_pin_only_from_the_key_it_names(db: PgPool) {
    let (a, b, c) = (Key::new(1), Key::new(2), Key::new(3));
    install(&db, &a, &[]).await.unwrap();

    let (statement, signature) = rotation(&a, &b);
    let files: &[(&str, &[u8])] = &[
        ("rotation.txt", statement.as_bytes()),
        ("rotation.txt.minisig", signature.as_bytes()),
    ];
    assert_eq!(
        install(&db, &b, files).await,
        Ok(Trust::Rotated { from: a.public() })
    );
    assert_eq!(pinned(&db).await, Some((b.public(), "rotation".to_owned())));

    // A rotation A endorsed, checked while A was still pinned, can't move
    // the pin once it is B.
    let (statement, signature) = rotation(&a, &c);
    let files: &[(&str, &[u8])] = &[
        ("rotation.txt", statement.as_bytes()),
        ("rotation.txt.minisig", signature.as_bytes()),
    ];
    assert_eq!(
        install_seeing(&db, &c, files, Some(a.public())).await,
        Err(StatusCode::BAD_REQUEST)
    );
    assert_eq!(pinned(&db).await.unwrap().0, b.public());
    let log = audited(&db).await;
    assert_eq!(log.len(), 2);
    assert_eq!(log[1].0, "plugin.key_rotated");
    assert_eq!(
        log[1].1,
        serde_json::json!({ "old": a.public(), "new": b.public() })
    );
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn admins_can_repin_after_confirming(db: PgPool) {
    let (a, b, c) = (Key::new(1), Key::new(2), Key::new(3));
    let (a_key, b_key, c_key) = (a.public(), b.public(), c.public());

    // Nothing pinned yet.
    let err = repin_key(&db, Actor::System, ID, &a_key, &b_key, ID)
        .await
        .unwrap_err();
    assert_eq!(err.status(), StatusCode::NOT_FOUND);

    install(&db, &a, &[]).await.unwrap();
    // The publisher lost key A and signs with B: refused until re-pinned.
    assert_eq!(
        install(&db, &b, &[]).await,
        Err(StatusCode::UNPROCESSABLE_ENTITY)
    );

    for (expected_old, new, confirmation) in [
        (a_key.as_str(), b_key.as_str(), "nmu.tes"), // confirmation doesn't match
        (&a_key, &b_key, "NMU.TEST"),                // exactly, not case-insensitively
        (&a_key, "not a key", ID),                   // not a key
        (&a_key, &a_key, ID),                        // already pinned
        (&c_key, &b_key, ID),                        // the admin looked at a different pin
    ] {
        let err = repin_key(&db, Actor::System, ID, expected_old, new, confirmation)
            .await
            .unwrap_err();
        assert_eq!(
            err.status(),
            StatusCode::BAD_REQUEST,
            "{confirmation} {new}"
        );
    }
    assert_eq!(pinned(&db).await.unwrap().0, a_key);

    repin_key(&db, Actor::System, ID, &a_key, &format!(" {b_key}\n"), ID)
        .await
        .unwrap();
    assert_eq!(pinned(&db).await, Some((b_key.clone(), "repin".to_owned())));
    let log = audited(&db).await;
    assert_eq!(log.last().unwrap().0, "plugin.key_repinned");
    assert_eq!(
        log.last().unwrap().1,
        serde_json::json!({ "old": a_key, "new": b_key })
    );

    // Now B's packages install, and A's are refused.
    assert_eq!(install(&db, &b, &[]).await, Ok(Trust::Pinned));
    assert_eq!(
        install(&db, &a, &[]).await,
        Err(StatusCode::UNPROCESSABLE_ENTITY)
    );
}
