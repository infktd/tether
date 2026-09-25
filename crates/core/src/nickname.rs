//! AA's Name Formatter: a format per state, such as
//! `[{corp_ticker}] {character_name:.20}`, filled from the account's main.
//! Fields and format specs follow AA (Python's `str.format`): a field may
//! carry `:[[fill]align][width][.precision]`, and `{{` / `}}` are literal
//! braces.

/// Discord's limit on nicknames, in characters.
pub const MAX_NICKNAME: usize = 32;
/// Longest format an admin may save.
pub const MAX_FORMAT: usize = 100;
/// AA's default.
pub const DEFAULT_FORMAT: &str = "{character_name}";

/// AA's fields.
pub const FIELDS: &[&str] = &[
    "character_name",
    "character_id",
    "corp_ticker",
    "corp_name",
    "corp_id",
    "alliance_ticker",
    "alliance_name",
    "alliance_id",
    "alliance_or_corp_name",
    "alliance_or_corp_ticker",
    "username",
];

/// What a format is filled with: the account's main character. Alliance
/// fields are empty when the corporation isn't in one.
#[derive(Debug, Clone, Default)]
pub struct Parts<'a> {
    pub character_name: &'a str,
    pub character_id: i64,
    pub corp_ticker: &'a str,
    pub corp_name: &'a str,
    pub corp_id: Option<i64>,
    pub alliance_ticker: &'a str,
    pub alliance_name: &'a str,
    pub alliance_id: Option<i64>,
}

impl Parts<'_> {
    fn field(&self, name: &str) -> Option<String> {
        let id = |id: Option<i64>| id.map_or_else(String::new, |i| i.to_string());
        Some(match name {
            "character_name" => self.character_name.to_owned(),
            "character_id" => self.character_id.to_string(),
            "corp_ticker" => self.corp_ticker.to_owned(),
            "corp_name" => self.corp_name.to_owned(),
            "corp_id" => id(self.corp_id),
            "alliance_ticker" => self.alliance_ticker.to_owned(),
            "alliance_name" => self.alliance_name.to_owned(),
            "alliance_id" => id(self.alliance_id),
            "alliance_or_corp_name" => {
                if self.alliance_name.is_empty() {
                    self.corp_name.to_owned()
                } else {
                    self.alliance_name.to_owned()
                }
            }
            "alliance_or_corp_ticker" => {
                if self.alliance_ticker.is_empty() {
                    self.corp_ticker.to_owned()
                } else {
                    self.alliance_ticker.to_owned()
                }
            }
            "username" => username(self.character_name),
            _ => return None,
        })
    }
}

/// AA's username for a character name: anything but letters, digits and
/// `@.+-` becomes `_`.
pub fn username(character_name: &str) -> String {
    character_name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || "@.+-_".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Spec {
    fill: char,
    /// `<`, `>` or `^`; strings default to `<`.
    align: char,
    width: usize,
    precision: Option<usize>,
}

fn parse_spec(spec: &str) -> Result<Spec, String> {
    let bad =
        || format!(":{spec} isn't a format spec this supports ([[fill]align][width][.precision]).");
    let chars: Vec<char> = spec.chars().collect();
    let mut i = 0;
    let (mut fill, mut align) = (' ', '<');
    if chars.len() >= 2 && "<>^".contains(chars[1]) {
        fill = chars[0];
        align = chars[1];
        i = 2;
    } else if !chars.is_empty() && "<>^".contains(chars[0]) {
        align = chars[0];
        i = 1;
    }
    let digits = |from: usize| {
        let mut end = from;
        while end < chars.len() && chars[end].is_ascii_digit() {
            end += 1;
        }
        end
    };
    let end = digits(i);
    let width = if end > i {
        chars[i..end]
            .iter()
            .collect::<String>()
            .parse()
            .map_err(|_| bad())?
    } else {
        0
    };
    i = end;
    let mut precision = None;
    if i < chars.len() && chars[i] == '.' {
        let end = digits(i + 1);
        if end == i + 1 {
            return Err(bad());
        }
        precision = Some(
            chars[i + 1..end]
                .iter()
                .collect::<String>()
                .parse()
                .map_err(|_| bad())?,
        );
        i = end;
    }
    if i != chars.len() || width > MAX_NICKNAME * 4 {
        return Err(bad());
    }
    Ok(Spec {
        fill,
        align,
        width,
        precision,
    })
}

fn apply(value: &str, spec: &Spec) -> String {
    let mut value: String = match spec.precision {
        Some(p) => value.chars().take(p).collect(),
        None => value.to_owned(),
    };
    let len = value.chars().count();
    if len < spec.width {
        let pad = spec.width - len;
        let fill = |n: usize| std::iter::repeat_n(spec.fill, n).collect::<String>();
        value = match spec.align {
            '>' => format!("{}{value}", fill(pad)),
            '^' => format!("{}{value}{}", fill(pad / 2), fill(pad - pad / 2)),
            _ => format!("{value}{}", fill(pad)),
        };
    }
    value
}

enum Piece<'a> {
    Text(String),
    Field(&'a str, Spec),
}

fn parse(format: &str) -> Result<Vec<Piece<'_>>, String> {
    let mut pieces = Vec::new();
    let mut text = String::new();
    let mut rest = format;
    while let Some(c) = rest.chars().next() {
        if rest.starts_with("{{") {
            text.push('{');
            rest = &rest[2..];
        } else if rest.starts_with("}}") {
            text.push('}');
            rest = &rest[2..];
        } else if c == '{' {
            let Some(end) = rest.find('}') else {
                return Err("A { has no matching }.".to_owned());
            };
            let inner = &rest[1..end];
            let (name, spec) = inner.split_once(':').unwrap_or((inner, ""));
            if !FIELDS.contains(&name) {
                return Err(format!(
                    "{{{name}}} isn't a field. Use one of: {}.",
                    FIELDS.join(", ")
                ));
            }
            if !text.is_empty() {
                pieces.push(Piece::Text(std::mem::take(&mut text)));
            }
            pieces.push(Piece::Field(name, parse_spec(spec)?));
            rest = &rest[end + 1..];
        } else if c == '}' {
            return Err("A } has no matching { (write }} for a brace).".to_owned());
        } else {
            text.push(c);
            rest = &rest[c.len_utf8()..];
        }
    }
    if !text.is_empty() {
        pieces.push(Piece::Text(text));
    }
    Ok(pieces)
}

/// Checks a format: known fields and supported specs only, and not too
/// long.
pub fn validate(format: &str) -> Result<(), String> {
    if format.trim().is_empty() {
        return Err("The format can't be empty.".to_owned());
    }
    if format.chars().count() > MAX_FORMAT {
        return Err(format!("The format is at most {MAX_FORMAT} characters."));
    }
    parse(format).map(|_| ())
}

/// Fills in the format. Empty brackets left by a missing alliance are
/// dropped, and the result is trimmed and cut to Discord's limit.
/// An invalid format (never saved: [`validate`]) falls back to the default.
pub fn render(format: &str, parts: &Parts<'_>) -> String {
    let pieces = parse(format)
        .or_else(|_| parse(DEFAULT_FORMAT))
        .unwrap_or_default();
    let mut filled = String::new();
    for piece in pieces {
        match piece {
            Piece::Text(text) => filled.push_str(&text),
            Piece::Field(name, spec) => {
                filled.push_str(&apply(&parts.field(name).unwrap_or_default(), &spec));
            }
        }
    }
    // Empty brackets (a missing alliance) go, with the space beside them;
    // padding a spec asked for stays.
    let tidied = ["[]", "()", "<>", "{}"].iter().fold(filled, |s, empty| {
        s.replace(&format!("{empty} "), "")
            .replace(&format!(" {empty}"), "")
            .replace(empty, "")
    });
    tidied
        .trim()
        .chars()
        .take(MAX_NICKNAME)
        .collect::<String>()
        .trim_end()
        .to_owned()
}

/// A format from the old single template (`{name}`, `{corp}`,
/// `{alliance}`), for the migration and anything still holding one.
pub fn from_template(template: &str) -> String {
    template
        .replace("{name}", "{character_name}")
        .replace("{corp}", "{corp_ticker}")
        .replace("{alliance}", "{alliance_ticker}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parts<'a>(name: &'a str, corp: &'a str, alliance: &'a str) -> Parts<'a> {
        Parts {
            character_name: name,
            character_id: 42,
            corp_ticker: corp,
            corp_name: "Corp Name",
            corp_id: Some(7),
            alliance_ticker: alliance,
            alliance_name: if alliance.is_empty() {
                ""
            } else {
                "Alliance Name"
            },
            alliance_id: (!alliance.is_empty()).then_some(9),
        }
    }

    #[test]
    fn renders_aas_fields() {
        let p = parts("Chribba", "OTHER", "OTHER");
        assert_eq!(
            render("[{corp_ticker}] {character_name}", &p),
            "[OTHER] Chribba"
        );
        assert_eq!(render("{alliance_or_corp_name}", &p), "Alliance Name");
        assert_eq!(
            render(
                "{alliance_or_corp_ticker} {character_id}",
                &parts("X", "SWA", "")
            ),
            "SWA 42"
        );
        assert_eq!(
            render(
                "[{alliance_ticker}] [{corp_ticker}] {character_name}",
                &parts("The Mittani", "SWA", "")
            ),
            "[SWA] The Mittani"
        );
        assert_eq!(
            render("{username}", &parts("The Mittani", "", "")),
            "The_Mittani"
        );
        assert_eq!(render(DEFAULT_FORMAT, &p), "Chribba");
    }

    #[test]
    fn applies_format_specs() {
        let p = parts("A Very Long Character Name", "TICKR", "");
        assert_eq!(render("{character_name:.6}", &p), "A Very");
        assert_eq!(render("[{corp_ticker:>7}]", &p), "[  TICKR]");
        assert_eq!(render("[{corp_ticker:*^9}]", &p), "[**TICKR**]");
        assert_eq!(render("{{{corp_ticker}}}", &p), "{TICKR}");
    }

    #[test]
    fn cuts_to_discords_limit() {
        let long = "A Very Long Character Name Indeed";
        let nick = render(
            "[{corp_ticker}] {character_name}",
            &parts(long, "TICKR", ""),
        );
        assert_eq!(nick.chars().count(), MAX_NICKNAME);
        assert!(nick.starts_with("[TICKR] A Very"));
    }

    #[test]
    fn validates_fields_and_specs() {
        assert!(validate("[{corp_ticker}] {character_name:.20}").is_ok());
        assert!(validate("{nam}").unwrap_err().contains("{nam}"));
        assert!(
            validate("{character_name")
                .unwrap_err()
                .contains("matching")
        );
        assert!(
            validate("{character_name:x}")
                .unwrap_err()
                .contains("format spec")
        );
        assert!(validate("{character_name:.}").is_err());
        assert!(validate("a } b").is_err());
        assert!(validate("").is_err());
        assert!(validate(&"x".repeat(101)).is_err());
    }

    #[test]
    fn converts_the_old_template() {
        assert_eq!(
            from_template("[{corp}] {name} ({alliance})"),
            "[{corp_ticker}] {character_name} ({alliance_ticker})"
        );
    }
}
