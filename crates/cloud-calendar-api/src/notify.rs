//! Event notifications through Omarchy's own notification path, `omarchy-notification-send`
//! (which calls the notification daemon directly; clicking one opens the app). `cloud-calendar
//! notify`, run every minute by a systemd user timer, sends what is due.
//!
//! The next day's timed events are cached for `REFRESH` so the timer doesn't ask every account
//! every minute; what was sent is remembered so nothing is sent twice. All-day events aren't
//! notified.

use chrono::{DateTime, Local, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::process::Command;
use std::time::Duration;

use crate::config;
use crate::provider::{AccountWarning, Run, run_command};
use crate::types::{Event, Range, Time};
use crate::unified::Calendars;

pub const COMMAND_ENV: &str = "CLOUD_CALENDAR_NOTIFY_COMMAND";
/// How long the cached day of events is trusted.
pub const REFRESH: chrono::Duration = chrono::Duration::minutes(10);
const LOOKAHEAD: chrono::Duration = chrono::Duration::hours(24);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Upcoming {
    pub id: String,
    pub title: String,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    pub calendar: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct State {
    #[serde(default)]
    pub fetched_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub upcoming: Vec<Upcoming>,
    /// Sent notifications by key, with the event's start (to forget them once it's over).
    #[serde(default)]
    pub sent: BTreeMap<String, DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Sent {
    pub id: String,
    pub title: String,
    pub start: DateTime<Utc>,
    pub minutes_before: u32,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Report {
    pub sent: Vec<Sent>,
    /// Notifications that couldn't be shown, and why.
    pub failed: Vec<String>,
    pub refreshed: bool,
    pub warnings: Vec<AccountWarning>,
}

pub fn state_path() -> std::path::PathBuf {
    config::state_dir().join("notify.json")
}

fn load_state() -> State {
    std::fs::read(state_path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn save_state(s: &State) {
    if let Ok(bytes) = serde_json::to_vec_pretty(s) {
        let _ = config::write_private(&state_path(), &bytes);
    }
}

pub fn upcoming(events: &[Event]) -> Vec<Upcoming> {
    events
        .iter()
        .filter_map(|e| match (e.start, e.end) {
            (Time::At(start), Time::At(end)) => Some(Upcoming { id: e.id.clone(), title: e.title.clone(), start, end, location: e.location.clone(), calendar: e.calendar.clone() }),
            _ => None,
        })
        .collect()
}

/// What is due at `now`: per event, the nearest lead time that has passed while the event hasn't
/// started, unless already sent; every passed lead is marked sent so it never fires later.
pub fn due(upcoming: &[Upcoming], leads: &[u32], now: DateTime<Utc>, sent: &mut BTreeMap<String, DateTime<Utc>>) -> Vec<(Upcoming, u32)> {
    let mut out = Vec::new();
    for e in upcoming {
        if e.start <= now {
            continue;
        }
        let mut passed: Vec<u32> = leads.iter().copied().filter(|m| e.start - chrono::Duration::minutes(i64::from(*m)) <= now).collect();
        passed.sort_unstable();
        let key = |m: u32| format!("{}|{}|{m}", e.id, e.start.timestamp());
        let fresh = passed.iter().any(|m| !sent.contains_key(&key(*m)));
        for m in &passed {
            sent.insert(key(*m), e.start);
        }
        if fresh && let Some(m) = passed.first() {
            out.push((e.clone(), *m));
        }
    }
    out
}

/// The notification's body: "in 10 min · 14:00–15:00 · Room 2 · Work".
pub fn body(e: &Upcoming, now: DateTime<Utc>) -> String {
    let mins = (e.start - now).num_seconds().max(0).div_euclid(60) + i64::from((e.start - now).num_seconds() % 60 > 0);
    let when = if mins <= 1 { "starting now".to_string() } else { format!("in {mins} min") };
    let (s, t) = (e.start.with_timezone(&Local), e.end.with_timezone(&Local));
    let mut parts = vec![when, format!("{}–{}", s.format("%H:%M"), t.format("%H:%M"))];
    if let Some(l) = &e.location {
        parts.push(l.clone());
    }
    if !e.calendar.is_empty() {
        parts.push(e.calendar.clone());
    }
    parts.join(" · ")
}

/// Shows one notification; clicking it opens the app. `CLOUD_CALENDAR_NOTIFY_COMMAND` names
/// another program taking the same arguments (the tests' recorder).
pub fn send(title: &str, body: &str) -> Result<(), String> {
    let program = std::env::var(COMMAND_ENV).ok().filter(|c| !c.trim().is_empty()).unwrap_or_else(|| "omarchy-notification-send".into());
    let args = ["--app-name", "Cloud Calendar", "-g", "󰃭", "-u", "normal", title, body, "--exec", "cloud-calendar-gtk"];
    match run_command(Command::new(&program).args(args), None, Duration::from_secs(15)) {
        Run::Done { status, .. } if status.success() => Ok(()),
        Run::Done { status, stderr, .. } => Err(format!("{program} failed ({status}): {stderr}")),
        Run::Missing => Err(format!("{program} isn't installed; notifications go through Omarchy's")),
        Run::TimedOut => Err(format!("{program} didn't answer")),
        Run::Failed(e) => Err(format!("could not run {program}: {e}")),
    }
}

/// One run of the notifier: refresh the cache when stale, send what's due, remember it.
pub fn run(calendars: &Calendars, leads: &[u32], now: DateTime<Utc>, force_refresh: bool) -> Report {
    let mut state = load_state();
    let mut report = Report::default();
    if force_refresh || state.fetched_at.is_none_or(|t| now - t >= REFRESH || t > now) {
        let listing = calendars.events(&Range { start: now - chrono::Duration::minutes(1), end: now + LOOKAHEAD });
        state.upcoming = upcoming(&listing.items);
        state.fetched_at = Some(now);
        report.refreshed = true;
        report.warnings = listing.warnings;
    }
    for (e, minutes) in due(&state.upcoming, leads, now, &mut state.sent) {
        match send(&e.title, &body(&e, now)) {
            Ok(()) => report.sent.push(Sent { id: e.id.clone(), title: e.title.clone(), start: e.start, minutes_before: minutes }),
            Err(message) => report.failed.push(message),
        }
    }
    state.upcoming.retain(|u| u.start > now - chrono::Duration::hours(1));
    state.sent.retain(|_, start| *start > now - chrono::Duration::hours(1));
    save_state(&state);
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn ev(id: &str, h: u32, m: u32) -> Upcoming {
        let start = Utc.with_ymd_and_hms(2026, 10, 9, h, m, 0).unwrap();
        Upcoming { id: id.into(), title: id.into(), start, end: start + chrono::Duration::hours(1), location: None, calendar: "Work".into() }
    }

    #[test]
    fn sends_once_at_the_nearest_lead() {
        let list = vec![ev("a", 14, 0), ev("b", 15, 0)];
        let mut sent = BTreeMap::new();
        let at = |h, m| Utc.with_ymd_and_hms(2026, 10, 9, h, m, 0).unwrap();
        assert!(due(&list, &[10], at(13, 49), &mut sent).is_empty());
        let d = due(&list, &[10], at(13, 50), &mut sent);
        assert_eq!(d.iter().map(|(e, m)| (e.id.as_str(), *m)).collect::<Vec<_>>(), vec![("a", 10)]);
        assert!(due(&list, &[10], at(13, 51), &mut sent).is_empty(), "never twice");
        // Woken late with two leads passed: one notification, at the nearer lead.
        let mut sent = BTreeMap::new();
        let d = due(&list, &[30, 5], at(14, 57), &mut sent);
        assert_eq!(d.iter().map(|(e, m)| (e.id.as_str(), *m)).collect::<Vec<_>>(), vec![("b", 5)]);
        // A started event is never notified.
        assert!(due(&list, &[10], at(14, 1), &mut BTreeMap::new()).iter().all(|(e, _)| e.id != "a"));
    }

    #[test]
    fn a_moved_event_notifies_again() {
        let mut sent = BTreeMap::new();
        let now = Utc.with_ymd_and_hms(2026, 10, 9, 13, 55, 0).unwrap();
        assert_eq!(due(&[ev("a", 14, 0)], &[10], now, &mut sent).len(), 1);
        assert_eq!(due(&[ev("a", 14, 1)], &[10], now, &mut sent).len(), 1);
    }
}
