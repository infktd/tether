//! EVE time in and out: what FCs type, and countdowns.

use chrono::{DateTime, Datelike, Duration, NaiveDateTime, Utc};

/// An EVE time as typed: `2026-09-30 18:00`, or `2026.09.30 18:00` as the
/// game shows it, seconds optional. Years 1 to 9999, as AA's (Python's
/// datetime): anything else Postgres or RFC 3339 can't hold.
pub fn parse_eve_time(text: &str) -> Option<DateTime<Utc>> {
    let text = text.trim().trim_end_matches('Z').replace('T', " ");
    let (date, clock) = text.split_once(' ')?;
    let normal = format!("{} {}", date.replace('.', "-"), clock.trim());
    ["%Y-%m-%d %H:%M:%S", "%Y-%m-%d %H:%M"]
        .iter()
        .find_map(|format| NaiveDateTime::parse_from_str(&normal, format).ok())
        .filter(|t| (1..=9999).contains(&t.year()))
        .map(|t| t.and_utc())
}

/// The form's way of showing a stored time.
pub fn eve_time_text(t: DateTime<Utc>) -> String {
    t.format("%Y-%m-%d %H:%M").to_string()
}

/// An operation's start as typed. Any time, as AA's form takes: past
/// operations can be posted and edited too.
pub fn start_time(text: &str) -> Result<DateTime<Utc>, &'static str> {
    parse_eve_time(text)
        .ok_or("Write the start as YYYY-MM-DD HH:MM in EVE time, such as 2026-09-30 18:00.")
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
        for text in [
            "",
            "tomorrow",
            "2026-09-30",
            "2026-13-01 18:00",
            "18:00",
            "+10000-01-01 00:00",
            "-5000-01-01 00:00",
            "0000-01-01 00:00",
        ] {
            assert_eq!(parse_eve_time(text), None, "{text}");
        }
        assert_eq!(eve_time_text(want), "2026-09-30 18:00");
    }

    #[test]
    fn a_start_is_any_eve_time() {
        assert_eq!(
            start_time("2026-09-30 18:00"),
            Ok(at("2026-09-30T18:00:00Z"))
        );
        // Past and far-off operations too, as AA's form.
        assert_eq!(
            start_time("2026.09.20 19:30"),
            Ok(at("2026-09-20T19:30:00Z"))
        );
        assert_eq!(
            start_time("2031-01-01 00:00"),
            Ok(at("2031-01-01T00:00:00Z"))
        );
        assert!(start_time("soon").is_err());
        assert!(start_time("").is_err());
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
