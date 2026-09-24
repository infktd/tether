//! Accounts and their characters.

use crate::PgPool;

/// Serializes sign-ins so owner bootstrap and alt linking can't race.
/// Sign-ins are rare enough that a global lock costs nothing.
const SIGN_IN_LOCK: i64 = 0x7465_7468_6572_0001;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccountId(pub i64);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignIn {
    /// A known character: signed in to its account.
    Existing(AccountId),
    /// A new character added to the signed-in account.
    AddedAlt(AccountId),
    /// A new character with no session: a new account.
    Created(AccountId),
    /// The character already belongs to a different account than the one
    /// signed in. Nothing changed.
    LinkedElsewhere,
}

impl SignIn {
    /// The account the browser should be signed in to afterwards, if any.
    pub fn account(&self) -> Option<AccountId> {
        match self {
            Self::Existing(a) | Self::AddedAlt(a) | Self::Created(a) => Some(*a),
            Self::LinkedElsewhere => None,
        }
    }
}

/// Handles a successful SSO login by `character_id`, optionally while
/// already signed in to `current`.
///
/// With `claim_owner` (the browser holds a valid first-run setup session),
/// the resulting account becomes the owner if there is none yet. Returns
/// whether it did alongside the outcome.
///
/// TODO(milestone 1): detect character transfers (sold or moved to another
/// EVE account) via the SSO `owner` hash and unlink instead of refusing.
pub async fn sign_in(
    pool: &PgPool,
    character_id: i64,
    character_name: &str,
    current: Option<AccountId>,
    claim_owner: bool,
) -> Result<(SignIn, bool), sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query!("SELECT pg_advisory_xact_lock($1)", SIGN_IN_LOCK)
        .execute(&mut *tx)
        .await?;

    let existing = sqlx::query_scalar!(
        "SELECT account_id FROM core.characters WHERE id = $1",
        character_id
    )
    .fetch_optional(&mut *tx)
    .await?
    .map(AccountId);

    let outcome = match (existing, current) {
        (Some(owner), Some(current)) if owner != current => SignIn::LinkedElsewhere,
        (Some(account), _) => {
            sqlx::query!(
                "UPDATE core.characters SET name = $2, last_login_at = now() WHERE id = $1",
                character_id,
                character_name,
            )
            .execute(&mut *tx)
            .await?;
            SignIn::Existing(account)
        }
        (None, Some(account)) => {
            insert_character(&mut tx, account, character_id, character_name).await?;
            SignIn::AddedAlt(account)
        }
        (None, None) => {
            let id = sqlx::query_scalar!(
                "INSERT INTO core.accounts (main_character_id) VALUES ($1) RETURNING id",
                character_id,
            )
            .fetch_one(&mut *tx)
            .await?;
            let account = AccountId(id);
            insert_character(&mut tx, account, character_id, character_name).await?;
            SignIn::Created(account)
        }
    };
    let became_owner = match outcome.account() {
        Some(account) if claim_owner => {
            let claimed = sqlx::query!(
                r#"
                UPDATE core.accounts SET is_owner = true
                WHERE id = $1 AND NOT EXISTS (SELECT 1 FROM core.accounts WHERE is_owner)
                "#,
                account.0,
            )
            .execute(&mut *tx)
            .await?;
            claimed.rows_affected() == 1
        }
        _ => false,
    };
    tx.commit().await?;
    Ok((outcome, became_owner))
}

pub async fn owner_exists(pool: &PgPool) -> Result<bool, sqlx::Error> {
    let exists = sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM core.accounts WHERE is_owner) AS "exists!""#
    )
    .fetch_one(pool)
    .await?;
    Ok(exists)
}

pub async fn all_ids(pool: &PgPool) -> Result<Vec<AccountId>, sqlx::Error> {
    let ids = sqlx::query_scalar!("SELECT id FROM core.accounts ORDER BY id")
        .fetch_all(pool)
        .await?;
    Ok(ids.into_iter().map(AccountId).collect())
}

async fn insert_character(
    tx: &mut sqlx::PgTransaction<'_>,
    account: AccountId,
    character_id: i64,
    character_name: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        r#"
        INSERT INTO core.characters (id, account_id, name, last_login_at)
        VALUES ($1, $2, $3, now())
        "#,
        character_id,
        account.0,
        character_name,
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Character {
    pub id: i64,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    pub id: AccountId,
    pub is_owner: bool,
    pub main: Character,
    /// All characters including the main, main first, then by name.
    pub characters: Vec<Character>,
}

pub async fn get(pool: &PgPool, account: AccountId) -> Result<Option<Account>, sqlx::Error> {
    let rows = sqlx::query!(
        r#"
        SELECT a.is_owner, a.main_character_id, c.id, c.name
        FROM core.accounts a
        JOIN core.characters c ON c.account_id = a.id
        WHERE a.id = $1
        ORDER BY c.id = a.main_character_id DESC, c.name
        "#,
        account.0,
    )
    .fetch_all(pool)
    .await?;
    let Some(first) = rows.first() else {
        return Ok(None);
    };
    let (is_owner, main_id) = (first.is_owner, first.main_character_id);
    let characters: Vec<Character> = rows
        .into_iter()
        .map(|r| Character {
            id: r.id,
            name: r.name,
        })
        .collect();
    let main = characters
        .iter()
        .find(|c| c.id == main_id)
        .cloned()
        .ok_or_else(|| sqlx::Error::RowNotFound)?;
    Ok(Some(Account {
        id: account,
        is_owner,
        main,
        characters,
    }))
}

/// Makes one of the account's own characters its main. Returns false if the
/// character isn't on this account.
pub async fn set_main(
    pool: &PgPool,
    account: AccountId,
    character_id: i64,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query!(
        r#"
        UPDATE core.accounts SET main_character_id = $2
        WHERE id = $1
          AND EXISTS (SELECT 1 FROM core.characters WHERE id = $2 AND account_id = $1)
        "#,
        account.0,
        character_id,
    )
    .execute(pool)
    .await?;
    Ok(result.rows_affected() == 1)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountSummary {
    pub id: AccountId,
    pub main_name: String,
    pub tier: String,
    pub is_owner: bool,
    pub characters: i64,
    pub groups: Vec<String>,
}

/// Accounts for the admin CLI, owner first, then by main name.
pub async fn list_summaries(pool: &PgPool, limit: i64) -> Result<Vec<AccountSummary>, sqlx::Error> {
    let rows = sqlx::query!(
        r#"
        SELECT a.id, m.name AS main_name, a.tier, a.is_owner,
               (SELECT count(*) FROM core.characters c WHERE c.account_id = a.id) AS "characters!",
               COALESCE((SELECT array_agg(g.name ORDER BY g.name)
                         FROM core.group_members gm JOIN core.groups g ON g.id = gm.group_id
                         WHERE gm.account_id = a.id), '{}') AS "groups!"
        FROM core.accounts a
        JOIN core.characters m ON m.id = a.main_character_id
        ORDER BY a.is_owner DESC, m.name
        LIMIT $1
        "#,
        limit,
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| AccountSummary {
            id: AccountId(r.id),
            main_name: r.main_name,
            tier: r.tier,
            is_owner: r.is_owner,
            characters: r.characters,
            groups: r.groups,
        })
        .collect())
}

/// Finds an account by account id, character id or exact character name
/// (case-insensitive).
pub async fn find(pool: &PgPool, query: &str) -> Result<Option<AccountId>, sqlx::Error> {
    let id: Option<i64> = query.trim().parse().ok();
    let found = sqlx::query_scalar!(
        r#"
        SELECT a.id
        FROM core.accounts a
        LEFT JOIN core.characters c ON c.account_id = a.id
        WHERE a.id = $1 OR c.id = $1 OR lower(c.name) = lower($2)
        ORDER BY a.id = $1 DESC
        LIMIT 1
        "#,
        id,
        query.trim(),
    )
    .fetch_optional(pool)
    .await?;
    Ok(found.map(AccountId))
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn login(pool: &PgPool, id: i64, name: &str, current: Option<AccountId>) -> SignIn {
        sign_in(pool, id, name, current, false).await.unwrap().0
    }

    async fn account(pool: &PgPool, id: i64, name: &str) -> AccountId {
        login(pool, id, name, None).await.account().unwrap()
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn only_a_setup_session_claims_ownership_and_only_once(pool: PgPool) {
        // Logging in before setup doesn't make anyone owner.
        let early = account(&pool, 1, "Early Bird").await;
        assert!(!owner_exists(&pool).await.unwrap());

        let (admin, claimed) = sign_in(&pool, 2, "Admin", None, true).await.unwrap();
        assert!(claimed);
        let admin = get(&pool, admin.account().unwrap()).await.unwrap().unwrap();
        assert!(admin.is_owner);
        assert_eq!(
            admin.main,
            Character {
                id: 2,
                name: "Admin".into()
            }
        );

        // A second claim can't take ownership.
        let (_, again) = sign_in(&pool, 1, "Early Bird", None, true).await.unwrap();
        assert!(!again);
        assert!(!get(&pool, early).await.unwrap().unwrap().is_owner);
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn an_existing_account_can_claim_ownership(pool: PgPool) {
        let existing = account(&pool, 1, "Admin").await;
        let (outcome, claimed) = sign_in(&pool, 1, "Admin", None, true).await.unwrap();
        assert_eq!(outcome, SignIn::Existing(existing));
        assert!(claimed);
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn alts_link_while_signed_in_and_sign_in_to_their_account(pool: PgPool) {
        let main = account(&pool, 1, "Main").await;

        assert_eq!(
            login(&pool, 2, "Alt", Some(main)).await,
            SignIn::AddedAlt(main)
        );
        // Later, logging in with the alt alone reaches the same account.
        assert_eq!(
            login(&pool, 2, "Alt Renamed", None).await,
            SignIn::Existing(main)
        );

        let a = get(&pool, main).await.unwrap().unwrap();
        assert_eq!(a.main.id, 1);
        assert_eq!(
            a.characters,
            vec![
                Character {
                    id: 1,
                    name: "Main".into()
                },
                Character {
                    id: 2,
                    name: "Alt Renamed".into()
                },
            ]
        );
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn character_on_another_account_is_not_moved(pool: PgPool) {
        let a = account(&pool, 1, "A").await;
        let b = account(&pool, 2, "B").await;

        assert_eq!(login(&pool, 2, "B", Some(a)).await, SignIn::LinkedElsewhere);
        assert_eq!(get(&pool, b).await.unwrap().unwrap().characters.len(), 1);
        assert_eq!(get(&pool, a).await.unwrap().unwrap().characters.len(), 1);
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn main_can_switch_only_to_own_characters(pool: PgPool) {
        let a = account(&pool, 1, "Main").await;
        login(&pool, 2, "Alt", Some(a)).await;
        account(&pool, 3, "Stranger").await;

        assert!(set_main(&pool, a, 2).await.unwrap());
        assert_eq!(get(&pool, a).await.unwrap().unwrap().main.id, 2);
        assert!(!set_main(&pool, a, 3).await.unwrap());
        assert!(!set_main(&pool, a, 999).await.unwrap());
        assert_eq!(get(&pool, a).await.unwrap().unwrap().main.id, 2);
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn concurrent_owner_claims_make_exactly_one_owner(pool: PgPool) {
        let mut logins = tokio::task::JoinSet::new();
        for id in 1..=10 {
            let pool = pool.clone();
            logins.spawn(async move { sign_in(&pool, id, "Pilot", None, true).await.unwrap() });
        }
        let outcomes = logins.join_all().await;

        assert_eq!(outcomes.iter().filter(|(_, claimed)| *claimed).count(), 1);
    }
}
