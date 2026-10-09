//! HEY Calendar through the official `hey` CLI (basecamp/hey-cli 1.7): every call is
//! `hey … --json`, mapped into cloud-calendar's types. The CLI owns the login (`hey auth login`,
//! one browser OAuth); cloud-calendar never sees a token.
//!
//! Commands used: `hey calendar list`, `hey event week <date> --all` (HEY's own expansion of
//! repeating events, over the calendars switched on in HEY), `hey event add`, `hey event edit` and
//! `hey event delete`. Clock times are written in this machine's IANA time zone and named with
//! `--time-zone`: hey 1.7 otherwise sends an empty zone when `TZ` is unset (usual on Arch), which
//! HEY reads as UTC. A repeating event can't be changed here: `hey event edit <series>` finds a
//! series by the day it began (or within a year of today), which a week listing doesn't give.
//! JSON shapes: hey-sdk's `generated.Recording` and `generated.Calendar` (basecamp/hey-sdk Go
//! client), which `hey … --json` prints.
//!
//! hey keeps its sign-in in the keyring unless no keyring is reachable, when it writes
//! `credentials.json` instead (basecamp/hey-cli `internal/auth/store.go`, commit 73938bb); `hey
//! auth status` says which. Signing in here insists on the keyring, and `HEY_NO_KEYRING` is never
//! passed on.
//!
//! IDs: a calendar is `hey:<calendar id>`, an event `hey:<event id>@<UTC day it starts on>` (the
//! day `hey event edit` needs to find it), an occurrence of a repeating event
//! `hey:<series id>~<day>`, whose deletes go to the series.

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

/// Recording and calendar IDs are int64 in hey-sdk.
fn id_of(v: &Value) -> Option<String> {
    v.as_i64().map(|n| n.to_string())
}

/// An event's span from HEY's `starts_at` / `ends_at` / `all_day`. An all-day event's days are
/// its UTC dates (hey-cli `eventDay`), and its last day is the day `ends_at` falls on, unless
/// it ends exactly at midnight, when it is the day before; no end, or one not after the start,
/// is the start's day alone. This is how hey's own calendar spreads an event over days
/// (hey-cli `internal/tui/calendar_views.go` `eventsByDate`, commit 73938bb).
pub fn span(row: &Value) -> Option<(Time, Time)> {
    let start = instant(&row["starts_at"])?;
    let end = instant(&row["ends_at"]);
    if row["all_day"] == json!(true) {
        let first = start.date_naive();
        let last = match end {
            Some(e) if e > start && e.date_naive() != first => {
                let midnight = e.time() == chrono::NaiveTime::MIN;
                if midnight { e.date_naive() - chrono::Days::new(1) } else { e.date_naive() }
            }
            _ => first,
        };
        return Some((Time::Date(first), Time::Date(last.max(first) + chrono::Days::new(1))));
    }
    let end = end?;
    Some((Time::At(start), Time::At(end.max(start))))
}

/// The series an occurrence belongs to, from its `occurrence_id` (`<series>_<YYYY-MM-DD>`).
pub fn series_of(row: &Value) -> Option<String> {
    let occ = text(&row["occurrence_id"]);
    let (series, _) = occ.rsplit_once('_')?;
    (!series.is_empty()).then(|| series.to_string())
}

/// `hey event add`/`edit` flags for a span, a timed one in `zone` (an IANA name) and named so.
fn span_args(start: &Time, end: &Time, zone: &(String, chrono_tz::Tz)) -> Vec<String> {
    let day = |d: NaiveDate| d.format("%Y-%m-%d").to_string();
    match (start, end) {
        (Time::Date(s), Time::Date(e)) => {
            let last = (*e - chrono::Days::new(1)).max(*s);
            vec!["--all-day".into(), "--starts-on".into(), day(*s), "--ends-on".into(), day(last)]
        }
        _ => {
            let (s, e) = (start.instant().with_timezone(&zone.1), end.instant().with_timezone(&zone.1));
            vec![
                "--time-zone".into(),
                zone.0.clone(),
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
        let command = std::env::var(COMMAND_ENV).ok().filter(|c| !c.trim().is_empty()).unwrap_or_else(|| "hey".into());
        Self { name: name.into(), command, account: cfg.account.clone().filter(|a| !a.trim().is_empty()) }
    }

    fn fail(&self, kind: ErrorKind, message: impl AsRef<str>) -> Error {
        Error::new(kind, format!("HEY: {}", message.as_ref()))
    }

    /// Runs `hey <args> --json` and returns the envelope's `data`.
    fn run(&self, args: &[String]) -> Result<Value> {
        let mut cmd = Command::new(&self.command);
        cmd.args(args).arg("--json").env("HEY_NONINTERACTIVE", "1").env_remove("HEY_NO_KEYRING");
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
        // With --json, hey writes its error envelope, indented, to stderr (hey-cli
        // internal/output/writer.go `Err`): `error`, and a `hint` saying what to do.
        let failure: Value = if envelope["ok"] == json!(false) { envelope } else { serde_json::from_str(stderr.trim()).unwrap_or(Value::Null) };
        let mut message = text(&failure["error"]);
        if let Some(hint) = nonempty(text(&failure["hint"])).filter(|_| !message.is_empty()) {
            message = format!("{message} ({hint})");
        }
        if message.is_empty() {
            message = stderr.lines().map(str::trim).rfind(|l| !l.is_empty()).unwrap_or("").to_string();
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

    /// `hey auth login`: hey opens the browser itself (hey-cli `internal/auth/browser.go`) and
    /// waits for HEY to send it back.
    pub fn login(&self) -> Result<()> {
        let mut cmd = Command::new(&self.command);
        cmd.args(["auth", "login"]).env_remove("HEY_NO_KEYRING");
        if let Some(a) = &self.account {
            cmd.args(["--account", a]);
        }
        match run_command(&mut cmd, None, crate::accounts::SIGN_IN_TIMEOUT) {
            Run::Done { status, .. } if status.success() => Ok(()),
            Run::Done { status, stderr, .. } => Err(self.fail(ErrorKind::AccountAuth, format!("`hey auth login` didn't finish ({status}): {}", stderr.lines().last().unwrap_or("")))),
            Run::TimedOut => Err(self.fail(ErrorKind::AccountAuth, "the HEY sign-in wasn't finished within 10 minutes")),
            Run::Missing => Err(self.fail(ErrorKind::AccountUnavailable, format!("the hey CLI isn't installed (no `{}` on PATH); see https://github.com/basecamp/hey-cli", self.command))),
            Run::Failed(e) => Err(self.fail(ErrorKind::AccountUnavailable, e.to_string())),
        }
    }

    pub fn signed_in(&self) -> Result<bool> {
        Ok(self.run(&Self::args(&["auth", "status"]))?["authenticated"] == json!(true))
    }

    /// Fails unless hey keeps its sign-in in the keyring (`hey auth status` → `storage`).
    pub fn check_keyring(&self) -> Result<()> {
        let status = self.run(&Self::args(&["auth", "status"]))?;
        if status["authenticated"] != json!(true) {
            return Err(self.fail(ErrorKind::AccountAuth, format!("not signed in: sign in again (`cloud-calendar account login {}`, or in the app)", self.name)));
        }
        match status["storage"].as_str() {
            Some("keyring") => Ok(()),
            _ => Err(self.fail(
                ErrorKind::AccountAuth,
                "hey keeps its sign-in in a plain file (credentials.json) because no keyring was reachable when it signed in; start a keyring (e.g. gnome-keyring), run `hey auth logout`, and sign in again",
            )),
        }
    }

    fn row_event(&self, row: &Value) -> Option<Event> {
        let (start, end) = span(row)?;
        let series = series_of(row).filter(|_| !text(&row["occurrence_id"]).is_empty());
        let recurring = series.is_some() || row["recurring"] == json!(true);
        let local = match &series {
            Some(s) => format!("{s}{}", provider::occurrence_suffix(&start)),
            None if recurring => format!("{}{}", id_of(&row["id"])?, provider::occurrence_suffix(&start)),
            // The UTC day it starts on rides along: `hey event edit <id> <day>` finds an event on
            // that day, where without one it looks only a year either side of today.
            None => format!("{}@{}", id_of(&row["id"])?, instant(&row["starts_at"])?.date_naive().format("%Y-%m-%d")),
        };
        let cal = &row["calendar"];
        let title = nonempty(text(&row["title"])).unwrap_or_else(|| "(no title)".into());
        Some(Event {
            id: format!("{}:{local}", self.name),
            account: self.name.clone(),
            calendar_id: id_of(&cal["id"]).map(|c| format!("{}:{c}", self.name)).unwrap_or_default(),
            calendar: text(&cal["name"]),
            color: nonempty(text(&cal["color"])),
            title,
            all_day: start.is_date(),
            start,
            end,
            location: nonempty(text(&row["location"])),
            // An event's notes are hey-sdk's `description` (`notes` is a time track's).
            notes: nonempty(text(&row["description"])),
            recurring,
        })
    }

    /// What an ID reaches: the HEY event id (an occurrence's series), whether it was an
    /// occurrence, and the UTC day the event starts on when the ID carries it (`<id>@YYYY-MM-DD`).
    fn target(&self, id: &str) -> Result<(String, bool, Option<String>)> {
        let local = provider::local_id(&self.name, "HEY", id)?;
        let (base, occurrence) = provider::split_occurrence(local);
        let (base, day) = match base.split_once('@') {
            Some((b, d)) if NaiveDate::parse_from_str(d, "%Y-%m-%d").is_ok() => (b, Some(d.to_string())),
            Some(_) => return Err(self.fail(ErrorKind::NotFound, format!("{id} isn't a HEY event ID"))),
            None => (base, None),
        };
        if base.is_empty() || !base.chars().all(|c| c.is_ascii_digit()) {
            return Err(self.fail(ErrorKind::NotFound, format!("{id} isn't a HEY event ID")));
        }
        Ok((base.to_string(), occurrence.is_some(), day))
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
            Ok(true) => match self.check_keyring() {
                Ok(()) => {
                    s.ok = true;
                    s.detail = format!("signed in via {}", self.command);
                }
                Err(e) => s.detail = e.message,
            },
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
                    // HEY refuses events filed to the personal calendar, and to subscriptions.
                    writable: c["owned"] == json!(true) && c["personal"] != json!(true) && c["external"] != json!(true),
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
        // `--title=` keeps a title that starts with "-" from being read as a flag.
        let mut args = Self::args(&["event", "add", &format!("--title={}", event.title), "--calendar", cal]);
        args.extend(span_args(&event.start, &event.end, &crate::icloud::zone()?).into_iter().filter(|a| a != "--all-day=false"));
        if let Some(l) = event.location.as_deref().filter(|l| !l.trim().is_empty()) {
            args.extend(["--location".into(), l.to_string()]);
        }
        if let Some(n) = event.notes.as_deref().filter(|n| !n.trim().is_empty()) {
            args.extend(["--notes".into(), n.to_string()]);
        }
        let data = self.run(&args)?;
        let id = id_of(&data["id"]).ok_or_else(|| self.fail(ErrorKind::AccountUnavailable, "`hey event add` didn't say which event it made"))?;
        let day = match event.start {
            Time::At(t) => t.date_naive(),
            Time::Date(d) => d,
        };
        Ok(format!("{}:{id}@{}", self.name, day.format("%Y-%m-%d")))
    }

    fn update(&self, id: &str, change: &EventChange) -> Result<()> {
        let (target, occurrence, day) = self.target(id)?;
        if occurrence {
            return Err(Error::bad_request(
                "HEY: a repeating HEY event can't be changed here yet (the hey CLI finds a series by the day it began, which the week listing doesn't give); change it in HEY",
            ));
        }
        let mut args = Self::args(&["event", "edit", &target]);
        args.extend(day);
        let base_len = args.len();
        if let Some(t) = &change.title {
            args.push(format!("--title={t}"));
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
            args.extend(span_args(&s, &e, &crate::icloud::zone()?));
        }
        if args.len() == base_len {
            return Ok(());
        }
        self.run(&args).map(|_| ())
    }

    fn delete(&self, id: &str) -> Result<()> {
        let (target, _, _) = self.target(id)?;
        self.run(&Self::args(&["event", "delete", &target])).map(|_| ())
    }

    /// hey 1.7 resends the whole event on any edit (HEY clears what a write leaves out): HEY
    /// serves notes back as plain text, no countdown at all, and no attached email you can't read
    /// (`hey event edit --help`).
    fn edit_caveat(&self) -> Option<&'static str> {
        Some("editing a HEY event removes its countdown, flattens its notes' formatting and detaches an attached email you can't read (HEY's CLI resends the whole event)")
    }

    fn series_support(&self) -> provider::SeriesSupport {
        provider::SeriesSupport { edit: false, delete: true }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn all_day_days_follow_heys_own_calendar() {
        let d = |day| Time::Date(NaiveDate::from_ymd_opt(2026, 10, day).unwrap());
        let row = |s: &str, e: &str| json!({"all_day": true, "starts_at": s, "ends_at": e});
        // Ending at midnight: the day before is the last. Ending inside a day: that day is.
        assert_eq!(span(&row("2026-10-15T00:00:00Z", "2026-10-18T00:00:00Z")).unwrap(), (d(15), d(18)));
        assert_eq!(span(&row("2026-10-15T00:00:00Z", "2026-10-17T23:59:59Z")).unwrap(), (d(15), d(18)));
        // No real end: the start's day alone.
        assert_eq!(span(&row("2026-10-15T00:00:00Z", "2026-10-15T00:00:00Z")).unwrap(), (d(15), d(16)));
        let timed = json!({"starts_at": "2026-10-09T14:00:00+02:00", "ends_at": "2026-10-09T15:00:00+02:00"});
        assert_eq!(span(&timed).unwrap().0, Time::At(Utc.with_ymd_and_hms(2026, 10, 9, 12, 0, 0).unwrap()));
    }
}
