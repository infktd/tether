use crate::common::*;
use axum::http::StatusCode;
use sqlx::PgPool;

#[cfg(feature = "dev-login")]
#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn fixtures_sign_in_without_sso(db: PgPool) {
    let h = harness(db, false).await;

    let listed = send(&h.app, get("/dev/login", &[])).await;
    assert!(listed.body.contains("\"owner\""));

    let owner = send(&h.app, get("/dev/login/owner", &[])).await;
    assert_eq!(owner.status, StatusCode::SEE_OTHER);
    let owner = owner.cookie_value(SESSION);
    let me_owner = me(&h, &owner).await;
    assert_eq!(me_owner["is_owner"], true);
    assert_eq!(me_owner["main"]["name"], "Dev Owner");

    let member = send(&h.app, get("/dev/login/member", &[]))
        .await
        .cookie_value(SESSION);
    let me_member = me(&h, &member).await;
    assert_eq!(me_member["state"], "Member");
    assert_eq!(me_member["is_owner"], false);

    assert_eq!(
        send(&h.app, get("/dev/login/admiral", &[])).await.status,
        StatusCode::NOT_FOUND
    );
}

#[cfg(not(feature = "dev-login"))]
#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn dev_login_does_not_exist_without_the_feature(db: PgPool) {
    let h = harness(db, false).await;
    for uri in ["/dev/login", "/dev/login/owner"] {
        assert_eq!(
            send(&h.app, get(uri, &[])).await.status,
            StatusCode::NOT_FOUND
        );
    }
    const { assert!(!tether_web::DEV_LOGIN) };
}
