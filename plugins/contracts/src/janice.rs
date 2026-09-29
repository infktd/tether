//! Janice appraisals: the link in a contract's description, the
//! appraisal's buy total (read once, with the admin's API key), and
//! whether the contract's price matches it.

use tether_plugin_sdk::http;

const HOST: &str = "janice.e-351.com";

/// The appraisal code a contract's description links
/// (`https://janice.e-351.com/a/Ab12Cd`), if any.
pub fn code(description: &str) -> Option<String> {
    let at = description.find(&format!("{HOST}/a/"))?;
    let rest = &description[at + HOST.len() + 3..];
    let code: String = rest
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric())
        .collect();
    (4..=16).contains(&code.len()).then_some(code)
}

/// The appraisal's page, for the card.
pub fn link(code: &str) -> String {
    format!("https://{HOST}/a/{code}")
}

/// What reading an appraisal came to.
#[derive(Debug, Clone, PartialEq)]
pub enum Appraisal {
    /// Its buy total (Jita buy, as Janice priced it), in ISK, what it
    /// appraised (type id and amount), and when it was made.
    Buy {
        buy: f64,
        items: Vec<(i64, i64)>,
        created: Option<chrono::DateTime<chrono::Utc>>,
    },
    /// Not read, and why, for the card and the page.
    NotChecked(String),
}

/// An appraisal older than its contract by more than this doesn't count:
/// prices it was made at may be long gone.
pub const MAX_AGE_HOURS: i64 = 24;

/// Why an appraisal read doesn't vouch for a contract, if it doesn't: it
/// appraised other items (the issuer links it, so it must be of what the
/// contract hands over), or it's older than the contract by more than a
/// day.
pub fn problem(
    appraised: &[(i64, i64)],
    created: Option<chrono::DateTime<chrono::Utc>>,
    included: &[(i64, i64)],
    issued: chrono::DateTime<chrono::Utc>,
) -> Option<String> {
    let totals = |items: &[(i64, i64)]| {
        let mut out: std::collections::BTreeMap<i64, i64> = std::collections::BTreeMap::new();
        for (type_id, amount) in items {
            *out.entry(*type_id).or_default() += amount;
        }
        out
    };
    if totals(appraised) != totals(included) {
        return Some("the appraisal isn't of the contract's items".to_owned());
    }
    let age = issued - created?;
    (age > chrono::Duration::hours(MAX_AGE_HOURS)).then(|| {
        format!(
            "the appraisal is {} days older than the contract",
            age.num_days().max(1)
        )
    })
}

/// Reads an appraisal's buy total. Only an answer Janice gave is final;
/// `Err` is worth trying again later (Janice down, too many requests).
pub fn read(code: &str) -> Result<Appraisal, String> {
    let request = http::Request::get(format!("https://{HOST}/api/rest/v2/appraisal/{code}"))
        .header("accept", "application/json")
        .secret("janice_api_key");
    let answer = match http::send(&request) {
        Ok(answer) => answer,
        Err(http::Error::NotAllowed(why)) if why.contains("hasn't been entered") => {
            return Ok(Appraisal::NotChecked(
                "no Janice API key entered".to_owned(),
            ));
        }
        Err(http::Error::NotAllowed(why)) => return Ok(Appraisal::NotChecked(why)),
        Err(err) => return Err(format!("{err:?}")),
    };
    match answer.status {
        200 => {}
        401 | 403 => {
            return Ok(Appraisal::NotChecked(
                "Janice refused the API key".to_owned(),
            ));
        }
        404 => return Ok(Appraisal::NotChecked("appraisal not found".to_owned())),
        status => return Err(format!("Janice answered HTTP {status}")),
    }
    let body: serde_json::Value = answer
        .text()
        .and_then(|t| serde_json::from_str(t).ok())
        .unwrap_or_default();
    let Some(buy) = body["effectivePrices"]["totalBuyPrice"]
        .as_f64()
        .filter(|v| v.is_finite() && *v >= 0.0)
    else {
        return Ok(Appraisal::NotChecked(
            "Janice's answer had no buy total".to_owned(),
        ));
    };
    let items = body["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|i| Some((i["itemType"]["eid"].as_i64()?, i["amount"].as_i64()?)))
        .collect();
    let created = body["created"]
        .as_str()
        .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
        .map(|t| t.to_utc());
    Ok(Appraisal::Buy {
        buy,
        items,
        created,
    })
}

/// How far the price is from the appraisal's buy total, in percent
/// (positive: asks more), and whether that's within `tolerance` percent.
pub fn compare(price: f64, buy: f64, tolerance: f64) -> (f64, bool) {
    if buy <= 0.0 {
        return (0.0, price <= 0.0);
    }
    let off = (price - buy) / buy * 100.0;
    (off, off.abs() <= tolerance + 1e-9)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_appraisal_linked() {
        assert_eq!(
            code("Ore buyback https://janice.e-351.com/a/Ab12Cd thanks"),
            Some("Ab12Cd".to_owned())
        );
        assert_eq!(code("janice.e-351.com/a/Xy9876"), Some("Xy9876".to_owned()));
        assert_eq!(code("no link here"), None);
        assert_eq!(code("https://janice.e-351.com/a/"), None);
        assert_eq!(code("https://evil.example/a/Ab12Cd"), None);
        assert_eq!(link("Ab12Cd"), "https://janice.e-351.com/a/Ab12Cd");
    }

    #[test]
    fn an_appraisal_vouches_only_for_its_own_items_and_time() {
        let issued = chrono::Utc::now();
        let items = [(62516, 13803), (62517, 6359)];
        // Split stacks count together.
        let appraised = [(62517, 6000), (62516, 13803), (62517, 359)];
        assert_eq!(problem(&appraised, Some(issued), &items, issued), None);
        assert_eq!(
            problem(&[(62516, 13803)], Some(issued), &items, issued).as_deref(),
            Some("the appraisal isn't of the contract's items")
        );
        assert_eq!(
            problem(
                &items,
                Some(issued - chrono::Duration::days(3)),
                &items,
                issued
            )
            .as_deref(),
            Some("the appraisal is 3 days older than the contract")
        );
        assert_eq!(problem(&items, None, &items, issued), None);
    }

    #[test]
    fn prices_match_within_the_tolerance() {
        assert_eq!(compare(1_000.0, 1_000.0, 1.0), (0.0, true));
        let (off, ok) = compare(990.0, 1_000.0, 1.0);
        assert!((off + 1.0).abs() < 1e-9 && ok);
        let (off, ok) = compare(1_120.0, 1_000.0, 1.0);
        assert!((off - 12.0).abs() < 1e-9 && !ok);
        assert!(!compare(1_001.0, 1_000.0, 0.0).1);
        assert!(compare(0.0, 0.0, 1.0).1);
    }
}
