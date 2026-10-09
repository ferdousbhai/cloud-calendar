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

/// One notification due: the event, the lead it fires at, and the keys of every lead that has
/// passed, to mark sent once it was shown.
#[derive(Debug, Clone)]
pub struct Due {
    pub event: Upcoming,
    pub minutes: u32,
    pub keys: Vec<String>,
}

/// What is due at `now`: per event, the nearest lead time that has passed while the event hasn't
/// started, unless already sent. Marking every passed lead sent (after a successful send) keeps a
/// farther lead from firing later.
pub fn due(upcoming: &[Upcoming], leads: &[u32], now: DateTime<Utc>, sent: &BTreeMap<String, DateTime<Utc>>) -> Vec<Due> {
    let mut out = Vec::new();
    for e in upcoming {
        if e.start <= now {
            continue;
        }
        let mut passed: Vec<u32> = leads.iter().copied().filter(|m| e.start - chrono::Duration::minutes(i64::from(*m)) <= now).collect();
        passed.sort_unstable();
        let keys: Vec<String> = passed.iter().map(|m| format!("{}|{}|{m}", e.id, e.start.timestamp())).collect();
        if keys.iter().any(|k| !sent.contains_key(k)) && let Some(m) = passed.first() {
            out.push(Due { event: e.clone(), minutes: *m, keys });
        }
    }
    out
}

/// The refreshed cache: `fresh` for every account that answered, and the previous entries of the
/// accounts in `failed` (they answered nothing this time, so what was cached still stands).
pub fn merge_refresh(previous: Vec<Upcoming>, fresh: Vec<Upcoming>, failed: &[String]) -> Vec<Upcoming> {
    let owned_by_failed = |u: &Upcoming| failed.iter().any(|a| u.id.starts_with(&format!("{a}:")));
    let mut out: Vec<Upcoming> = previous.into_iter().filter(owned_by_failed).collect();
    out.extend(fresh.into_iter().filter(|u| !owned_by_failed(u)));
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
        let failed: Vec<String> = listing.warnings.iter().map(|w| w.account.clone()).collect();
        state.upcoming = merge_refresh(std::mem::take(&mut state.upcoming), upcoming(&listing.items), &failed);
        // A refresh some account didn't answer isn't fresh: the next run asks again.
        state.fetched_at = failed.is_empty().then_some(now);
        report.refreshed = true;
        report.warnings = listing.warnings;
    }
    for d in due(&state.upcoming, leads, now, &state.sent) {
        let e = &d.event;
        match send(&e.title, &body(e, now)) {
            Ok(()) => {
                for k in d.keys {
                    state.sent.insert(k, e.start);
                }
                report.sent.push(Sent { id: e.id.clone(), title: e.title.clone(), start: e.start, minutes_before: d.minutes });
            }
            // Not marked sent, so the next run tries again while the event hasn't started.
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

    fn ids(d: &[Due]) -> Vec<(&str, u32)> {
        d.iter().map(|d| (d.event.id.as_str(), d.minutes)).collect()
    }

    fn mark(d: &[Due], sent: &mut BTreeMap<String, DateTime<Utc>>) {
        for d in d {
            for k in &d.keys {
                sent.insert(k.clone(), d.event.start);
            }
        }
    }

    #[test]
    fn sends_once_at_the_nearest_lead() {
        let list = vec![ev("a", 14, 0), ev("b", 15, 0)];
        let mut sent = BTreeMap::new();
        let at = |h, m| Utc.with_ymd_and_hms(2026, 10, 9, h, m, 0).unwrap();
        assert!(due(&list, &[10], at(13, 49), &sent).is_empty());
        let d = due(&list, &[10], at(13, 50), &sent);
        assert_eq!(ids(&d), vec![("a", 10)]);
        mark(&d, &mut sent);
        assert!(due(&list, &[10], at(13, 51), &sent).is_empty(), "never twice");
        // Woken late with two leads passed: one notification, at the nearer lead.
        let d = due(&list, &[30, 5], at(14, 57), &BTreeMap::new());
        assert_eq!(ids(&d), vec![("b", 5)]);
        // A started event is never notified.
        assert!(due(&list, &[10], at(14, 1), &BTreeMap::new()).iter().all(|d| d.event.id != "a"));
    }

    #[test]
    fn a_moved_event_notifies_again() {
        let mut sent = BTreeMap::new();
        let now = Utc.with_ymd_and_hms(2026, 10, 9, 13, 55, 0).unwrap();
        let d = due(&[ev("a", 14, 0)], &[10], now, &sent);
        assert_eq!(d.len(), 1);
        mark(&d, &mut sent);
        assert_eq!(due(&[ev("a", 14, 1)], &[10], now, &sent).len(), 1);
    }

    #[test]
    fn a_failing_account_keeps_its_events_and_a_failed_send_is_retried() {
        let cached = vec![ev("icloud:a", 14, 0), ev("hey:b", 14, 0)];
        // iCloud didn't answer this refresh; HEY's event moved.
        let refreshed = merge_refresh(cached, vec![ev("hey:b", 14, 5)], &["icloud".to_string()]);
        let mut got: Vec<_> = refreshed.iter().map(|u| (u.id.as_str(), u.start)).collect();
        got.sort();
        assert_eq!(got, vec![("hey:b", ev("hey:b", 14, 5).start), ("icloud:a", ev("icloud:a", 14, 0).start)]);
        // The send failed, so nothing was marked: the next minute it's due again.
        let sent = BTreeMap::new();
        let at = |m| Utc.with_ymd_and_hms(2026, 10, 9, 13, m, 0).unwrap();
        assert_eq!(ids(&due(&refreshed, &[10], at(50), &sent)), vec![("icloud:a", 10)]);
        assert_eq!(ids(&due(&refreshed, &[10], at(51), &sent)), vec![("icloud:a", 10)]);
    }
}
