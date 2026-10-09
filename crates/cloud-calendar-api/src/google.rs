//! Google Calendar through Google's Workspace CLI `gws` (github.com/googleworkspace/cli): every
//! call is a raw Calendar API request, `gws calendar <resource> <method> --params '<json>'
//! [--json '<body>']`, whose JSON answer is mapped into cloud-calendar's types. gws owns the
//! tokens; cloud-calendar never sees one.
//!
//! Isolation, as cloud-mail does for Gmail: gws runs with its config directory set to
//! cloud-calendar's own (`~/.config/cloud-calendar/gws/<account>`) and no way to fall back to
//! Application Default Credentials, so a gws setup of your own is never read or changed.
//!
//! Where gws keeps the sign-in: `credentials.enc` in that directory, encrypted with a key gws
//! keeps in `.encryption_key` beside it. gws can't keep that key only in the keyring on Linux:
//! its "keyring" backend still writes and reads the file there, because its keyring crate is built
//! without a Secret Service backend on Linux (googleworkspace/cli
//! `crates/google-workspace-cli/src/credential_store.rs`, `resolve_key`, commit a3768d0). So the
//! "file" backend is used, which is what gws does on Linux anyway, without touching the one
//! keyring entry a gws of your own uses.
//!
//! Sign-in: `gws auth login --scopes <calendar.events, calendar.calendarlist.readonly>` with
//! Cloud Calendar's built-in Google OAuth client (see `GOOGLE_CLIENT_ID`), as cloud-mail signs in
//! to Gmail: one browser sign-in, nothing to set up in Google Cloud.
//!
//! IDs: a calendar is `google:<calendarId>`, an event `google:<calendarId>/<eventId>`; listings
//! expand repeating events (`singleEvents`), and an occurrence's edits and deletes go to its series
//! (`recurringEventId`). Only calendars shown in Google Calendar (`selected`) are read.

use chrono::{DateTime, NaiveDate, Utc};
use serde_json::{Map, Value, json};
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::config::AccountConfig;
use crate::error::{Error, ErrorKind, Result};
use crate::provider::{self, AccountStatus, Provider, Run, run_command};
use crate::types::*;

pub const COMMAND_ENV: &str = "CLOUD_CALENDAR_GWS_COMMAND";
/// Cloud Calendar's own Google OAuth client, a "Desktop app" client in cloud-mail's Google Cloud
/// project with the Google Calendar API enabled. Google treats a desktop client's secret as
/// public, so it ships in the binary, as cloud-mail's does. Empty until the maintainer fills them
/// in; until then adding Google says sign-in isn't configured. `CLOUD_CALENDAR_GOOGLE_CLIENT_ID` /
/// `_SECRET` let a build use its own client instead.
pub const GOOGLE_CLIENT_ID: &str = "";
pub const GOOGLE_CLIENT_SECRET: &str = "";
pub const CLIENT_ID_ENV: &str = "CLOUD_CALENDAR_GOOGLE_CLIENT_ID";
pub const CLIENT_SECRET_ENV: &str = "CLOUD_CALENDAR_GOOGLE_CLIENT_SECRET";
/// The program that opens the sign-in link (default: xdg-open).
pub const BROWSER_ENV: &str = "CLOUD_CALENDAR_BROWSER";
/// Events on your calendars, and the list of calendars; nothing else in your Google account.
pub const SCOPES: &str = "https://www.googleapis.com/auth/calendar.events,https://www.googleapis.com/auth/calendar.calendarlist.readonly";
pub const INSTALL_HINT: &str = "install Google's Workspace CLI: `npm install -g @googleworkspace/cli` (or a release binary from https://github.com/googleworkspace/cli/releases)";

const TIMEOUT: Duration = Duration::from_secs(90);
const LOGIN_TIMEOUT: Duration = Duration::from_secs(600);
/// Pages of events read per calendar and window, at most.
const MAX_PAGES: usize = 10;

pub struct Google {
    name: String,
    command: String,
    dir: PathBuf,
    client: Option<(String, String)>,
}

fn text(v: &Value) -> String {
    v.as_str().unwrap_or_default().trim().to_string()
}

fn nonblank(v: Option<String>) -> Option<String> {
    v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// `start` / `end` of a Google event: `{dateTime}` or `{date}`.
pub fn parse_when(v: &Value) -> Option<Time> {
    if let Some(dt) = v["dateTime"].as_str() {
        return DateTime::parse_from_rfc3339(dt).ok().map(|t| Time::At(t.with_timezone(&Utc)));
    }
    v["date"].as_str().and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok()).map(Time::Date)
}

pub fn when_json(t: &Time) -> Value {
    match t {
        Time::At(t) => json!({ "dateTime": t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true) }),
        Time::Date(d) => json!({ "date": d.format("%Y-%m-%d").to_string() }),
    }
}

/// cloud-calendar's own gws directory for an account, beside its config file.
pub fn gws_dir(name: &str) -> PathBuf {
    crate::config::config_dir().join("gws").join(name)
}

fn sign_in_url(line: &str) -> Option<String> {
    line.split_whitespace().find(|w| w.starts_with("https://accounts.google.com/")).map(str::to_string)
}

fn open_browser(url: &str) {
    let program = nonblank(std::env::var(BROWSER_ENV).ok()).unwrap_or_else(|| "xdg-open".into());
    let _ = Command::new(program).arg(url).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
}

impl Google {
    pub fn new(name: &str, _cfg: &AccountConfig) -> Self {
        let command = nonblank(std::env::var(COMMAND_ENV).ok()).unwrap_or_else(|| "gws".into());
        let env = |k| nonblank(std::env::var(k).ok());
        let builtin = (nonblank(Some(GOOGLE_CLIENT_ID.to_string())), nonblank(Some(GOOGLE_CLIENT_SECRET.to_string())));
        let client = [(env(CLIENT_ID_ENV), env(CLIENT_SECRET_ENV)), builtin].into_iter().find_map(|(id, secret)| Some((id?, secret?)));
        Self { name: name.into(), command, dir: gws_dir(name), client }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn signed_in(&self) -> bool {
        self.dir.join("credentials.enc").is_file()
    }

    fn fail(&self, kind: ErrorKind, message: impl AsRef<str>) -> Error {
        Error::new(kind, format!("Google: {}", message.as_ref()))
    }

    fn missing(&self) -> Error {
        self.fail(ErrorKind::AccountUnavailable, format!("Google's Workspace CLI isn't installed (no `{}` on PATH); {INSTALL_HINT}", self.command))
    }

    /// Fails, saying so, when this build has no Google OAuth client to sign in with.
    pub fn require_client(&self) -> Result<()> {
        self.client().map(|_| ())
    }

    fn client(&self) -> Result<(String, String)> {
        self.client.clone().ok_or_else(|| {
            Error::new(ErrorKind::Config, format!("Google sign-in isn't configured in this build of Cloud Calendar (no Google OAuth client; set {CLIENT_ID_ENV} and {CLIENT_SECRET_ENV} to use one of your own)"))
        })
    }

    fn gws(&self) -> Result<Command> {
        let (id, secret) = self.client()?;
        let mut cmd = Command::new(&self.command);
        // gws also reads a .env from its working directory.
        cmd.current_dir(if self.dir.is_dir() { self.dir.clone() } else { std::env::temp_dir() })
            .env("GOOGLE_WORKSPACE_CLI_CONFIG_DIR", &self.dir)
            .env("GOOGLE_WORKSPACE_CLI_KEYRING_BACKEND", "file")
            .env("GOOGLE_APPLICATION_CREDENTIALS", self.dir.join("no-application-default-credentials.json"))
            .env_remove("GOOGLE_WORKSPACE_CLI_TOKEN")
            .env_remove("GOOGLE_WORKSPACE_CLI_CREDENTIALS_FILE")
            .env("GOOGLE_WORKSPACE_CLI_CLIENT_ID", id)
            .env("GOOGLE_WORKSPACE_CLI_CLIENT_SECRET", secret);
        Ok(cmd)
    }

    /// Google's browser sign-in through `gws auth login`: gws prints a link, opened here in the
    /// browser, and waits for Google to send the browser back.
    pub fn login(&self) -> Result<()> {
        let mut cmd = self.gws()?;
        crate::config::private_dir(&self.dir).map_err(|e| self.fail(ErrorKind::AccountUnavailable, format!("could not create {}: {e}", self.dir.display())))?;
        cmd.current_dir(&self.dir).args(["auth", "login", "--scopes", SCOPES]).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = cmd.spawn().map_err(|e| if e.kind() == std::io::ErrorKind::NotFound { self.missing() } else { self.fail(ErrorKind::AccountUnavailable, e.to_string()) })?;
        let opened = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let watch = |pipe: Box<dyn Read + Send>| {
            let opened = opened.clone();
            std::thread::spawn(move || {
                let mut all = String::new();
                for line in BufReader::new(pipe).lines().map_while(std::result::Result::ok) {
                    if let Some(url) = sign_in_url(&line) {
                        if !opened.swap(true, std::sync::atomic::Ordering::SeqCst) {
                            open_browser(&url);
                        }
                        eprintln!("{line}");
                    }
                    all.push_str(&line);
                    all.push('\n');
                }
                all
            })
        };
        let out = child.stdout.take().map(|p| watch(Box::new(p)));
        let err = child.stderr.take().map(|p| watch(Box::new(p)));
        let deadline = Instant::now() + LOGIN_TIMEOUT;
        let status = loop {
            match child.try_wait() {
                Ok(Some(s)) => break s,
                Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
                Ok(None) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(self.fail(ErrorKind::AccountAuth, "the Google sign-in wasn't finished within 10 minutes"));
                }
                Err(e) => return Err(self.fail(ErrorKind::AccountUnavailable, e.to_string())),
            }
        };
        let _ = out.and_then(|h| h.join().ok());
        let stderr = err.and_then(|h| h.join().ok()).unwrap_or_default();
        if !status.success() || !self.signed_in() {
            let last = stderr.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").to_string();
            return Err(self.fail(ErrorKind::AccountAuth, format!("the Google sign-in didn't finish ({status}) {last}")));
        }
        Ok(())
    }

    /// Signs out on this computer: removes cloud-calendar's gws directory.
    pub fn forget(&self) -> std::io::Result<bool> {
        match std::fs::remove_dir_all(&self.dir) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// `gws calendar <resource> <method> --params <params> [--json <body>]`.
    fn call(&self, resource: &str, method: &str, params: Value, body: Option<&Value>) -> Result<Value> {
        if !self.signed_in() {
            return Err(self.fail(ErrorKind::AccountAuth, format!("not signed in; run `cloud-calendar account login {}`", self.name)));
        }
        let what = format!("{resource} {method}");
        let mut cmd = self.gws()?;
        cmd.args(["calendar", resource, method]).arg("--params").arg(params.to_string());
        if let Some(b) = body {
            cmd.arg("--json").arg(b.to_string());
        }
        let (status, stdout, stderr) = match run_command(&mut cmd, None, TIMEOUT) {
            Run::Done { status, stdout, stderr } => (status, stdout, stderr),
            Run::Missing => return Err(self.missing()),
            Run::TimedOut => return Err(self.fail(ErrorKind::AccountUnavailable, format!("`gws calendar {what}` took longer than {}s", TIMEOUT.as_secs()))),
            Run::Failed(e) => return Err(self.fail(ErrorKind::AccountUnavailable, format!("could not run {}: {e}", self.command))),
        };
        let parsed: std::result::Result<Value, _> = serde_json::from_slice(&stdout);
        if status.success() {
            if String::from_utf8_lossy(&stdout).trim().is_empty() {
                return Ok(Value::Null);
            }
            return parsed.map_err(|e| self.fail(ErrorKind::AccountUnavailable, format!("`gws calendar {what}` answered something that isn't JSON ({e})")));
        }
        Err(self.gws_error(status.code(), &parsed.unwrap_or(Value::Null), &stderr, &what))
    }

    /// gws's exit statuses: 1 API error (Google's HTTP status in the JSON), 2 auth, 3 validation,
    /// 4 discovery (Google unreachable), 5 other; the error JSON goes to stdout.
    fn gws_error(&self, code: Option<i32>, out: &Value, stderr: &str, what: &str) -> Error {
        let err = &out["error"];
        let mut message = text(&err["message"]);
        if message.is_empty() {
            message = stderr.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").trim().to_string();
        }
        if message.is_empty() {
            message = format!("`gws calendar {what}` failed");
        }
        let message: String = message.lines().next().unwrap_or("").chars().take(300).collect();
        let again = format!("run `cloud-calendar account login {}` to sign in again", self.name);
        match (code, err["code"].as_i64().unwrap_or(0)) {
            (Some(2), _) | (Some(1), 401) => self.fail(ErrorKind::AccountAuth, format!("the Google sign-in expired or was revoked ({message}); {again}")),
            (Some(1), 403) if message.to_ascii_lowercase().contains("insufficient") => self.fail(ErrorKind::AccountAuth, format!("not allowed ({message}); {again}")),
            (Some(1), 403) => self.fail(ErrorKind::BadRequest, message),
            (Some(1), 404 | 410) => self.fail(ErrorKind::NotFound, message),
            (Some(1), 400) => self.fail(ErrorKind::BadRequest, message),
            (Some(1), 429) => self.fail(ErrorKind::AccountUnavailable, format!("Google is rate limiting ({message}); try again shortly")),
            (Some(4), _) => self.fail(ErrorKind::AccountUnavailable, format!("couldn't reach Google ({message})")),
            (Some(3), _) => self.fail(ErrorKind::AccountUnavailable, format!("gws rejected `{what}` ({message}); is gws up to date?")),
            _ => self.fail(ErrorKind::AccountUnavailable, message),
        }
    }

    /// Every entry of the calendar list.
    fn calendar_list(&self) -> Result<Vec<Value>> {
        let mut items = Vec::new();
        let mut token: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let mut params = json!({ "maxResults": 250 });
            if let Some(t) = &token {
                params["pageToken"] = json!(t);
            }
            let page = self.call("calendarList", "list", params, None)?;
            items.extend(page["items"].as_array().cloned().unwrap_or_default());
            token = nonblank(page["nextPageToken"].as_str().map(str::to_string));
            if token.is_none() {
                break;
            }
        }
        Ok(items)
    }

    fn to_calendar(&self, c: &Value) -> Option<Calendar> {
        let id = text(&c["id"]);
        if id.is_empty() {
            return None;
        }
        let name = nonblank(c["summaryOverride"].as_str().map(str::to_string)).unwrap_or_else(|| text(&c["summary"]));
        Some(Calendar {
            id: format!("{}:{id}", self.name),
            account: self.name.clone(),
            name: if name.is_empty() { id.clone() } else { name },
            color: nonblank(c["backgroundColor"].as_str().map(str::to_string)),
            writable: matches!(c["accessRole"].as_str(), Some("owner" | "writer")),
        })
    }

    fn calendar_events(&self, cal: &Calendar, range: &Range) -> Result<Vec<Event>> {
        let cal_id = provider::local_id(&self.name, "Google", &cal.id)?;
        let mut out = Vec::new();
        let mut token: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let mut params = json!({
                "calendarId": cal_id,
                "timeMin": range.start.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                "timeMax": range.end.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                "singleEvents": true,
                "orderBy": "startTime",
                "maxResults": 2500,
            });
            if let Some(t) = &token {
                params["pageToken"] = json!(t);
            }
            let page = self.call("events", "list", params, None)?;
            for e in page["items"].as_array().into_iter().flatten() {
                if e["status"] == "cancelled" {
                    continue;
                }
                let (Some(start), Some(end)) = (parse_when(&e["start"]), parse_when(&e["end"])) else { continue };
                let recurring = e["recurringEventId"].as_str().is_some_and(|r| !r.is_empty());
                let suffix = if recurring { provider::occurrence_suffix(&start) } else { String::new() };
                let title = text(&e["summary"]);
                out.push(Event {
                    id: format!("{}:{cal_id}/{}{suffix}", self.name, text(&e["id"])),
                    account: self.name.clone(),
                    calendar_id: cal.id.clone(),
                    calendar: cal.name.clone(),
                    color: cal.color.clone(),
                    title: if title.is_empty() { "(no title)".into() } else { title },
                    all_day: start.is_date(),
                    start,
                    end,
                    location: nonblank(e["location"].as_str().map(str::to_string)),
                    notes: nonblank(e["description"].as_str().map(str::to_string)),
                    recurring,
                });
            }
            token = nonblank(page["nextPageToken"].as_str().map(str::to_string));
            if token.is_none() {
                break;
            }
        }
        Ok(out)
    }

    /// `(calendarId, eventId, is an occurrence)` from an event ID.
    fn event_ref<'a>(&self, id: &'a str) -> Result<(&'a str, &'a str, bool)> {
        let local = provider::local_id(&self.name, "Google", id)?;
        let (base, occurrence) = provider::split_occurrence(local);
        let (cal, ev) = base.rsplit_once('/').filter(|(c, e)| !c.is_empty() && !e.is_empty()).ok_or_else(|| self.fail(ErrorKind::NotFound, format!("{id} isn't a Google event ID")))?;
        Ok((cal, ev, occurrence.is_some()))
    }

    /// The event an action reaches: an occurrence's series, else the event itself; with it, the
    /// event as Google has it.
    fn target(&self, id: &str) -> Result<(String, String, Value)> {
        let (cal, ev, occurrence) = self.event_ref(id)?;
        let current = self.call("events", "get", json!({ "calendarId": cal, "eventId": ev }), None)?;
        let series = text(&current["recurringEventId"]);
        if occurrence && !series.is_empty() {
            return Ok((cal.to_string(), series, current));
        }
        Ok((cal.to_string(), ev.to_string(), current))
    }
}

impl Provider for Google {
    fn name(&self) -> &str {
        &self.name
    }

    fn label(&self) -> &str {
        "Google"
    }

    fn status(&self) -> AccountStatus {
        let mut s = AccountStatus { name: self.name.clone(), provider: "google".into(), label: "Google".into(), ..Default::default() };
        if !self.signed_in() {
            s.detail = format!("not signed in: run `cloud-calendar account login {}`", self.name);
            return s;
        }
        match self.calendar_list() {
            Ok(c) => {
                s.ok = true;
                s.detail = format!("signed in via {} ({} calendars)", self.command, c.len());
            }
            Err(e) => s.detail = e.message,
        }
        s
    }

    fn sign_in(&self) -> Result<()> {
        self.login()
    }

    fn calendars(&self) -> Result<Vec<Calendar>> {
        Ok(self.calendar_list()?.iter().filter_map(|c| self.to_calendar(c)).collect())
    }

    fn events(&self, range: &Range) -> Result<Vec<Event>> {
        let list = self.calendar_list()?;
        let cals: Vec<Calendar> = list.iter().filter(|c| c["selected"] != json!(false)).filter_map(|c| self.to_calendar(c)).collect();
        let results: Vec<Result<Vec<Event>>> = std::thread::scope(|s| {
            let handles: Vec<_> = cals.iter().map(|c| s.spawn(move || self.calendar_events(c, range))).collect();
            handles.into_iter().map(|h| h.join().unwrap_or_else(|_| Err(self.fail(ErrorKind::AccountUnavailable, "failed unexpectedly")))).collect()
        });
        let mut out = Vec::new();
        for r in results {
            out.extend(r?);
        }
        Ok(out)
    }

    fn create(&self, event: &NewEvent) -> Result<String> {
        check_span(&event.start, &event.end)?;
        let cal = provider::local_id(&self.name, "Google", &event.calendar_id)?;
        let mut body = json!({ "summary": event.title, "start": when_json(&event.start), "end": when_json(&event.end) });
        if let Some(l) = event.location.as_deref().filter(|l| !l.trim().is_empty()) {
            body["location"] = json!(l);
        }
        if let Some(n) = event.notes.as_deref().filter(|n| !n.trim().is_empty()) {
            body["description"] = json!(n);
        }
        let created = self.call("events", "insert", json!({ "calendarId": cal }), Some(&body))?;
        let id = text(&created["id"]);
        if id.is_empty() {
            return Err(self.fail(ErrorKind::AccountUnavailable, "unexpected answer to `gws calendar events insert` (no id)"));
        }
        Ok(format!("{}:{cal}/{id}", self.name))
    }

    fn update(&self, id: &str, change: &EventChange) -> Result<()> {
        let (_, _, occurrence) = self.event_ref(id)?;
        if occurrence {
            provider::refuse_series_move("Google", change)?;
        }
        let (cal, target, current) = self.target(id)?;
        let mut body = Map::new();
        if let Some(t) = &change.title {
            body.insert("summary".into(), json!(t));
        }
        if let Some(l) = &change.location {
            body.insert("location".into(), json!(l.trim()));
        }
        if let Some(n) = &change.notes {
            body.insert("description".into(), json!(n.trim()));
        }
        if change.moves() {
            let (Some(s), Some(e)) = (parse_when(&current["start"]), parse_when(&current["end"])) else {
                return Err(self.fail(ErrorKind::AccountUnavailable, "the event has no start or end to move"));
            };
            let (s, e) = provider::changed_span(change, s, e)?;
            body.insert("start".into(), when_json(&s));
            body.insert("end".into(), when_json(&e));
        }
        if body.is_empty() {
            return Ok(());
        }
        self.call("events", "patch", json!({ "calendarId": cal, "eventId": target }), Some(&Value::Object(body))).map(|_| ())
    }

    fn delete(&self, id: &str) -> Result<()> {
        let (cal, ev, occurrence) = self.event_ref(id)?;
        let target = if occurrence { self.target(id)?.1 } else { ev.to_string() };
        self.call("events", "delete", json!({ "calendarId": cal, "eventId": target }), None).map(|_| ())
    }
}
