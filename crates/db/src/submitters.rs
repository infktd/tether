//! Accounts that submitted an app's forms (`core.plugin_submitters`), which
//! the app may notify by the reference it was given (Jay, 2026-10-07): one
//! random reference per account and app, telling nothing about the account.

use crate::PgPool;
use crate::accounts::AccountId;

/// How long after a pilot last posted one of an app's forms its reference
/// still reaches them.
pub const KEPT_DAYS: i32 = 365;

/// `account`'s reference for `plugin_id`, made the first time it's asked
/// for; each time marks them as having posted now.
pub async fn reference(
    pool: &PgPool,
    plugin_id: &str,
    account: AccountId,
) -> Result<String, sqlx::Error> {
    sqlx::query_scalar!(
        "INSERT INTO core.plugin_submitters (plugin_id, account_id) VALUES ($1, $2) \
         ON CONFLICT (plugin_id, account_id) DO UPDATE SET last_posted_at = now() \
         RETURNING reference",
        plugin_id,
        account.0,
    )
    .fetch_one(pool)
    .await
}

/// The account behind one of `plugin_id`'s references, if it's still there
/// and they posted one of its forms in the last [`KEPT_DAYS`]; never
/// another app's.
pub async fn account(
    pool: &PgPool,
    plugin_id: &str,
    reference: &str,
) -> Result<Option<AccountId>, sqlx::Error> {
    Ok(sqlx::query_scalar!(
        "SELECT account_id FROM core.plugin_submitters WHERE plugin_id = $1 AND reference = $2 \
         AND last_posted_at > now() - make_interval(days => $3)",
        plugin_id,
        reference,
        KEPT_DAYS,
    )
    .fetch_optional(pool)
    .await?
    .map(AccountId))
}

/// The characters now on the account behind one of `plugin_id`'s
/// references (main first), as [`account`] finds it; none for a reference
/// that reaches nobody. One query: an app may ask for many in one call.
pub async fn characters(
    pool: &PgPool,
    plugin_id: &str,
    reference: &str,
) -> Result<Vec<crate::plugin_esi::CharacterRow>, sqlx::Error> {
    sqlx::query_as!(
        crate::plugin_esi::CharacterRow,
        r#"
        SELECT c.id, c.name, c.corporation_id, c.alliance_id
        FROM core.plugin_submitters s
        JOIN core.accounts a ON a.id = s.account_id
        JOIN core.characters c ON c.account_id = s.account_id
        WHERE s.plugin_id = $1 AND s.reference = $2
          AND s.last_posted_at > now() - make_interval(days => $3)
        ORDER BY c.id = a.main_character_id DESC NULLS LAST, c.name
        "#,
        plugin_id,
        reference,
        KEPT_DAYS,
    )
    .fetch_all(pool)
    .await
}
