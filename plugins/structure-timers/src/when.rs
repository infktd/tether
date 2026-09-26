//! EVE time in and out: what pilots type, and countdowns.

use chrono::{DateTime, Duration, NaiveDateTime, Utc};

/// How far from now a timer may be (a typo guard).
pub const MAX_DISTANCE: Duration = Duration::days(366);

/// An EVE time as typed: `2026-09-30 18:00`, or `2026.09.30 18:00` as the
/// game shows it, seconds optional.
pub fn parse_eve_time(text: &str) -> Option<DateTime<Utc>> {
    let text = text.trim().trim_end_matches('Z').replace('T', " ");
    let (date, clock) = text.split_once(' ')?;
    let normal = format!("{} {}", date.replace('.', "-"), clock.trim());
    ["%Y-%m-%d %H:%M:%S", "%Y-%m-%d %H:%M"]
        .iter()
        .find_map(|format| NaiveDateTime::parse_from_str(&normal, format).ok())
        .map(|t| t.and_utc())
}

/// The form's way of showing a stored time.
pub fn eve_time_text(t: DateTime<Utc>) -> String {
    t.format("%Y-%m-%d %H:%M").to_string()
}

/// Where a timer lands, from the EVE time or the time left (days, hours,
/// minutes, as the game shows it). Exactly one of the two.
pub fn timer_time(
    now: DateTime<Utc>,
    eve_time: &str,
    left: [&str; 3],
) -> Result<DateTime<Utc>, &'static str> {
    let given = |s: &str| !s.trim().is_empty();
    let has_left = left.iter().any(|s| given(s));
    let at = match (given(eve_time), has_left) {
        (true, true) => return Err("Give either the EVE time or the time left, not both."),
        (false, false) => return Err("Give the EVE time, or the time left."),
        (true, false) => parse_eve_time(eve_time)
            .ok_or("Write the EVE time as YYYY-MM-DD HH:MM, such as 2026-09-30 18:00.")?,
        (false, true) => {
            let number = |s: &str| -> Result<i64, &'static str> {
                if given(s) {
                    s.trim()
                        .parse()
                        .map_err(|_| "The time left is whole numbers.")
                } else {
                    Ok(0)
                }
            };
            let [days, hours, minutes] = left;
            now + Duration::days(number(days)?)
                + Duration::hours(number(hours)?)
                + Duration::minutes(number(minutes)?)
        }
    };
    if (at - now).abs() > MAX_DISTANCE {
        return Err("That's more than a year away: check the date.");
    }
    Ok(at)
}

/// `2d 4h 13m`, `4h 13m`, `13m`; `now` under a minute; `… ago` when past.
pub fn countdown(now: DateTime<Utc>, at: DateTime<Utc>) -> String {
    let delta = at - now;
    let past = delta < Duration::zero();
    let minutes = delta.num_minutes().abs();
    if minutes == 0 {
        return "now".to_owned();
    }
    let (days, hours, minutes) = (minutes / 1440, minutes / 60 % 24, minutes % 60);
    let text = if days > 0 {
        format!("{days}d {hours}h {minutes}m")
    } else if hours > 0 {
        format!("{hours}h {minutes}m")
    } else {
        format!("{minutes}m")
    };
    if past { format!("{text} ago") } else { text }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn at(text: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(text).unwrap().to_utc()
    }

    #[test]
    fn eve_times_parse_as_typed_or_as_the_game_shows_them() {
        let want = at("2026-09-30T18:00:00Z");
        for text in [
            "2026-09-30 18:00",
            " 2026.09.30 18:00 ",
            "2026-09-30 18:00:00",
            "2026-09-30T18:00:00Z",
        ] {
            assert_eq!(parse_eve_time(text), Some(want), "{text}");
        }
        for text in ["", "tomorrow", "2026-09-30", "2026-13-01 18:00", "18:00"] {
            assert_eq!(parse_eve_time(text), None, "{text}");
        }
        assert_eq!(eve_time_text(want), "2026-09-30 18:00");
    }

    #[test]
    fn a_timer_lands_at_the_eve_time_or_after_the_time_left() {
        let now = at("2026-09-26T12:00:00Z");
        assert_eq!(
            timer_time(now, "2026-09-30 18:00", ["", "", ""]),
            Ok(at("2026-09-30T18:00:00Z"))
        );
        assert_eq!(
            timer_time(now, "", ["1", "2", "30"]),
            Ok(at("2026-09-27T14:30:00Z"))
        );
        assert_eq!(
            timer_time(now, "", ["", "", "45"]),
            Ok(at("2026-09-26T12:45:00Z"))
        );
        assert!(timer_time(now, "2026-09-30 18:00", ["1", "", ""]).is_err());
        assert!(timer_time(now, "", ["", "", ""]).is_err());
        assert!(timer_time(now, "soon", ["", "", ""]).is_err());
        assert!(timer_time(now, "2031-01-01 00:00", ["", "", ""]).is_err());
    }

    #[test]
    fn countdowns_read_like_the_game() {
        let now = at("2026-09-26T12:00:00Z");
        assert_eq!(countdown(now, at("2026-09-28T16:13:30Z")), "2d 4h 13m");
        assert_eq!(countdown(now, at("2026-09-26T16:13:00Z")), "4h 13m");
        assert_eq!(countdown(now, at("2026-09-26T12:13:00Z")), "13m");
        assert_eq!(countdown(now, at("2026-09-26T12:00:30Z")), "now");
        assert_eq!(countdown(now, at("2026-09-26T09:00:00Z")), "3h 0m ago");
    }
}
