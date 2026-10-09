//! Calendar providers: iCloud (CalDAV), Google Calendar (Google's `gws` CLI) and HEY Calendar
//! (the official `hey` CLI). Each speaks in cloud-calendar's own types and prefixes every ID it
//! hands out with its account's name (`icloud:…`), so any later action on that ID goes back to it.
//!
//! Repeating events: listings expand a series into its occurrences, and an occurrence's ID ends in
//! `~<day>` (`~20261009` or `~20261009T090000Z`). Editing or deleting an occurrence reaches the
//! whole series; moving one (a new start or end) is refused, since that would move every one.

use serde::Serialize;
use std::io::{Read, Write};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::config::AccountConfig;
use crate::error::{Error, ErrorKind, Result};
use crate::types::*;

/// What `account list` and the app show about an account.
#[derive(Debug, Clone, Default, Serialize)]
pub struct AccountStatus {
    pub name: String,
    pub provider: String,
    pub label: String,
    /// Signed in and reachable.
    pub ok: bool,
    /// What's wrong, or a short note about the account.
    pub detail: String,
}

/// One account's failure during an operation that went on without it.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct AccountWarning {
    pub account: String,
    pub code: String,
    pub message: String,
}

impl AccountWarning {
    pub fn new(account: &str, e: &Error) -> Self {
        Self { account: account.to_string(), code: e.kind.code().to_string(), message: e.message.clone() }
    }
}

pub trait Provider: Send + Sync {
    /// The account's name, which prefixes its IDs ("icloud").
    fn name(&self) -> &str;
    /// Human name of the service ("iCloud").
    fn label(&self) -> &str;
    /// Whether an ID (calendar or event) belongs to this account.
    fn owns(&self, id: &str) -> bool {
        id.strip_prefix(self.name()).is_some_and(|rest| rest.starts_with(':'))
    }
    fn status(&self) -> AccountStatus;
    /// Signs in again, for an account whose sign-in expired or was revoked. Blocks until done.
    fn sign_in(&self) -> Result<()> {
        Err(Error::bad_request(format!("{} has no sign-in to run here", self.label())))
    }
    fn calendars(&self) -> Result<Vec<Calendar>>;
    /// Events overlapping the range, repeating ones expanded into their occurrences.
    fn events(&self, range: &Range) -> Result<Vec<Event>>;
    /// Adds an event; returns its ID.
    fn create(&self, event: &NewEvent) -> Result<String>;
    fn update(&self, id: &str, change: &EventChange) -> Result<()>;
    fn delete(&self, id: &str) -> Result<()>;
}

/// Providers cloud-calendar knows how to link, for `account add` and its help.
pub const KNOWN_PROVIDERS: &[(&str, &str)] = &[
    ("icloud", "iCloud Calendar, over CalDAV with an app-specific password"),
    ("google", "Google Calendar, through Google's Workspace CLI `gws`"),
    ("hey", "HEY Calendar, through the official `hey` CLI"),
];

/// Opens a configured account.
pub fn open(name: &str, cfg: &AccountConfig) -> Result<Arc<dyn Provider>> {
    match cfg.provider(name) {
        "icloud" => Ok(Arc::new(crate::caldav::ICloud::new(name, cfg))),
        "google" => Ok(Arc::new(crate::google::Google::new(name, cfg))),
        "hey" => Ok(Arc::new(crate::hey::Hey::new(name, cfg))),
        other => Err(Error::new(
            ErrorKind::Config,
            format!("account {name}: unknown provider \"{other}\" (known: {})", KNOWN_PROVIDERS.iter().map(|(p, _)| *p).collect::<Vec<_>>().join(", ")),
        )),
    }
}

/// Account names become ID prefixes, so they are short lowercase words.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 32 && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// The part of an ID after `<account>:`.
pub(crate) fn local_id<'a>(account: &str, label: &str, id: &'a str) -> Result<&'a str> {
    id.strip_prefix(account).and_then(|r| r.strip_prefix(':')).filter(|r| !r.is_empty()).ok_or_else(|| Error::bad_request(format!("{id} is not a {label} ID")))
}

/// `(base, occurrence day)` from a local ID that may end in `~<day>`.
pub fn split_occurrence(local: &str) -> (&str, Option<&str>) {
    match local.rsplit_once('~') {
        Some((base, day)) if !base.is_empty() && (day.len() == 8 || day.len() == 16) && day.chars().all(|c| c.is_ascii_alphanumeric()) => (base, Some(day)),
        _ => (local, None),
    }
}

/// The `~<day>` suffix for an occurrence starting at `start`.
pub fn occurrence_suffix(start: &Time) -> String {
    match start {
        Time::At(t) => format!("~{}", t.format("%Y%m%dT%H%M%SZ")),
        Time::Date(d) => format!("~{}", d.format("%Y%m%d")),
    }
}

/// Refuses moving one occurrence of a series, which would move them all.
pub(crate) fn refuse_series_move(label: &str, change: &EventChange) -> Result<()> {
    if change.moves() {
        return Err(Error::bad_request(format!(
            "this is one occurrence of a repeating {label} event, and changing its time would move the whole series; change the series' time in {label} itself (title, location and notes can change here)"
        )));
    }
    Ok(())
}

/// Checks a change's new ends against the event's current ones.
pub(crate) fn changed_span(change: &EventChange, start: Time, end: Time) -> Result<(Time, Time)> {
    let (s, e) = (change.start.unwrap_or(start), change.end.unwrap_or(end));
    // A new start alone keeps the event's length.
    let e = if change.start.is_some() && change.end.is_none() && s.is_date() == start.is_date() {
        match (start, end, s) {
            (Time::At(a), Time::At(b), Time::At(n)) => Time::At(n + (b - a)),
            (Time::Date(a), Time::Date(b), Time::Date(n)) => Time::Date(n + (b - a)),
            _ => e,
        }
    } else {
        e
    };
    check_span(&s, &e)?;
    Ok((s, e))
}

/// How a command-line tool ended.
pub(crate) enum Run {
    Done { status: ExitStatus, stdout: Vec<u8>, stderr: String },
    Missing,
    TimedOut,
    Failed(std::io::Error),
}

/// Runs a prepared command (stdout and stderr piped, stdin fed when given), killing it after `timeout`.
pub(crate) fn run_command(cmd: &mut Command, stdin: Option<&str>, timeout: Duration) -> Run {
    cmd.stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() }).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Run::Missing,
        Err(e) => return Run::Failed(e),
    };
    if let (Some(input), Some(mut pipe)) = (stdin, child.stdin.take()) {
        let input = input.to_string();
        std::thread::spawn(move || {
            let _ = pipe.write_all(input.as_bytes());
        });
    }
    let reader = |pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut p) = pipe {
                let _ = p.read_to_end(&mut buf);
            }
            buf
        })
    };
    let out = reader(child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let err = reader(child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(15)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Run::TimedOut;
            }
            Err(e) => return Run::Failed(e),
        }
    };
    let stdout = out.join().unwrap_or_default();
    let stderr = String::from_utf8_lossy(&err.join().unwrap_or_default()).trim().to_string();
    Run::Done { status, stdout, stderr }
}

/// Runs an interactive sign-in command attached to the terminal, for up to ten minutes.
pub(crate) fn run_attached(cmd: &mut Command) -> std::io::Result<Option<ExitStatus>> {
    let mut child = cmd.stdin(Stdio::null()).spawn()?;
    let deadline = Instant::now() + Duration::from_secs(600);
    loop {
        match child.try_wait()? {
            Some(s) => return Ok(Some(s)),
            None if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            None => {
                let _ = child.kill();
                let _ = child.wait();
                return Ok(None);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{NaiveDate, TimeZone, Utc};

    #[test]
    fn occurrence_ids() {
        assert_eq!(split_occurrence("123~20261009"), ("123", Some("20261009")));
        assert_eq!(split_occurrence("/cal/a.ics~20261009T090000Z"), ("/cal/a.ics", Some("20261009T090000Z")));
        assert_eq!(split_occurrence("a@group.calendar.google.com/ev1"), ("a@group.calendar.google.com/ev1", None));
        assert_eq!(split_occurrence("/home/~user/a.ics"), ("/home/~user/a.ics", None));
        let t = Time::At(Utc.with_ymd_and_hms(2026, 10, 9, 9, 0, 0).unwrap());
        assert_eq!(occurrence_suffix(&t), "~20261009T090000Z");
        assert_eq!(occurrence_suffix(&Time::Date(NaiveDate::from_ymd_opt(2026, 10, 9).unwrap())), "~20261009");
    }

    #[test]
    fn new_start_keeps_length() {
        let at = |h| Time::At(Utc.with_ymd_and_hms(2026, 10, 9, h, 0, 0).unwrap());
        let change = EventChange { start: Some(at(14)), ..Default::default() };
        assert_eq!(changed_span(&change, at(9), at(10)).unwrap(), (at(14), at(15)));
        let bad = EventChange { end: Some(at(8)), ..Default::default() };
        assert!(changed_span(&bad, at(9), at(10)).is_err());
    }
}
