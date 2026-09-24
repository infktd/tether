//! The Discord nickname template, such as `[{corp}] {name}`.

/// Discord's limit on nicknames, in characters.
pub const MAX_NICKNAME: usize = 32;
/// Longest template an admin may save.
pub const MAX_TEMPLATE: usize = 100;
pub const PLACEHOLDERS: &[&str] = &["{name}", "{corp}", "{alliance}"];

/// What the template is filled with: the account's main character.
#[derive(Debug, Clone, Default)]
pub struct Parts<'a> {
    pub name: &'a str,
    pub corp: &'a str,
    /// Empty when the corporation isn't in an alliance.
    pub alliance: &'a str,
}

/// Checks a template: known placeholders only, `{name}` required (a
/// nickname has to say who it is), and not too long.
pub fn validate(template: &str) -> Result<(), String> {
    if template.chars().count() > MAX_TEMPLATE {
        return Err(format!(
            "The template is at most {MAX_TEMPLATE} characters."
        ));
    }
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        let after = &rest[start..];
        let Some(end) = after.find('}') else {
            return Err("A { has no matching }.".to_owned());
        };
        let placeholder = &after[..=end];
        if !PLACEHOLDERS.contains(&placeholder) {
            return Err(format!(
                "{placeholder} isn't a placeholder. Use {}.",
                PLACEHOLDERS.join(", ")
            ));
        }
        rest = &after[end + 1..];
    }
    if !template.contains("{name}") {
        return Err("The template needs {name}.".to_owned());
    }
    Ok(())
}

/// Fills in the template. Empty brackets left by a missing alliance are
/// dropped, spaces are collapsed, and the result is cut to Discord's limit.
pub fn render(template: &str, parts: &Parts<'_>) -> String {
    let filled = template
        .replace("{name}", parts.name)
        .replace("{corp}", parts.corp)
        .replace("{alliance}", parts.alliance);
    let tidied = ["[]", "()", "<>", "{}"]
        .iter()
        .fold(filled, |s, empty| s.replace(empty, ""));
    let collapsed = tidied.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed
        .chars()
        .take(MAX_NICKNAME)
        .collect::<String>()
        .trim_end()
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parts<'a>(name: &'a str, corp: &'a str, alliance: &'a str) -> Parts<'a> {
        Parts {
            name,
            corp,
            alliance,
        }
    }

    #[test]
    fn renders_tickers_and_name() {
        assert_eq!(
            render("[{corp}] {name}", &parts("Chribba", "OTHER", "OTHER")),
            "[OTHER] Chribba"
        );
        assert_eq!(
            render(
                "[{alliance}] [{corp}] {name}",
                &parts("The Mittani", "SWA", "")
            ),
            "[SWA] The Mittani"
        );
    }

    #[test]
    fn cuts_to_discords_limit() {
        let long = "A Very Long Character Name Indeed";
        let nick = render("[{corp}] {name}", &parts(long, "TICKR", ""));
        assert_eq!(nick.chars().count(), MAX_NICKNAME);
        assert!(nick.starts_with("[TICKR] A Very"));
    }

    #[test]
    fn validates_placeholders() {
        assert!(validate("[{corp}] {name}").is_ok());
        assert!(validate("{name} ({alliance})").is_ok());
        assert!(validate("[{corp}]").unwrap_err().contains("{name}"));
        assert!(validate("{nam} {name}").unwrap_err().contains("{nam}"));
        assert!(validate("{name").unwrap_err().contains("matching"));
        assert!(validate(&"x".repeat(101)).is_err());
    }
}
