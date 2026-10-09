//! Human-readable output.

use chrono::{Datelike, Local};
use cloud_calendar_api::{AccountStatus, AccountWarning, Calendar, Event, Range, Time};

pub fn calendars(list: &[Calendar]) -> String {
    if list.is_empty() {
        return "No calendars.".into();
    }
    let mut out = String::new();
    let mut account = "";
    for c in list {
        if c.account != account {
            account = &c.account;
            out.push_str(&format!("{account}\n"));
        }
        out.push_str(&format!("  {}{}  {}\n", c.name, if c.writable { "" } else { " (read-only)" }, c.id));
    }
    out
}

pub fn accounts(list: &[AccountStatus], broken: &[AccountWarning]) -> String {
    if list.is_empty() && broken.is_empty() {
        return "No accounts linked. Add one with `cloud-calendar account add icloud`, `… add google` or `… add hey`, or under Accounts in the app.".into();
    }
    let mut out = String::new();
    for s in list {
        out.push_str(&format!("{} {:<10} {:<7} {}\n", if s.ok { "✓" } else { "✗" }, s.name, s.label, s.detail));
    }
    for b in broken {
        out.push_str(&format!("✗ {:<10} {}\n", b.account, b.message));
    }
    out
}

fn clock(t: &Time) -> String {
    match t {
        Time::At(t) => t.with_timezone(&Local).format("%H:%M").to_string(),
        Time::Date(_) => String::new(),
    }
}

/// Events grouped by local day; an event spanning days shows on each.
pub fn agenda(events: &[Event], range: &Range) -> String {
    let first = range.start.with_timezone(&Local).date_naive();
    let last = (range.end - chrono::Duration::seconds(1)).with_timezone(&Local).date_naive();
    let mut out = String::new();
    let mut day = first;
    while day <= last {
        let day_range = Range::days(day, 1);
        let todays: Vec<&Event> = events.iter().filter(|e| day_range.overlaps(&e.start, &e.end)).collect();
        if !todays.is_empty() {
            out.push_str(&format!("{} {} {}\n", day.weekday(), day.day(), day.format("%b")));
            for e in todays {
                let when = if e.all_day { "all day".to_string() } else { format!("{}–{}", clock(&e.start), clock(&e.end)) };
                let mut line = format!("  {when:<11} {}", e.title);
                if e.recurring {
                    line.push_str(" ↻");
                }
                line.push_str(&format!("  · {} ({})", e.calendar, e.account));
                if let Some(l) = &e.location {
                    line.push_str(&format!(" · {l}"));
                }
                out.push_str(&format!("{line}\n    {}\n", e.id));
            }
        }
        let Some(next) = day.succ_opt() else { break };
        day = next;
    }
    if out.is_empty() { "No events.".into() } else { out }
}
