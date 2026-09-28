//! The site's own name (DESIGN.md, Identity): an alliance's, say, shown in
//! browser tabs ("Dashboard · Name · Tether") and on the sign-in page,
//! beside Tether's own wordmark. None unless an admin sets one, at setup's
//! end or on System.

use serde_json::json;
use tether_db::accounts::AccountId;
use tether_db::audit::{self, Actor};
use tether_db::settings::{self, SITE_NAME};

use crate::AppState;
use crate::error::AppError;

/// At most this many characters: EVE's own limit for alliance and
/// corporation names, so the prefilled one always fits.
pub const MAX_CHARS: usize = 50;

/// Characters that aren't visible text: controls, and the invisible and
/// direction-changing ones (a right-to-left override would turn the rest
/// of a tab title around), and line and paragraph separators.
fn hidden(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{00AD}'
                | '\u{200B}'..='\u{200F}'
                | '\u{2028}'..='\u{202E}'
                | '\u{2060}'..='\u{206F}'
                | '\u{FEFF}'
                | '\u{FFF9}'..='\u{FFFB}'
        )
}

/// An admin's name, trimmed: none when empty (Tether's alone). One line
/// of at most [`MAX_CHARS`] characters.
pub fn check(name: &str) -> Result<Option<String>, AppError> {
    let name = name.trim();
    if name.is_empty() {
        return Ok(None);
    }
    if name.chars().count() > MAX_CHARS {
        return Err(AppError::bad_request(format!(
            "A site name is at most {MAX_CHARS} characters."
        )));
    }
    if name.chars().any(hidden) {
        return Err(AppError::bad_request(
            "A site name is one line of visible text.",
        ));
    }
    Ok(Some(name.to_owned()))
}

/// The site's name, if one is set.
pub async fn get<'e>(executor: impl sqlx::PgExecutor<'e>) -> Result<Option<String>, sqlx::Error> {
    Ok(settings::get_string(executor, SITE_NAME)
        .await?
        .filter(|name| !name.trim().is_empty()))
}

/// Sets the site's name (none: Tether's alone), audited.
pub async fn set(
    state: &AppState,
    actor: AccountId,
    name: &str,
) -> Result<Option<String>, AppError> {
    let name = check(name)?;
    let mut tx = state.db.begin().await?;
    match &name {
        Some(name) => settings::set(&mut *tx, SITE_NAME, json!(name)).await?,
        None => settings::delete(&mut *tx, SITE_NAME).await?,
    }
    audit::record(
        &mut *tx,
        Actor::Account(actor),
        "site.name",
        None,
        json!({ "name": name }),
    )
    .await?;
    tx.commit().await?;
    Ok(name)
}

#[cfg(test)]
mod tests {
    use super::check;

    #[test]
    fn a_site_name_is_one_short_line() {
        assert_eq!(
            check("  Some Alliance  ").ok().flatten().as_deref(),
            Some("Some Alliance")
        );
        assert_eq!(check("   ").ok().flatten(), None);
        assert!(check(&"x".repeat(51)).is_err());
        assert!(check(&"é".repeat(50)).is_ok());
        assert!(check("Two\nlines").is_err());
        assert!(check("Two\u{2028}lines").is_err());
        assert!(check("a\u{202E}b").is_err());
        assert!(check("zero\u{200B}width").is_err());
    }
}
