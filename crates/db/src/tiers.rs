//! Tier rules, character affiliations and account tiers.

use tether_core::tiers::{Affiliation, EntityKind, Tier, TierRules};

use crate::PgPool;
use crate::accounts::AccountId;

pub async fn load_rules(pool: &PgPool) -> Result<TierRules, sqlx::Error> {
    let rows = sqlx::query!("SELECT entity_id, entity_kind, tier FROM core.tier_rules")
        .fetch_all(pool)
        .await?;
    let mut rules = TierRules::default();
    for row in rows {
        // The CHECK constraints make other values impossible.
        if let (Some(kind), Some(tier)) =
            (EntityKind::parse(&row.entity_kind), Tier::parse(&row.tier))
        {
            rules.add(kind, row.entity_id, tier);
        }
    }
    Ok(rules)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TierRule {
    pub entity_id: i64,
    pub kind: EntityKind,
    pub tier: Tier,
    pub name: String,
}

pub async fn list_rules(pool: &PgPool) -> Result<Vec<TierRule>, sqlx::Error> {
    let rows = sqlx::query!(
        "SELECT entity_id, entity_kind, tier, name FROM core.tier_rules ORDER BY tier, name"
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|r| {
            Some(TierRule {
                entity_id: r.entity_id,
                kind: EntityKind::parse(&r.entity_kind)?,
                tier: Tier::parse(&r.tier)?,
                name: r.name,
            })
        })
        .collect())
}

/// Adds or replaces the rule for an alliance or corporation.
pub async fn set_rule<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    rule: &TierRule,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        r#"
        INSERT INTO core.tier_rules (entity_id, entity_kind, tier, name)
        VALUES ($1, $2, $3, $4)
        ON CONFLICT (entity_id) DO UPDATE
        SET entity_kind = EXCLUDED.entity_kind, tier = EXCLUDED.tier, name = EXCLUDED.name
        "#,
        rule.entity_id,
        rule.kind.as_str(),
        rule.tier.as_str(),
        rule.name,
    )
    .execute(executor)
    .await?;
    Ok(())
}

pub async fn remove_rule<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    entity_id: i64,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query!(
        "DELETE FROM core.tier_rules WHERE entity_id = $1",
        entity_id
    )
    .execute(executor)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// Every linked character, for the affiliation sync.
pub async fn all_character_ids(pool: &PgPool) -> Result<Vec<i64>, sqlx::Error> {
    sqlx::query_scalar!("SELECT id FROM core.characters ORDER BY id")
        .fetch_all(pool)
        .await
}

pub async fn character_ids(pool: &PgPool, account: AccountId) -> Result<Vec<i64>, sqlx::Error> {
    sqlx::query_scalar!(
        "SELECT id FROM core.characters WHERE account_id = $1 ORDER BY id",
        account.0
    )
    .fetch_all(pool)
    .await
}

/// Stores fresh affiliations. Ids not in `core.characters` are ignored.
pub async fn update_affiliations(
    pool: &PgPool,
    affiliations: &[(i64, Affiliation)],
) -> Result<(), sqlx::Error> {
    let ids: Vec<i64> = affiliations.iter().map(|(id, _)| *id).collect();
    let corps: Vec<i64> = affiliations.iter().map(|(_, a)| a.corporation_id).collect();
    let alliances: Vec<Option<i64>> = affiliations.iter().map(|(_, a)| a.alliance_id).collect();
    sqlx::query!(
        r#"
        UPDATE core.characters c
        SET corporation_id = u.corporation_id,
            alliance_id = u.alliance_id,
            affiliation_checked_at = now()
        FROM UNNEST($1::bigint[], $2::bigint[], $3::bigint[])
            AS u(id, corporation_id, alliance_id)
        WHERE c.id = u.id
        "#,
        &ids,
        &corps,
        &alliances as &[Option<i64>],
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// The main's last known affiliation, if it has been checked.
pub async fn main_affiliation(
    pool: &PgPool,
    account: AccountId,
) -> Result<Option<Affiliation>, sqlx::Error> {
    let row = sqlx::query!(
        r#"
        SELECT c.corporation_id, c.alliance_id
        FROM core.accounts a
        JOIN core.characters c ON c.id = a.main_character_id
        WHERE a.id = $1
        "#,
        account.0,
    )
    .fetch_optional(pool)
    .await?;
    Ok(row.and_then(|r| {
        Some(Affiliation {
            corporation_id: r.corporation_id?,
            alliance_id: r.alliance_id,
        })
    }))
}

/// Records the account's tier and returns the previous one.
pub async fn set_account_tier<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    account: AccountId,
    tier: Tier,
) -> Result<Option<Tier>, sqlx::Error> {
    let previous = sqlx::query_scalar!(
        r#"
        UPDATE core.accounts new
        SET tier = $2, tier_evaluated_at = now()
        FROM core.accounts old
        WHERE new.id = $1 AND old.id = new.id
        RETURNING old.tier
        "#,
        account.0,
        tier.as_str(),
    )
    .fetch_optional(executor)
    .await?;
    Ok(previous.as_deref().and_then(Tier::parse))
}

pub async fn account_tier(pool: &PgPool, account: AccountId) -> Result<Option<Tier>, sqlx::Error> {
    let tier = sqlx::query_scalar!("SELECT tier FROM core.accounts WHERE id = $1", account.0)
        .fetch_optional(pool)
        .await?;
    Ok(tier.as_deref().and_then(Tier::parse))
}
