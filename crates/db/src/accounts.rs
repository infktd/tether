//! Accounts and their characters.

use tether_core::states::StateId;

use crate::PgPool;

/// Serializes sign-ins so owner bootstrap and alt linking can't race.
/// Sign-ins are rare enough that a global lock costs nothing.
const SIGN_IN_LOCK: i64 = 0x7465_7468_6572_0001;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccountId(pub i64);

/// What a sign-in (a verified SSO login with no account to link to)
/// came to, following Alliance Auth's authentication backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignIn {
    /// Its account's main (or it became the main of an account that had
    /// none): signed in.
    Existing(AccountId),
    /// A returning owner: re-attached to the account it last belonged to,
    /// which had no main, and made its main.
    Reattached(AccountId),
    /// A new character: a new account, with it as main.
    Created(AccountId),
    /// An alt of an account with a main. Only the main signs in: SSO
    /// proves who controls a character, not who owns the account.
    NotMain,
    /// Its account is deactivated.
    Deactivated,
}

impl SignIn {
    /// The account the browser should be signed in to, if any.
    pub fn account(&self) -> Option<AccountId> {
        match self {
            Self::Existing(a) | Self::Reattached(a) | Self::Created(a) => Some(*a),
            Self::NotMain | Self::Deactivated => None,
        }
    }
}

/// A verified SSO login.
#[derive(Debug, Clone, Copy)]
pub struct Login<'a> {
    pub character_id: i64,
    pub character_name: &'a str,
    /// CCP's owner hash from the verified token.
    pub owner_hash: &'a str,
}

/// A character an account lost: sold (its owner hash changed), moved to
/// another account, or its last valid token gone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lost {
    pub character_id: i64,
    pub character_name: String,
    pub from: AccountId,
    /// It was that account's main, which is now cleared.
    pub was_main: bool,
    /// It was the owner account's last character: the account stops being
    /// the owner, so first-run setup reopens for the holder of the setup
    /// token (otherwise nobody could ever administer the instance again).
    pub owner_lost: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignInResult {
    pub outcome: SignIn,
    pub became_owner: bool,
    /// The character was sold: its old account lost it.
    pub lost: Option<Lost>,
}

/// Handles a verified SSO login with no account to link it to, as Alliance
/// Auth's backend does:
///
/// - A known character with the recorded owner hash signs in to its
///   account if it's the main, or if the account has no main (it becomes
///   the main: SSO just proved control of it). Any other alt is refused.
/// - A known character with a different owner hash was sold: its old
///   account loses it, and it's treated as new.
/// - A new character whose owner hash was seen on it before is
///   re-attached to that account if the account has no main (and made its
///   main); refused as an alt if it has one.
/// - Otherwise a new account is created with it as main.
///
/// With `claim_owner` (the browser holds a valid first-run setup session),
/// the resulting account becomes the owner if there is none yet.
pub async fn sign_in(
    pool: &PgPool,
    login: Login<'_>,
    claim_owner: bool,
) -> Result<SignInResult, sqlx::Error> {
    let mut tx = pool.begin().await?;
    lock(&mut tx).await?;
    let mut lost = None;
    let row = sqlx::query!(
        r#"
        SELECT c.account_id, c.owner_hash, a.main_character_id, a.active
        FROM core.characters c JOIN core.accounts a ON a.id = c.account_id
        WHERE c.id = $1
        "#,
        login.character_id
    )
    .fetch_optional(&mut *tx)
    .await?;
    let outcome = match row {
        Some(r)
            if r.owner_hash
                .as_deref()
                .is_none_or(|h| h == login.owner_hash) =>
        {
            let account = AccountId(r.account_id);
            if !r.active {
                SignIn::Deactivated
            } else {
                match r.main_character_id {
                    Some(main) if main != login.character_id => SignIn::NotMain,
                    // Taking over a main-less account needs a recorded owner
                    // hash to match (characters from before hashes were
                    // recorded can't prove it).
                    None if r.owner_hash.is_none() => SignIn::NotMain,
                    main => {
                        touch(&mut tx, login).await?;
                        if main.is_none() {
                            set_main_in(&mut tx, account, login.character_id).await?;
                            audit_main(&mut tx, account, login, "signed in").await?;
                        }
                        SignIn::Existing(account)
                    }
                }
            }
        }
        known => {
            if known.is_some() {
                // Sold: the old account loses it (AA deletes the ownership).
                lost = lose(&mut tx, login.character_id, "sold").await?;
            }
            let returning = sqlx::query!(
                r#"
                SELECT r.account_id AS "account_id!", a.main_character_id, a.active
                FROM core.ownership_records r JOIN core.accounts a ON a.id = r.account_id
                WHERE r.character_id = $1 AND r.owner_hash = $2
                ORDER BY r.id DESC
                LIMIT 1
                "#,
                login.character_id,
                login.owner_hash,
            )
            .fetch_optional(&mut *tx)
            .await?;
            match returning {
                Some(r) if r.main_character_id.is_some() => SignIn::NotMain,
                Some(r) => {
                    let account = AccountId(r.account_id);
                    attach(&mut tx, account, login).await?;
                    set_main_in(&mut tx, account, login.character_id).await?;
                    audit_main(&mut tx, account, login, "returning owner").await?;
                    if r.active {
                        SignIn::Reattached(account)
                    } else {
                        SignIn::Deactivated
                    }
                }
                None => {
                    let id = sqlx::query_scalar!(
                        "INSERT INTO core.accounts (main_character_id) VALUES ($1) RETURNING id",
                        login.character_id,
                    )
                    .fetch_one(&mut *tx)
                    .await?;
                    let account = AccountId(id);
                    attach(&mut tx, account, login).await?;
                    SignIn::Created(account)
                }
            }
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
    Ok(SignInResult {
        outcome,
        became_owner,
        lost,
    })
}

/// What linking a character to an account did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Linked {
    /// Already this account's.
    Existing,
    /// New to Tether: added.
    Added,
    /// Taken from another account (see `LinkResult::lost`).
    Moved,
    /// It belongs to a deactivated account: refused, nothing changed.
    /// Taking it from there is an admin's decision, or deactivation could
    /// be undone from a fresh account.
    Deactivated,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkResult {
    pub outcome: Linked,
    /// The account the character was taken from (moved or sold).
    pub lost: Option<Lost>,
}

/// Links a character to `account` after a fresh SSO login started by that
/// account (Add Character, or an offer), as Alliance Auth does for any
/// token saved for a signed-in user: a character on another account moves
/// here (SSO just proved control of it), clearing that account's main if
/// it was the main.
pub async fn link(
    pool: &PgPool,
    login: Login<'_>,
    account: AccountId,
) -> Result<LinkResult, sqlx::Error> {
    let mut tx = pool.begin().await?;
    lock(&mut tx).await?;
    let row = sqlx::query!(
        r#"
        SELECT c.account_id, c.owner_hash, a.active
        FROM core.characters c JOIN core.accounts a ON a.id = c.account_id
        WHERE c.id = $1
        "#,
        login.character_id
    )
    .fetch_optional(&mut *tx)
    .await?;
    let (outcome, lost) = match row {
        Some(r) if r.account_id != account.0 && !r.active => {
            return Ok(LinkResult {
                outcome: Linked::Deactivated,
                lost: None,
            });
        }
        Some(r) if r.account_id == account.0 => {
            if r.owner_hash.as_deref() != Some(login.owner_hash) {
                record(&mut tx, account, login).await?;
            }
            touch(&mut tx, login).await?;
            (Linked::Existing, None)
        }
        Some(_) => {
            let lost = lose(&mut tx, login.character_id, "moved").await?;
            attach(&mut tx, account, login).await?;
            (Linked::Moved, lost)
        }
        None => {
            // A character that left a deactivated account (its tokens
            // revoked, say) can't come back through a fresh account either.
            let from_inactive = sqlx::query_scalar!(
                r#"
                SELECT NOT a.active AS "inactive!"
                FROM core.ownership_records r JOIN core.accounts a ON a.id = r.account_id
                WHERE r.character_id = $1 AND r.owner_hash = $2
                ORDER BY r.id DESC
                LIMIT 1
                "#,
                login.character_id,
                login.owner_hash,
            )
            .fetch_optional(&mut *tx)
            .await?
            .unwrap_or(false);
            if from_inactive {
                return Ok(LinkResult {
                    outcome: Linked::Deactivated,
                    lost: None,
                });
            }
            attach(&mut tx, account, login).await?;
            (Linked::Added, None)
        }
    };
    // An account without a main takes this one.
    let took = sqlx::query!(
        "UPDATE core.accounts SET main_character_id = $2 WHERE id = $1 AND main_character_id IS NULL",
        account.0,
        login.character_id,
    )
    .execute(&mut *tx)
    .await?;
    if took.rows_affected() == 1 {
        audit_main(&mut tx, account, login, "added").await?;
    }
    tx.commit().await?;
    Ok(LinkResult { outcome, lost })
}

/// Why a character leaves its account on the ownership check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LossCause {
    /// A refresh showed another owner hash: proof of a sale.
    Sold,
    /// Its token is dead. Only if it's still revoked (not re-registered
    /// since), has been for a day (a grace period against glitches), and
    /// isn't the owner account's last character (which would lock the
    /// instance's owner out).
    Token,
}

/// Takes a character away from its account on the ownership check: its
/// tokens and everything tied to it go, and its account's main is cleared
/// if it was the main. The account stays, as AA's user does. Audited in
/// the same transaction.
pub async fn lose_ownership(
    pool: &PgPool,
    character_id: i64,
    cause: LossCause,
) -> Result<Option<Lost>, sqlx::Error> {
    let mut tx = pool.begin().await?;
    lock(&mut tx).await?;
    // Re-checked under the lock: the owner may have signed in (and
    // re-registered it) since the check listed it.
    let eligible = sqlx::query_scalar!(
        r#"
        SELECT true AS "ok!"
        FROM core.character_tokens t
        JOIN core.characters c ON c.id = t.character_id
        JOIN core.accounts a ON a.id = c.account_id
        WHERE t.character_id = $1 AND t.state = 'revoked'
          AND CASE WHEN $2 THEN t.revoked_reason = 'owner hash changed'
              ELSE t.revoked_at < now() - interval '1 day'
                   -- Deactivated accounts keep their characters until an
                   -- admin decides: else revoking would be a way out.
                   AND a.active
                   AND NOT (a.is_owner AND (
                     SELECT count(*) FROM core.characters o WHERE o.account_id = a.id
                   ) = 1)
              END
        "#,
        character_id,
        cause == LossCause::Sold,
    )
    .fetch_optional(&mut *tx)
    .await?;
    if eligible.is_none() {
        return Ok(None);
    }
    let reason = match cause {
        LossCause::Sold => "sold",
        LossCause::Token => "token",
    };
    let lost = lose(&mut tx, character_id, reason).await?;
    tx.commit().await?;
    Ok(lost)
}

async fn audit_main(
    tx: &mut sqlx::PgTransaction<'_>,
    account: AccountId,
    login: Login<'_>,
    how: &str,
) -> Result<(), sqlx::Error> {
    crate::audit::record(
        &mut **tx,
        crate::audit::Actor::System,
        "account.main_set",
        Some(&format!("account:{}", account.0)),
        serde_json::json!({ "character_id": login.character_id, "name": login.character_name, "how": how }),
    )
    .await
}

/// Serializes sign-ins, links and ownership changes.
async fn lock(tx: &mut sqlx::PgTransaction<'_>) -> Result<(), sqlx::Error> {
    sqlx::query!("SELECT pg_advisory_xact_lock($1)", SIGN_IN_LOCK)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

async fn lose(
    tx: &mut sqlx::PgTransaction<'_>,
    character_id: i64,
    reason: &str,
) -> Result<Option<Lost>, sqlx::Error> {
    let row = sqlx::query!(
        r#"
        SELECT c.account_id, c.name, a.main_character_id
        FROM core.characters c JOIN core.accounts a ON a.id = c.account_id
        WHERE c.id = $1
        "#,
        character_id
    )
    .fetch_optional(&mut **tx)
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let was_main = row.main_character_id == Some(character_id);
    if was_main {
        sqlx::query!(
            "UPDATE core.accounts SET main_character_id = NULL WHERE id = $1",
            row.account_id
        )
        .execute(&mut **tx)
        .await?;
    }
    sqlx::query!("DELETE FROM core.characters WHERE id = $1", character_id)
        .execute(&mut **tx)
        .await?;
    let owner_lost = sqlx::query!(
        r#"
        UPDATE core.accounts SET is_owner = false
        WHERE id = $1 AND is_owner
          AND NOT EXISTS (SELECT 1 FROM core.characters WHERE account_id = $1)
        "#,
        row.account_id
    )
    .execute(&mut **tx)
    .await?
    .rows_affected()
        == 1;
    crate::audit::record(
        &mut **tx,
        crate::audit::Actor::System,
        "character.ownership_lost",
        Some(&format!("character:{character_id}")),
        serde_json::json!({
            "name": row.name,
            "from_account": row.account_id,
            "was_main": was_main,
            "owner_lost": owner_lost,
            "reason": reason,
        }),
    )
    .await?;
    Ok(Some(Lost {
        character_id,
        character_name: row.name,
        from: AccountId(row.account_id),
        was_main,
        owner_lost,
    }))
}

async fn touch(tx: &mut sqlx::PgTransaction<'_>, login: Login<'_>) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "UPDATE core.characters SET name = $2, owner_hash = $3, last_login_at = now() WHERE id = $1",
        login.character_id,
        login.character_name,
        login.owner_hash,
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn set_main_in(
    tx: &mut sqlx::PgTransaction<'_>,
    account: AccountId,
    character_id: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "UPDATE core.accounts SET main_character_id = $2 WHERE id = $1",
        account.0,
        character_id,
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Adds a character to an account and records the ownership.
async fn attach(
    tx: &mut sqlx::PgTransaction<'_>,
    account: AccountId,
    login: Login<'_>,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        r#"
        INSERT INTO core.characters (id, account_id, name, owner_hash, last_login_at)
        VALUES ($1, $2, $3, $4, now())
        "#,
        login.character_id,
        account.0,
        login.character_name,
        login.owner_hash,
    )
    .execute(&mut **tx)
    .await?;
    record(tx, account, login).await
}

/// Records an ownership, unless the latest record for this character and
/// owner hash is already this account's.
async fn record(
    tx: &mut sqlx::PgTransaction<'_>,
    account: AccountId,
    login: Login<'_>,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        r#"
        INSERT INTO core.ownership_records (character_id, character_name, owner_hash, account_id)
        SELECT $1, $2, $3, $4
        WHERE (
            SELECT account_id FROM core.ownership_records
            WHERE character_id = $1 AND owner_hash = $3
            ORDER BY id DESC LIMIT 1
        ) IS DISTINCT FROM $4
        "#,
        login.character_id,
        login.character_name,
        login.owner_hash,
        account.0,
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Deactivates an account (AA's inactive user): its sessions end and it
/// can't sign in; evaluation makes it Guest. Not the owner. False if it
/// was already inactive, is the owner, or doesn't exist.
pub async fn deactivate(
    tx: &mut sqlx::PgConnection,
    account: AccountId,
    by: Option<AccountId>,
) -> Result<bool, sqlx::Error> {
    let updated = sqlx::query!(
        r#"
        UPDATE core.accounts SET active = false, deactivated_at = now(), deactivated_by = $2
        WHERE id = $1 AND active AND NOT is_owner
        "#,
        account.0,
        by.map(|b| b.0),
    )
    .execute(&mut *tx)
    .await?;
    if updated.rows_affected() != 1 {
        return Ok(false);
    }
    sqlx::query!("DELETE FROM core.sessions WHERE account_id = $1", account.0)
        .execute(&mut *tx)
        .await?;
    Ok(true)
}

pub async fn is_active<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    account: AccountId,
) -> Result<Option<bool>, sqlx::Error> {
    sqlx::query_scalar!("SELECT active FROM core.accounts WHERE id = $1", account.0)
        .fetch_optional(executor)
        .await
}

/// Reactivates an account. False if it was active or doesn't exist.
pub async fn reactivate<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    account: AccountId,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query!(
        r#"
        UPDATE core.accounts SET active = true, deactivated_at = NULL, deactivated_by = NULL
        WHERE id = $1 AND NOT active
        "#,
        account.0,
    )
    .execute(executor)
    .await?;
    Ok(result.rows_affected() == 1)
}

pub async fn owner_exists(pool: &PgPool) -> Result<bool, sqlx::Error> {
    let exists = sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM core.accounts WHERE is_owner) AS "exists!""#
    )
    .fetch_one(pool)
    .await?;
    Ok(exists)
}

/// What group rules look at: whether the account is the owner, is
/// active, and has a main, and its state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Standing {
    pub is_owner: bool,
    pub active: bool,
    pub has_main: bool,
    pub state: StateId,
}

pub async fn standing<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    account: AccountId,
) -> Result<Option<Standing>, sqlx::Error> {
    let row = sqlx::query!(
        r#"
        SELECT is_owner, active, main_character_id IS NOT NULL AS "has_main!", state_id
        FROM core.accounts WHERE id = $1
        "#,
        account.0
    )
    .fetch_optional(executor)
    .await?;
    Ok(row.map(|r| Standing {
        is_owner: r.is_owner,
        active: r.active,
        has_main: r.has_main,
        state: StateId(r.state_id),
    }))
}

pub async fn all_ids(pool: &PgPool) -> Result<Vec<AccountId>, sqlx::Error> {
    let ids = sqlx::query_scalar!("SELECT id FROM core.accounts ORDER BY id")
        .fetch_all(pool)
        .await?;
    Ok(ids.into_iter().map(AccountId).collect())
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
    pub active: bool,
    /// `None` after the main was sold or lost its token, until the owner
    /// signs in with one of the account's characters or uses Change Main.
    pub main: Option<Character>,
    /// All characters including the main, main first, then by name.
    pub characters: Vec<Character>,
}

pub async fn get(pool: &PgPool, account: AccountId) -> Result<Option<Account>, sqlx::Error> {
    let Some(head) = sqlx::query!(
        "SELECT is_owner, active, main_character_id FROM core.accounts WHERE id = $1",
        account.0
    )
    .fetch_optional(pool)
    .await?
    else {
        return Ok(None);
    };
    let characters: Vec<Character> = sqlx::query!(
        r#"
        SELECT c.id, c.name
        FROM core.characters c JOIN core.accounts a ON a.id = c.account_id
        WHERE c.account_id = $1
        ORDER BY c.id = a.main_character_id DESC NULLS LAST, c.name
        "#,
        account.0,
    )
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|r| Character {
        id: r.id,
        name: r.name,
    })
    .collect();
    let main = characters
        .iter()
        .find(|c| Some(c.id) == head.main_character_id)
        .cloned();
    Ok(Some(Account {
        id: account,
        is_owner: head.is_owner,
        active: head.active,
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
          -- As AA, only to a character with a working token: still yours.
          AND EXISTS (
            SELECT 1 FROM core.character_tokens
            WHERE character_id = $2 AND state = 'valid'
          )
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
    /// The state's name.
    pub state: String,
    pub is_owner: bool,
    pub characters: i64,
    pub groups: Vec<String>,
}

/// Accounts for the admin CLI, owner first, then by main name.
pub async fn list_summaries(pool: &PgPool, limit: i64) -> Result<Vec<AccountSummary>, sqlx::Error> {
    let rows = sqlx::query!(
        r#"
        SELECT a.id, COALESCE(m.name, '(no main)') AS "main_name!", s.name AS state, a.is_owner,
               (SELECT count(*) FROM core.characters c WHERE c.account_id = a.id) AS "characters!",
               COALESCE((SELECT array_agg(g.name ORDER BY g.name)
                         FROM core.group_members gm JOIN core.groups g ON g.id = gm.group_id
                         WHERE gm.account_id = a.id), '{}') AS "groups!"
        FROM core.accounts a
        LEFT JOIN core.characters m ON m.id = a.main_character_id
        JOIN core.states s ON s.id = a.state_id
        ORDER BY a.is_owner DESC, m.name NULLS LAST
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
            state: r.state,
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

    fn who(id: i64, name: &str) -> Login<'_> {
        Login {
            character_id: id,
            character_name: name,
            owner_hash: "owner-a",
        }
    }

    async fn login(pool: &PgPool, id: i64, name: &str) -> SignIn {
        sign_in(pool, who(id, name), false).await.unwrap().outcome
    }

    async fn claim(pool: &PgPool, id: i64, name: &str) -> SignInResult {
        sign_in(pool, who(id, name), true).await.unwrap()
    }

    async fn account(pool: &PgPool, id: i64, name: &str) -> AccountId {
        login(pool, id, name).await.account().unwrap()
    }

    async fn add(pool: &PgPool, id: i64, name: &str, to: AccountId) -> LinkResult {
        link(pool, who(id, name), to).await.unwrap()
    }

    fn character(id: i64, name: &str) -> Character {
        Character {
            id,
            name: name.into(),
        }
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn only_a_setup_session_claims_ownership_and_only_once(pool: PgPool) {
        // Logging in before setup doesn't make anyone owner.
        let early = account(&pool, 1, "Early Bird").await;
        assert!(!owner_exists(&pool).await.unwrap());

        let admin = claim(&pool, 2, "Admin").await;
        assert!(admin.became_owner);
        let admin = get(&pool, admin.outcome.account().unwrap())
            .await
            .unwrap()
            .unwrap();
        assert!(admin.is_owner);
        assert_eq!(admin.main, Some(character(2, "Admin")));

        // A second claim can't take ownership.
        assert!(!claim(&pool, 1, "Early Bird").await.became_owner);
        assert!(!get(&pool, early).await.unwrap().unwrap().is_owner);
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn an_existing_account_can_claim_ownership(pool: PgPool) {
        let existing = account(&pool, 1, "Admin").await;
        let result = claim(&pool, 1, "Admin").await;
        assert_eq!(result.outcome, SignIn::Existing(existing));
        assert!(result.became_owner);
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn only_the_main_signs_in(pool: PgPool) {
        let main = account(&pool, 1, "Main").await;
        assert_eq!(add(&pool, 2, "Alt", main).await.outcome, Linked::Added);

        // AA: SSO proves control of a character, not of the account.
        assert_eq!(login(&pool, 2, "Alt").await, SignIn::NotMain);
        assert_eq!(login(&pool, 1, "Main").await, SignIn::Existing(main));

        let a = get(&pool, main).await.unwrap().unwrap();
        assert_eq!(a.main, Some(character(1, "Main")));
        assert_eq!(
            a.characters,
            vec![character(1, "Main"), character(2, "Alt")]
        );
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn a_character_on_another_account_moves_when_linked(pool: PgPool) {
        let a = account(&pool, 1, "A").await;
        let b = account(&pool, 2, "B").await;

        let result = add(&pool, 2, "B", a).await;

        assert_eq!(result.outcome, Linked::Moved);
        assert_eq!(
            result.lost,
            Some(Lost {
                character_id: 2,
                character_name: "B".into(),
                from: b,
                was_main: true,
                owner_lost: false,
            })
        );
        let b = get(&pool, b).await.unwrap().unwrap();
        assert!(b.characters.is_empty() && b.main.is_none());
        assert_eq!(get(&pool, a).await.unwrap().unwrap().characters.len(), 2);
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn main_changes_only_to_own_characters_with_a_working_token(pool: PgPool) {
        let a = account(&pool, 1, "Main").await;
        add(&pool, 2, "Alt", a).await;
        add(&pool, 4, "Tokenless", a).await;
        account(&pool, 3, "Stranger").await;
        for id in [2, 3] {
            crate::tokens::upsert(&pool, id, b"sealed", &[])
                .await
                .unwrap();
        }

        assert!(set_main(&pool, a, 2).await.unwrap());
        assert_eq!(
            get(&pool, a).await.unwrap().unwrap().main.map(|m| m.id),
            Some(2)
        );
        assert!(!set_main(&pool, a, 3).await.unwrap(), "someone else's");
        assert!(!set_main(&pool, a, 4).await.unwrap(), "no token");
        crate::tokens::mark_revoked(&pool, 2, "gone", None)
            .await
            .unwrap();
        add(&pool, 5, "Other", a).await;
        crate::tokens::upsert(&pool, 5, b"sealed", &[])
            .await
            .unwrap();
        crate::tokens::mark_revoked(&pool, 5, "gone", None)
            .await
            .unwrap();
        assert!(!set_main(&pool, a, 5).await.unwrap(), "revoked token");
        assert!(!set_main(&pool, a, 999).await.unwrap());
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn concurrent_owner_claims_make_exactly_one_owner(pool: PgPool) {
        let mut logins = tokio::task::JoinSet::new();
        for id in 1..=10 {
            let pool = pool.clone();
            logins.spawn(async move { claim(&pool, id, "Pilot").await });
        }
        let outcomes = logins.join_all().await;

        assert_eq!(outcomes.iter().filter(|r| r.became_owner).count(), 1);
    }

    async fn sold(pool: &PgPool, id: i64) -> SignInResult {
        let login = Login {
            character_id: id,
            character_name: "Sold",
            owner_hash: "owner-b",
        };
        sign_in(pool, login, false).await.unwrap()
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn a_sold_alt_leaves_its_account(pool: PgPool) {
        let seller = account(&pool, 1, "Seller").await;
        add(&pool, 2, "Sold", seller).await;

        let result = sold(&pool, 2).await;

        let buyer = result.outcome.account().unwrap();
        assert_ne!(buyer, seller);
        assert_eq!(
            result.lost.map(|l| (l.from, l.was_main)),
            Some((seller, false))
        );
        let seller = get(&pool, seller).await.unwrap().unwrap();
        assert_eq!(seller.characters, vec![character(1, "Seller")]);
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn a_sold_main_clears_the_main_until_the_owner_signs_in_again(pool: PgPool) {
        let seller = account(&pool, 1, "Seller Main").await;
        add(&pool, 2, "Seller Alt", seller).await;

        let result = sold(&pool, 1).await;

        assert!(matches!(result.outcome, SignIn::Created(_)));
        assert_eq!(result.lost.map(|l| l.was_main), Some(true));
        let account = get(&pool, seller).await.unwrap().unwrap();
        assert_eq!(account.main, None, "no alt is promoted silently");
        assert_eq!(account.characters, vec![character(2, "Seller Alt")]);

        // The owner signs in with a character still theirs: it's the main.
        assert_eq!(
            login(&pool, 2, "Seller Alt").await,
            SignIn::Existing(seller)
        );
        assert_eq!(
            get(&pool, seller).await.unwrap().unwrap().main,
            Some(character(2, "Seller Alt"))
        );
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn losing_the_owners_last_character_reopens_setup(pool: PgPool) {
        let owner = claim(&pool, 1, "Owner").await.outcome.account().unwrap();

        let result = sold(&pool, 1).await;

        let owner_account = get(&pool, owner).await.unwrap().unwrap();
        assert!(owner_account.characters.is_empty());
        // Nobody could administer the instance: setup reopens.
        assert_eq!(result.lost.map(|l| l.owner_lost), Some(true));
        assert!(!owner_account.is_owner && !owner_exists(&pool).await.unwrap());
        let buyer = get(&pool, result.outcome.account().unwrap())
            .await
            .unwrap()
            .unwrap();
        assert!(!buyer.is_owner, "the buyer must not inherit ownership");
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn a_returning_owner_is_reattached(pool: PgPool) {
        let a = account(&pool, 1, "Main").await;
        add(&pool, 2, "Alt", a).await;
        // Both tokens died two days ago: both leave the account.
        for id in [1, 2] {
            crate::tokens::upsert(&pool, id, b"sealed", &[])
                .await
                .unwrap();
            crate::tokens::mark_revoked(&pool, id, "invalid_grant", None)
                .await
                .unwrap();
        }
        sqlx::query("UPDATE core.character_tokens SET revoked_at = now() - interval '2 days'")
            .execute(&pool)
            .await
            .unwrap();
        assert!(
            lose_ownership(&pool, 1, LossCause::Token)
                .await
                .unwrap()
                .is_some()
        );
        assert!(
            lose_ownership(&pool, 2, LossCause::Token)
                .await
                .unwrap()
                .is_some()
        );

        // Same character, same owner: back to the old account, as its main.
        assert_eq!(login(&pool, 2, "Alt").await, SignIn::Reattached(a));
        assert_eq!(
            get(&pool, a).await.unwrap().unwrap().main.map(|m| m.id),
            Some(2)
        );
        // The other one returns as an alt: refused, and not re-attached.
        assert_eq!(login(&pool, 1, "Main").await, SignIn::NotMain);
        assert_eq!(get(&pool, a).await.unwrap().unwrap().characters.len(), 1);
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn deactivated_accounts_cant_sign_in_and_the_owner_cant_be_deactivated(pool: PgPool) {
        let owner = claim(&pool, 1, "Owner").await.outcome.account().unwrap();
        let pilot = account(&pool, 2, "Pilot").await;
        let mut tx = pool.begin().await.unwrap();
        assert!(!deactivate(&mut tx, owner, None).await.unwrap());
        assert!(deactivate(&mut tx, pilot, Some(owner)).await.unwrap());
        tx.commit().await.unwrap();

        assert_eq!(login(&pool, 2, "Pilot").await, SignIn::Deactivated);
        assert!(reactivate(&pool, pilot).await.unwrap());
        assert_eq!(login(&pool, 2, "Pilot").await, SignIn::Existing(pilot));
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn same_owner_hash_or_first_sighting_is_not_a_sale(pool: PgPool) {
        let a = account(&pool, 1, "Pilot").await;
        assert_eq!(login(&pool, 1, "Pilot").await, SignIn::Existing(a));
        // Characters from before owner hashes were recorded adopt the first
        // hash they're seen with.
        sqlx::query("UPDATE core.characters SET owner_hash = NULL WHERE id = 1")
            .execute(&pool)
            .await
            .unwrap();
        let result = sold(&pool, 1).await;
        assert_eq!(result.outcome, SignIn::Existing(a));
        assert!(result.lost.is_none());
    }
}
