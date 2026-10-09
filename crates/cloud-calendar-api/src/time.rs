//! Dates and times as people type them: `2026-10-09`, `today`, `tomorrow 14:00`,
//! `2026-10-09 14:00`, `14:00`, `+3` (days from today), or full RFC 3339. Clock times are local.

use chrono::{DateTime, Local, NaiveDate, NaiveTime, TimeZone, Utc};

use crate::error::{Error, Result};
use crate::types::Time;

/// A day: `today`, `tomorrow`, `yesterday`, `+N` / `-N` days, or `YYYY-MM-DD`.
pub fn parse_date(s: &str, today: NaiveDate) -> Result<NaiveDate> {
    let s = s.trim().to_ascii_lowercase();
    let shifted = |n: i64| today.checked_add_signed(chrono::Duration::days(n)).ok_or_else(|| Error::bad_request(format!("{s} is out of range")));
    match s.as_str() {
        "today" => Ok(today),
        "tomorrow" => shifted(1),
        "yesterday" => shifted(-1),
        _ if (s.starts_with('+') || s.starts_with('-')) && s[1..].chars().all(|c| c.is_ascii_digit()) && s.len() > 1 => shifted(s.parse().unwrap_or(0)),
        _ => NaiveDate::parse_from_str(&s, "%Y-%m-%d").map_err(|_| Error::bad_request(format!("\"{s}\" isn't a date (YYYY-MM-DD, today, tomorrow or +N)"))),
    }
}

fn parse_clock(s: &str) -> Option<NaiveTime> {
    NaiveTime::parse_from_str(s, "%H:%M").or_else(|_| NaiveTime::parse_from_str(s, "%H:%M:%S")).ok()
}

/// A local wall-clock time as an instant (the earlier one when a DST change repeats it).
pub fn local_instant(d: NaiveDate, t: NaiveTime) -> Result<DateTime<Utc>> {
    Local
        .from_local_datetime(&d.and_time(t))
        .earliest()
        .map(|x| x.with_timezone(&Utc))
        .ok_or_else(|| Error::bad_request(format!("{d} {t} doesn't exist here (a daylight-saving gap)")))
}

/// A start or end: a date alone means all-day; a date and a clock time, or a clock time today.
pub fn parse_time(s: &str, today: NaiveDate) -> Result<Time> {
    let s = s.trim();
    if let Ok(t) = DateTime::parse_from_rfc3339(s) {
        return Ok(Time::At(t.with_timezone(&Utc)));
    }
    if let Some(t) = parse_clock(s) {
        return local_instant(today, t).map(Time::At);
    }
    let (day, clock) = match s.split_once(['T', ' ']) {
        Some((d, c)) => (d, Some(c.trim())),
        None => (s, None),
    };
    let date = parse_date(day, today)?;
    match clock {
        None => Ok(Time::Date(date)),
        Some(c) => {
            let t = parse_clock(c).ok_or_else(|| Error::bad_request(format!("\"{c}\" isn't a time (HH:MM)")))?;
            local_instant(date, t).map(Time::At)
        }
    }
}

/// A length: `90m`, `1h`, `1h30m`, `2d`.
pub fn parse_length(s: &str) -> Result<chrono::Duration> {
    let bad = || Error::bad_request(format!("\"{s}\" isn't a length (e.g. 30m, 1h, 1h30m, 2d)"));
    let (mut total, mut num) = (0i64, String::new());
    for c in s.trim().to_ascii_lowercase().chars() {
        match c {
            '0'..='9' => num.push(c),
            'd' | 'h' | 'm' => {
                let n: i64 = num.parse().map_err(|_| bad())?;
                num.clear();
                total += n * match c {
                    'd' => 86_400,
                    'h' => 3_600,
                    _ => 60,
                };
            }
            _ => return Err(bad()),
        }
    }
    if !num.is_empty() || total <= 0 {
        return Err(bad());
    }
    Ok(chrono::Duration::seconds(total))
}

/// The end for a start and a length: whole days for an all-day start.
pub fn add_length(start: &Time, len: chrono::Duration) -> Time {
    match start {
        Time::At(t) => Time::At(*t + len),
        Time::Date(d) => Time::Date(*d + chrono::Days::new(len.num_days().max(1) as u64)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 10, d).unwrap()
    }

    #[test]
    fn dates() {
        assert_eq!(parse_date("today", day(9)).unwrap(), day(9));
        assert_eq!(parse_date("Tomorrow", day(9)).unwrap(), day(10));
        assert_eq!(parse_date("+3", day(9)).unwrap(), day(12));
        assert_eq!(parse_date("-1", day(9)).unwrap(), day(8));
        assert_eq!(parse_date("2026-10-20", day(9)).unwrap(), day(20));
        assert!(parse_date("next week", day(9)).is_err());
    }

    #[test]
    fn times() {
        assert_eq!(parse_time("2026-10-12", day(9)).unwrap(), Time::Date(day(12)));
        let at = parse_time("tomorrow 14:00", day(9)).unwrap();
        assert_eq!(at.local_date(), day(10));
        assert_eq!(parse_time("2026-10-10T14:00", day(9)).unwrap(), at);
        assert_eq!(parse_time("2026-10-10 14:00", day(9)).unwrap(), at);
        assert_eq!(parse_time("2026-10-10T12:00:00Z", day(9)).unwrap(), Time::At(Utc.with_ymd_and_hms(2026, 10, 10, 12, 0, 0).unwrap()));
        assert_eq!(parse_time("09:30", day(9)).unwrap().local_date(), day(9));
        assert!(parse_time("2026-10-10 25:00", day(9)).is_err());
    }
}
