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
    /// A new character with no session: a new account, owner if first.
    Created { account: AccountId, is_owner: bool },
    /// The character already belongs to a different account than the one
    /// signed in. Nothing changed.
    LinkedElsewhere,
}

impl SignIn {
    /// The account the browser should be signed in to afterwards, if any.
    pub fn account(&self) -> Option<AccountId> {
        match self {
            Self::Existing(a) | Self::AddedAlt(a) => Some(*a),
            Self::Created { account, .. } => Some(*account),
            Self::LinkedElsewhere => None,
        }
    }
}

/// Handles a successful SSO login by `character_id`, optionally while
/// already signed in to `current`.
///
/// TODO(milestone 1): detect character transfers (sold or moved to another
/// EVE account) via the SSO `owner` hash and unlink instead of refusing.
pub async fn sign_in(
    pool: &PgPool,
    character_id: i64,
    character_name: &str,
    current: Option<AccountId>,
) -> Result<SignIn, sqlx::Error> {
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
            let row = sqlx::query!(
                r#"
                INSERT INTO core.accounts (main_character_id, is_owner)
                VALUES ($1, NOT EXISTS (SELECT 1 FROM core.accounts WHERE is_owner))
                RETURNING id, is_owner
                "#,
                character_id,
            )
            .fetch_one(&mut *tx)
            .await?;
            let account = AccountId(row.id);
            insert_character(&mut tx, account, character_id, character_name).await?;
            SignIn::Created {
                account,
                is_owner: row.is_owner,
            }
        }
    };
    tx.commit().await?;
    Ok(outcome)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn first_account_is_owner_and_later_ones_are_not(pool: PgPool) {
        let first = sign_in(&pool, 1, "First", None).await.unwrap();
        let second = sign_in(&pool, 2, "Second", None).await.unwrap();

        assert!(matches!(first, SignIn::Created { is_owner: true, .. }));
        assert!(matches!(
            second,
            SignIn::Created {
                is_owner: false,
                ..
            }
        ));
        let owner = get(&pool, first.account().unwrap()).await.unwrap().unwrap();
        assert_eq!(
            owner.main,
            Character {
                id: 1,
                name: "First".into()
            }
        );
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn alts_link_while_signed_in_and_sign_in_to_their_account(pool: PgPool) {
        let account = sign_in(&pool, 1, "Main", None)
            .await
            .unwrap()
            .account()
            .unwrap();

        let alt = sign_in(&pool, 2, "Alt", Some(account)).await.unwrap();
        assert_eq!(alt, SignIn::AddedAlt(account));

        // Later, logging in with the alt alone reaches the same account.
        assert_eq!(
            sign_in(&pool, 2, "Alt Renamed", None).await.unwrap(),
            SignIn::Existing(account)
        );
        let a = get(&pool, account).await.unwrap().unwrap();
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
        let a = sign_in(&pool, 1, "A", None)
            .await
            .unwrap()
            .account()
            .unwrap();
        let b = sign_in(&pool, 2, "B", None)
            .await
            .unwrap()
            .account()
            .unwrap();

        assert_eq!(
            sign_in(&pool, 2, "B", Some(a)).await.unwrap(),
            SignIn::LinkedElsewhere
        );
        assert_eq!(get(&pool, b).await.unwrap().unwrap().characters.len(), 1);
        assert_eq!(get(&pool, a).await.unwrap().unwrap().characters.len(), 1);
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn main_can_switch_only_to_own_characters(pool: PgPool) {
        let a = sign_in(&pool, 1, "Main", None)
            .await
            .unwrap()
            .account()
            .unwrap();
        sign_in(&pool, 2, "Alt", Some(a)).await.unwrap();
        sign_in(&pool, 3, "Stranger", None).await.unwrap();

        assert!(set_main(&pool, a, 2).await.unwrap());
        assert_eq!(get(&pool, a).await.unwrap().unwrap().main.id, 2);
        assert!(!set_main(&pool, a, 3).await.unwrap());
        assert!(!set_main(&pool, a, 999).await.unwrap());
        assert_eq!(get(&pool, a).await.unwrap().unwrap().main.id, 2);
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn concurrent_first_logins_make_exactly_one_owner(pool: PgPool) {
        let mut logins = tokio::task::JoinSet::new();
        for id in 1..=10 {
            let pool = pool.clone();
            logins.spawn(async move { sign_in(&pool, id, "Pilot", None).await.unwrap() });
        }
        let outcomes = logins.join_all().await;

        let owners = outcomes
            .iter()
            .filter(|o| matches!(o, SignIn::Created { is_owner: true, .. }))
            .count();
        assert_eq!(owners, 1);
    }
}
