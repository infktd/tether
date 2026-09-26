//! Killmail links as pilots paste them, and what zKillboard and ESI answer
//! about a kill.

use chrono::{DateTime, NaiveDateTime, Utc};

/// A kill named by a link: its id, and its hash when the link has it
/// (ESI links do; zKillboard's don't).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    pub id: i64,
    pub hash: Option<String>,
}

/// `https://zkillboard.com/kill/<id>/`, or an ESI killmail link
/// `https://esi.evetech.net/[latest|v1/]killmails/<id>/<hash>/`.
pub fn parse_link(link: &str) -> Option<Link> {
    let link = link.trim();
    let rest = link
        .strip_prefix("https://")
        .or_else(|| link.strip_prefix("http://"))?;
    let (host, path) = rest.split_once('/')?;
    let path = path.split(['?', '#']).next().unwrap_or("");
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    match host.to_ascii_lowercase().as_str() {
        "zkillboard.com" | "www.zkillboard.com" => match parts.as_slice() {
            ["kill", id] => Some(Link {
                id: kill_id(id)?,
                hash: None,
            }),
            _ => None,
        },
        "esi.evetech.net" => {
            let parts = match parts.as_slice() {
                [version, rest @ ..] if *version == "latest" || version.starts_with('v') => rest,
                all => all,
            };
            match parts {
                ["killmails", id, hash] if is_hash(hash) => Some(Link {
                    id: kill_id(id)?,
                    hash: Some(hash.to_ascii_lowercase()),
                }),
                _ => None,
            }
        }
        _ => None,
    }
}

fn kill_id(text: &str) -> Option<i64> {
    text.parse::<i64>().ok().filter(|id| *id > 0)
}

/// A killmail hash: 40 hex digits.
pub fn is_hash(text: &str) -> bool {
    text.len() == 40 && text.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The most a kill may be worth: more than any ship. A bigger value from
/// zKillboard is refused rather than becoming a payout.
pub const MAX_VALUE: f64 = 1e13;

/// What zKillboard knows of a kill: its hash, and its value in ISK.
#[derive(Debug, Clone, PartialEq)]
pub struct Zkb {
    pub hash: String,
    pub total_value: f64,
}

/// zKillboard's `api/killID/<id>/` answer: `[{"killmail_id", "zkb":
/// {"hash", "totalValue", ...}}]`.
pub fn parse_zkb(body: &str, id: i64) -> Option<Zkb> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    let kill = value
        .as_array()?
        .iter()
        .find(|k| k["killmail_id"].as_i64() == Some(id))?;
    let hash = kill["zkb"]["hash"].as_str().filter(|h| is_hash(h))?;
    let total_value = kill["zkb"]["totalValue"]
        .as_f64()
        .filter(|v| v.is_finite() && (0.0..=MAX_VALUE).contains(v))?;
    Some(Zkb {
        hash: hash.to_ascii_lowercase(),
        total_value,
    })
}

/// The parts of an ESI killmail SRP needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Killmail {
    pub time: DateTime<Utc>,
    pub solar_system_id: i64,
    /// None for structures and other losses without a pilot.
    pub victim_character_id: Option<i64>,
    pub ship_type_id: i64,
}

pub fn parse_killmail(body: &str) -> Option<Killmail> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    let time = value["killmail_time"].as_str()?;
    let time = DateTime::parse_from_rfc3339(time)
        .map(|t| t.with_timezone(&Utc))
        .or_else(|_| {
            NaiveDateTime::parse_from_str(time.trim_end_matches('Z'), "%Y-%m-%dT%H:%M:%S")
                .map(|t| t.and_utc())
        })
        .ok()?;
    Some(Killmail {
        time,
        solar_system_id: value["solar_system_id"].as_i64().unwrap_or_default(),
        victim_character_id: value["victim"]["character_id"].as_i64(),
        ship_type_id: value["victim"]["ship_type_id"].as_i64()?,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    const HASH: &str = "0123456789abcdef0123456789abcdef01234567";

    #[test]
    fn links() {
        for (text, id, hash) in [
            ("https://zkillboard.com/kill/128570923/", 128570923, None),
            ("https://zkillboard.com/kill/128570923", 128570923, None),
            ("  http://www.zkillboard.com/kill/5/#top ", 5, None),
            (
                &format!("https://esi.evetech.net/latest/killmails/7/{HASH}/"),
                7,
                Some(HASH),
            ),
            (
                &format!("https://esi.evetech.net/v1/killmails/7/{HASH}/?datasource=tranquility"),
                7,
                Some(HASH),
            ),
            (
                &format!(
                    "https://esi.evetech.net/killmails/7/{}",
                    HASH.to_uppercase()
                ),
                7,
                Some(HASH),
            ),
        ] {
            assert_eq!(
                parse_link(text),
                Some(Link {
                    id,
                    hash: hash.map(str::to_owned)
                }),
                "{text}"
            );
        }
        for bad in [
            "",
            "128570923",
            "https://zkillboard.com/character/1/",
            "https://zkillboard.com/kill/abc/",
            "https://zkillboard.com/kill/-1/",
            "https://zkillboard.com.evil.example/kill/1/",
            "https://evil.example/kill/1/",
            "https://esi.evetech.net/latest/killmails/7/short/",
            "https://esi.evetech.net/latest/killmails/7/",
            "ftp://zkillboard.com/kill/1/",
        ] {
            assert_eq!(parse_link(bad), None, "{bad}");
        }
    }

    #[test]
    fn zkb_answers() {
        let body = format!(
            r#"[{{"killmail_id": 7, "zkb": {{"hash": "{HASH}", "totalValue": 123456789.5, "points": 1}}}}]"#
        );
        assert_eq!(
            parse_zkb(&body, 7),
            Some(Zkb {
                hash: HASH.to_owned(),
                total_value: 123456789.5
            })
        );
        assert_eq!(parse_zkb(&body, 8), None);
        let huge =
            format!(r#"[{{"killmail_id": 7, "zkb": {{"hash": "{HASH}", "totalValue": 1e18}}}}]"#);
        assert_eq!(parse_zkb(&huge, 7), None);
        assert_eq!(parse_zkb("[]", 7), None);
        assert_eq!(parse_zkb("not json", 7), None);
    }

    #[test]
    fn killmails() {
        let body = r#"{"killmail_id": 7, "killmail_time": "2026-09-20T19:04:05Z",
            "solar_system_id": 30000142,
            "victim": {"character_id": 196379789, "corporation_id": 1, "ship_type_id": 587},
            "attackers": []}"#;
        let km = parse_killmail(body).unwrap();
        assert_eq!(km.victim_character_id, Some(196379789));
        assert_eq!(km.ship_type_id, 587);
        assert_eq!(km.time.to_rfc3339(), "2026-09-20T19:04:05+00:00");
        let structure = r#"{"killmail_time": "2026-09-20T19:04:05Z", "solar_system_id": 1,
            "victim": {"corporation_id": 1, "ship_type_id": 35832}}"#;
        assert_eq!(parse_killmail(structure).unwrap().victim_character_id, None);
    }
}
