//! HEY Calendar through the official `hey` CLI (basecamp/hey-cli 1.7): every call is
//! `hey … --json`, mapped into cloud-calendar's types. The CLI owns the login (`hey auth login`,
//! one browser OAuth); cloud-calendar never sees a token.
//!
//! Commands used: `hey calendar list`, `hey event week <date> --all` (HEY's own expansion of
//! repeating events, over the calendars switched on in HEY), `hey event add`, `hey event edit` and
//! `hey event delete`. Times are written in this machine's time zone, as the CLI does by default.
//!
//! IDs: a calendar is `hey:<calendar id>`, an event `hey:<event id>`, an occurrence of a repeating
//! event `hey:<series id>~<day>`, whose edits and deletes go to the series.

use chrono::{DateTime, Local, NaiveDate, Utc};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::process::Command;
use std::time::Duration;

use crate::config::AccountConfig;
use crate::error::{Error, ErrorKind, Result};
use crate::provider::{self, AccountStatus, Provider, Run, run_command};
use crate::types::*;

pub const COMMAND_ENV: &str = "CLOUD_CALENDAR_HEY_COMMAND";
const TIMEOUT: Duration = Duration::from_secs(90);

pub struct Hey {
    name: String,
    command: String,
    account: Option<String>,
}

fn text(v: &Value) -> String {
    v.as_str().unwrap_or_default().trim().to_string()
}

fn nonempty(s: String) -> Option<String> {
    Some(s).filter(|s| !s.is_empty())
}

fn instant(v: &Value) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(v.as_str()?).ok().map(|t| t.with_timezone(&Utc))
}

fn id_of(v: &Value) -> Option<String> {
    match v {
        Value::Number(n) => Some(n.to_string()),
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        _ => None,
    }
}

/// An event's span from HEY's `starts_at` / `ends_at` / `all_day`. HEY dates an all-day event by
/// its UTC day; its last day is inclusive unless `ends_at` falls a whole number of days after
/// `starts_at`, which reads as an exclusive end.
pub fn span(row: &Value) -> Option<(Time, Time)> {
    let start = instant(&row["starts_at"])?;
    let end = instant(&row["ends_at"]).unwrap_or(start);
    if row["all_day"] == json!(true) {
        let first = start.date_naive();
        let whole_days = end > start && (end - start).num_seconds() % 86_400 == 0;
        let last_excl = if whole_days { first + chrono::Days::new(((end - start).num_seconds() / 86_400) as u64) } else { end.date_naive().max(first) + chrono::Days::new(1) };
        return Some((Time::Date(first), Time::Date(last_excl)));
    }
    Some((Time::At(start), Time::At(end.max(start))))
}

/// The series an occurrence belongs to, from its `occurrence_id` (`<series>_<YYYY-MM-DD>`).
pub fn series_of(row: &Value) -> Option<String> {
    let occ = text(&row["occurrence_id"]);
    let (series, _) = occ.rsplit_once('_')?;
    (!series.is_empty()).then(|| series.to_string())
}

/// `hey event add`/`edit` flags for a span, in local time.
fn span_args(start: &Time, end: &Time) -> Vec<String> {
    let day = |d: NaiveDate| d.format("%Y-%m-%d").to_string();
    match (start, end) {
        (Time::Date(s), Time::Date(e)) => {
            let last = (*e - chrono::Days::new(1)).max(*s);
            vec!["--all-day".into(), "--starts-on".into(), day(*s), "--ends-on".into(), day(last)]
        }
        _ => {
            let (s, e) = (start.instant().with_timezone(&Local), end.instant().with_timezone(&Local));
            vec![
                "--all-day=false".into(),
                "--starts-on".into(),
                day(s.date_naive()),
                "--start-time".into(),
                s.format("%H:%M").to_string(),
                "--ends-on".into(),
                day(e.date_naive()),
                "--end-time".into(),
                e.format("%H:%M").to_string(),
            ]
        }
    }
}

impl Hey {
    pub fn new(name: &str, cfg: &AccountConfig) -> Self {
        let command = std::env::var(COMMAND_ENV)
            .ok()
            .filter(|c| !c.trim().is_empty())
            .or_else(|| cfg.command.clone().filter(|c| !c.trim().is_empty()))
            .unwrap_or_else(|| "hey".into());
        Self { name: name.into(), command, account: cfg.account.clone().filter(|a| !a.trim().is_empty()) }
    }

    fn fail(&self, kind: ErrorKind, message: impl AsRef<str>) -> Error {
        Error::new(kind, format!("HEY: {}", message.as_ref()))
    }

    /// Runs `hey <args> --json` and returns the envelope's `data`.
    fn run(&self, args: &[String]) -> Result<Value> {
        let mut cmd = Command::new(&self.command);
        cmd.args(args).arg("--json").env("HEY_NONINTERACTIVE", "1");
        if let Some(a) = &self.account {
            cmd.args(["--account", a]);
        }
        let what = args.iter().take(2).cloned().collect::<Vec<_>>().join(" ");
        let (status, stdout, stderr) = match run_command(&mut cmd, None, TIMEOUT) {
            Run::Done { status, stdout, stderr } => (status, stdout, stderr),
            Run::Missing => return Err(self.fail(ErrorKind::AccountUnavailable, format!("the hey CLI isn't installed (no `{}` on PATH); see https://github.com/basecamp/hey-cli", self.command))),
            Run::TimedOut => return Err(self.fail(ErrorKind::AccountUnavailable, format!("`hey {what}` took longer than {}s", TIMEOUT.as_secs()))),
            Run::Failed(e) => return Err(self.fail(ErrorKind::AccountUnavailable, format!("could not run {}: {e}", self.command))),
        };
        let envelope: Value = serde_json::from_slice(&stdout).unwrap_or(Value::Null);
        if status.success() && envelope["ok"] == json!(true) {
            return Ok(envelope["data"].clone());
        }
        if status.success() {
            return Err(self.fail(ErrorKind::AccountUnavailable, format!("`hey {what}` answered something unexpected (a newer hey CLI?)")));
        }
        let mut message = text(&envelope["error"]);
        if message.is_empty() {
            message = stderr.lines().last().unwrap_or("").to_string();
        }
        if message.is_empty() {
            message = format!("`hey {what}` failed ({status})");
        }
        // hey's exit statuses: 1 usage/conflict, 2 not found, 3 auth, 4 forbidden, 5 rate
        // limited, 6 network, 7 API/server, 8 ambiguous.
        Err(match status.code() {
            Some(3) => self.fail(ErrorKind::AccountAuth, format!("{message}; run `cloud-calendar account login {}`", self.name)),
            Some(2) => self.fail(ErrorKind::NotFound, message),
            Some(1 | 4 | 8) => self.fail(ErrorKind::BadRequest, message),
            _ => self.fail(ErrorKind::AccountUnavailable, message),
        })
    }

    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    pub fn login(&self) -> Result<()> {
        match provider::run_attached(Command::new(&self.command).args(["auth", "login"])) {
            Ok(Some(s)) if s.success() => Ok(()),
            Ok(Some(s)) => Err(self.fail(ErrorKind::AccountAuth, format!("`hey auth login` didn't finish ({s})"))),
            Ok(None) => Err(self.fail(ErrorKind::AccountAuth, "the HEY sign-in wasn't finished within 10 minutes")),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(self.fail(ErrorKind::AccountUnavailable, format!("the hey CLI isn't installed (no `{}` on PATH)", self.command))),
            Err(e) => Err(self.fail(ErrorKind::AccountUnavailable, e.to_string())),
        }
    }

    pub fn signed_in(&self) -> Result<bool> {
        Ok(self.run(&Self::args(&["auth", "status"]))?["authenticated"] == json!(true))
    }

    fn row_event(&self, row: &Value) -> Option<Event> {
        let (start, end) = span(row)?;
        let series = series_of(row).filter(|_| !text(&row["occurrence_id"]).is_empty());
        let recurring = series.is_some() || row["recurring"] == json!(true);
        let local = match &series {
            Some(s) => format!("{s}{}", provider::occurrence_suffix(&start)),
            None if recurring => format!("{}{}", id_of(&row["id"])?, provider::occurrence_suffix(&start)),
            None => id_of(&row["id"])?,
        };
        let cal = &row["calendar"];
        let title = nonempty(text(&row["title"])).or_else(|| nonempty(text(&row["summary"]))).unwrap_or_else(|| "(no title)".into());
        Some(Event {
            id: format!("{}:{local}", self.name),
            account: self.name.clone(),
            calendar_id: id_of(&cal["id"]).map(|c| format!("{}:{c}", self.name)).unwrap_or_default(),
            calendar: text(&cal["name"]),
            color: nonempty(text(&cal["color"])).or_else(|| nonempty(text(&row["color"]))),
            title,
            all_day: start.is_date(),
            start,
            end,
            location: nonempty(text(&row["location"])),
            notes: nonempty(text(&row["notes"])),
            recurring,
        })
    }

    /// The HEY event id an action reaches: an occurrence's series, else the event.
    fn target(&self, id: &str) -> Result<(String, bool)> {
        let local = provider::local_id(&self.name, "HEY", id)?;
        let (base, occurrence) = provider::split_occurrence(local);
        if base.is_empty() || !base.chars().all(|c| c.is_ascii_digit()) {
            return Err(self.fail(ErrorKind::NotFound, format!("{id} isn't a HEY event ID")));
        }
        Ok((base.to_string(), occurrence.is_some()))
    }
}

impl Provider for Hey {
    fn name(&self) -> &str {
        &self.name
    }

    fn label(&self) -> &str {
        "HEY"
    }

    fn status(&self) -> AccountStatus {
        let mut s = AccountStatus { name: self.name.clone(), provider: "hey".into(), label: "HEY".into(), ..Default::default() };
        match self.signed_in() {
            Ok(true) => {
                s.ok = true;
                s.detail = format!("signed in via {}", self.command);
            }
            Ok(false) => s.detail = format!("not signed in: run `cloud-calendar account login {}`", self.name),
            Err(e) => s.detail = e.message,
        }
        s
    }

    fn sign_in(&self) -> Result<()> {
        self.login()
    }

    fn calendars(&self) -> Result<Vec<Calendar>> {
        let data = self.run(&Self::args(&["calendar", "list"]))?;
        let list = data.as_array().ok_or_else(|| self.fail(ErrorKind::AccountUnavailable, "unexpected `hey calendar list` output (a newer hey CLI?)"))?;
        Ok(list
            .iter()
            .filter_map(|c| {
                let id = id_of(&c["id"])?;
                Some(Calendar {
                    id: format!("{}:{id}", self.name),
                    account: self.name.clone(),
                    name: nonempty(text(&c["name"])).unwrap_or(id),
                    color: nonempty(text(&c["color"])),
                    writable: c["external"] != json!(true),
                })
            })
            .collect())
    }

    fn events(&self, range: &Range) -> Result<Vec<Event>> {
        // HEY reads by week; one date in each week the range touches.
        let first = range.start.with_timezone(&Local).date_naive();
        let last = (range.end - chrono::Duration::seconds(1)).with_timezone(&Local).date_naive().max(first);
        let mut days = Vec::new();
        let mut d = first;
        while d <= last {
            days.push(d);
            d = d + chrono::Days::new(7);
        }
        if days.last() != Some(&last) {
            days.push(last);
        }
        let results: Vec<Result<Value>> = std::thread::scope(|s| {
            let handles: Vec<_> = days.iter().map(|d| s.spawn(move || self.run(&Self::args(&["event", "week", &d.format("%Y-%m-%d").to_string(), "--all"])))).collect();
            handles.into_iter().map(|h| h.join().unwrap_or_else(|_| Err(self.fail(ErrorKind::AccountUnavailable, "failed unexpectedly")))).collect()
        });
        let (mut out, mut seen) = (Vec::new(), HashSet::new());
        for r in results {
            let data = r?;
            let rows = data.as_array().ok_or_else(|| self.fail(ErrorKind::AccountUnavailable, "unexpected `hey event week` output (a newer hey CLI?)"))?;
            for row in rows {
                if let Some(e) = self.row_event(row)
                    && range.overlaps(&e.start, &e.end)
                    && seen.insert(e.id.clone())
                {
                    out.push(e);
                }
            }
        }
        Ok(out)
    }

    fn create(&self, event: &NewEvent) -> Result<String> {
        check_span(&event.start, &event.end)?;
        let cal = provider::local_id(&self.name, "HEY", &event.calendar_id)?;
        let mut args = Self::args(&["event", "add", &event.title, "--calendar", cal]);
        args.extend(span_args(&event.start, &event.end).into_iter().filter(|a| a != "--all-day=false"));
        if let Some(l) = event.location.as_deref().filter(|l| !l.trim().is_empty()) {
            args.extend(["--location".into(), l.to_string()]);
        }
        if let Some(n) = event.notes.as_deref().filter(|n| !n.trim().is_empty()) {
            args.extend(["--notes".into(), n.to_string()]);
        }
        let data = self.run(&args)?;
        let id = id_of(&data["id"]).ok_or_else(|| self.fail(ErrorKind::AccountUnavailable, "`hey event add` didn't say which event it made"))?;
        Ok(format!("{}:{id}", self.name))
    }

    fn update(&self, id: &str, change: &EventChange) -> Result<()> {
        let (target, occurrence) = self.target(id)?;
        if occurrence {
            provider::refuse_series_move("HEY", change)?;
        }
        let mut args = Self::args(&["event", "edit", &target]);
        if let Some(t) = &change.title {
            args.extend(["--title".into(), t.clone()]);
        }
        if let Some(l) = &change.location {
            args.extend(["--location".into(), l.trim().to_string()]);
        }
        if let Some(n) = &change.notes {
            args.extend(["--notes".into(), n.trim().to_string()]);
        }
        if change.moves() {
            // HEY has no read of one event to keep its length from, so a move names both ends.
            let (Some(s), Some(e)) = (change.start, change.end) else {
                return Err(Error::bad_request("HEY: give both the new start and the new end to move a HEY event"));
            };
            check_span(&s, &e)?;
            args.extend(span_args(&s, &e));
        }
        if args.len() == 3 {
            return Ok(());
        }
        self.run(&args).map(|_| ())
    }

    fn delete(&self, id: &str) -> Result<()> {
        let (target, _) = self.target(id)?;
        self.run(&Self::args(&["event", "delete", &target])).map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn spans() {
        let timed = json!({"starts_at": "2026-10-09T14:00:00Z", "ends_at": "2026-10-09T15:00:00Z"});
        let (s, e) = span(&timed).unwrap();
        assert_eq!(s, Time::At(Utc.with_ymd_and_hms(2026, 10, 9, 14, 0, 0).unwrap()));
        assert_eq!(e, Time::At(Utc.with_ymd_and_hms(2026, 10, 9, 15, 0, 0).unwrap()));
        let d = |day| Time::Date(NaiveDate::from_ymd_opt(2026, 10, day).unwrap());
        // Inclusive end of day, and an exclusive whole-day end, both read as the 9th alone.
        let inclusive = json!({"all_day": true, "starts_at": "2026-10-09T00:00:00Z", "ends_at": "2026-10-09T23:59:59Z"});
        assert_eq!(span(&inclusive).unwrap(), (d(9), d(10)));
        let exclusive = json!({"all_day": true, "starts_at": "2026-10-09T00:00:00Z", "ends_at": "2026-10-10T00:00:00Z"});
        assert_eq!(span(&exclusive).unwrap(), (d(9), d(10)));
        let same = json!({"all_day": true, "starts_at": "2026-10-09T00:00:00Z", "ends_at": "2026-10-09T00:00:00Z"});
        assert_eq!(span(&same).unwrap(), (d(9), d(10)));
        let two = json!({"all_day": true, "starts_at": "2026-10-09T00:00:00Z", "ends_at": "2026-10-10T00:00:00Z", "x": 1});
        assert_eq!(span(&two).unwrap().1, d(10));
    }

    #[test]
    fn series() {
        assert_eq!(series_of(&json!({"occurrence_id": "4821_2026-09-15"})).as_deref(), Some("4821"));
        assert_eq!(series_of(&json!({"id": 1})), None);
    }

    #[test]
    fn all_day_args_use_an_inclusive_last_day() {
        let d = |day| Time::Date(NaiveDate::from_ymd_opt(2026, 10, day).unwrap());
        assert_eq!(span_args(&d(9), &d(11)), vec!["--all-day", "--starts-on", "2026-10-09", "--ends-on", "2026-10-10"]);
    }
}
