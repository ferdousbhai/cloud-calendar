//! Calendars and events as every provider hands them out.
//!
//! Times: a timed event starts and ends at instants (`Time::At`, UTC inside, shown in local time);
//! an all-day event spans dates (`Time::Date`) with an exclusive end, as iCalendar has it: a
//! one-day event on the 9th runs from the 9th to the 10th.

use chrono::{DateTime, Local, NaiveDate, TimeZone, Utc};
use serde::{Serialize, Serializer};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Time {
    At(DateTime<Utc>),
    Date(NaiveDate),
}

impl Time {
    /// The instant it stands for: a date is its local midnight.
    pub fn instant(&self) -> DateTime<Utc> {
        match self {
            Time::At(t) => *t,
            Time::Date(d) => local_midnight(*d),
        }
    }

    /// The local calendar date it falls on.
    pub fn local_date(&self) -> NaiveDate {
        match self {
            Time::At(t) => t.with_timezone(&Local).date_naive(),
            Time::Date(d) => *d,
        }
    }

    pub fn is_date(&self) -> bool {
        matches!(self, Time::Date(_))
    }
}

/// A date's first instant in the local time zone (the earliest one, across a DST change).
pub fn local_midnight(d: NaiveDate) -> DateTime<Utc> {
    let naive = d.and_hms_opt(0, 0, 0).unwrap_or_default();
    Local.from_local_datetime(&naive).earliest().map(|t| t.with_timezone(&Utc)).unwrap_or_else(|| naive.and_utc())
}

impl std::fmt::Display for Time {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Time::At(t) => f.write_str(&t.with_timezone(&Local).to_rfc3339_opts(chrono::SecondsFormat::Secs, false)),
            Time::Date(d) => write!(f, "{}", d.format("%Y-%m-%d")),
        }
    }
}

impl Serialize for Time {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

/// A window of time to read events in, `[start, end)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Range {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

impl Range {
    /// Local days `from` through `from + days - 1`.
    pub fn days(from: NaiveDate, days: u32) -> Self {
        let to = from + chrono::Days::new(u64::from(days.max(1)));
        Self { start: local_midnight(from), end: local_midnight(to) }
    }

    /// Whether an event from `start` to `end` overlaps the window.
    pub fn overlaps(&self, start: &Time, end: &Time) -> bool {
        let (s, e) = (start.instant(), end.instant());
        s < self.end && (e > self.start || (e == s && s >= self.start))
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Calendar {
    /// `<account>:<provider's own id>`.
    pub id: String,
    pub account: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// You can add events to it.
    pub writable: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Event {
    /// `<account>:<provider's own id>`; an occurrence of a repeating event carries the day after `@`.
    pub id: String,
    pub account: String,
    pub calendar_id: String,
    pub calendar: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    pub title: String,
    pub start: Time,
    pub end: Time,
    pub all_day: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    /// One occurrence of a repeating event: edits and deletes reach the whole series.
    pub recurring: bool,
}

impl Event {
    /// Start order, all-day events first on their day.
    pub fn sort_key(&self) -> (chrono::DateTime<Utc>, bool, String) {
        (self.start.instant(), !self.all_day, self.title.to_lowercase())
    }
}

/// A new event. Dates for both ends make it all-day.
#[derive(Debug, Clone, PartialEq)]
pub struct NewEvent {
    pub calendar_id: String,
    pub title: String,
    pub start: Time,
    pub end: Time,
    pub location: Option<String>,
    pub notes: Option<String>,
}

/// A change to an event: only what is `Some` changes; an empty location or notes clears it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EventChange {
    pub title: Option<String>,
    pub start: Option<Time>,
    pub end: Option<Time>,
    pub location: Option<String>,
    pub notes: Option<String>,
}

impl EventChange {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    pub fn moves(&self) -> bool {
        self.start.is_some() || self.end.is_some()
    }
}

/// Checks an event's ends: both dates or both instants, and the end after the start.
pub fn check_span(start: &Time, end: &Time) -> crate::Result<()> {
    if start.is_date() != end.is_date() {
        return Err(crate::Error::bad_request("an event's start and end must both be dates (all-day) or both be times"));
    }
    if end <= start {
        return Err(crate::Error::bad_request(format!("the end ({end}) must come after the start ({start})")));
    }
    Ok(())
}
