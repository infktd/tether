//! Menu customization (AA's Menu): the sidebar's sections, folders, items
//! and custom links, as admins arranged them.

/// What an entry is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Section,
    Folder,
    Item,
    Link,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Section => "section",
            Self::Folder => "folder",
            Self::Item => "item",
            Self::Link => "link",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "section" => Some(Self::Section),
            "folder" => Some(Self::Folder),
            "item" => Some(Self::Item),
            "link" => Some(Self::Link),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub id: i64,
    pub kind: Kind,
    pub key: Option<String>,
    pub label: Option<String>,
    pub url: Option<String>,
    pub new_tab: bool,
    pub parent_id: Option<i64>,
    pub position: i32,
    pub hidden: bool,
}

pub async fn entries<'e>(executor: impl sqlx::PgExecutor<'e>) -> Result<Vec<Entry>, sqlx::Error> {
    let rows = sqlx::query!(
        r#"
        SELECT id, kind, key, label, url, new_tab, parent_id, position, hidden
        FROM core.menu_entries ORDER BY position, id
        "#
    )
    .fetch_all(executor)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|r| {
            Some(Entry {
                id: r.id,
                kind: Kind::parse(&r.kind)?,
                key: r.key,
                label: r.label,
                url: r.url,
                new_tab: r.new_tab,
                parent_id: r.parent_id,
                position: r.position,
                hidden: r.hidden,
            })
        })
        .collect())
}

/// A new entry; returns its id.
pub struct NewEntry<'a> {
    pub kind: Kind,
    pub key: Option<&'a str>,
    pub label: Option<&'a str>,
    pub url: Option<&'a str>,
    pub new_tab: bool,
    pub parent_id: Option<i64>,
    pub position: i32,
}

pub async fn insert<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    entry: NewEntry<'_>,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar!(
        r#"
        INSERT INTO core.menu_entries (kind, key, label, url, new_tab, parent_id, position)
        VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING id
        "#,
        entry.kind.as_str(),
        entry.key,
        entry.label,
        entry.url,
        entry.new_tab,
        entry.parent_id,
        entry.position,
    )
    .fetch_one(executor)
    .await
}

/// Changes what an admin can change about an entry.
pub struct Change<'a> {
    pub label: Option<&'a str>,
    pub url: Option<&'a str>,
    pub new_tab: bool,
    pub parent_id: Option<i64>,
    pub hidden: bool,
}

pub async fn update<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    id: i64,
    change: Change<'_>,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query!(
        r#"
        UPDATE core.menu_entries
        SET label = $2, url = $3, new_tab = $4, parent_id = $5, hidden = $6
        WHERE id = $1
        "#,
        id,
        change.label,
        change.url,
        change.new_tab,
        change.parent_id,
        change.hidden,
    )
    .execute(executor)
    .await?;
    Ok(result.rows_affected() == 1)
}

pub async fn set_position<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    id: i64,
    position: i32,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "UPDATE core.menu_entries SET position = $2 WHERE id = $1",
        id,
        position
    )
    .execute(executor)
    .await?;
    Ok(())
}

/// Deletes a custom section, a folder or a link (never an item or a
/// default section). Links and folders in it go with it; items go back to
/// their default place.
pub async fn delete(tx: &mut sqlx::PgConnection, id: i64) -> Result<bool, sqlx::Error> {
    sqlx::query!(
        r#"
        DELETE FROM core.menu_entries
        WHERE kind = 'link'
          AND (parent_id = $1
               OR parent_id IN (SELECT id FROM core.menu_entries WHERE parent_id = $1 AND kind = 'folder'))
        "#,
        id
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        "DELETE FROM core.menu_entries WHERE kind = 'folder' AND parent_id = $1",
        id
    )
    .execute(&mut *tx)
    .await?;
    let result = sqlx::query!(
        "DELETE FROM core.menu_entries WHERE id = $1 AND kind <> 'item' AND key IS NULL",
        id
    )
    .execute(&mut *tx)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// Back to the default layout.
pub async fn reset<'e>(executor: impl sqlx::PgExecutor<'e>) -> Result<(), sqlx::Error> {
    sqlx::query!("DELETE FROM core.menu_entries")
        .execute(executor)
        .await?;
    Ok(())
}

/// Serializes menu edits, so two admins can't interleave a
/// materialize-and-move.
pub async fn lock(tx: &mut sqlx::PgConnection) -> Result<(), sqlx::Error> {
    sqlx::query!("LOCK TABLE core.menu_entries IN SHARE ROW EXCLUSIVE MODE")
        .execute(&mut *tx)
        .await?;
    Ok(())
}
