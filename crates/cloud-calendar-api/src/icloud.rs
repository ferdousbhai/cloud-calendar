//! iCloud Calendar through icloud.com's own calendar web service, signed in through
//! icloud-session: the one iCloud web sign-in every app on this machine shares
//! (github.com/ferdousbhai/icloud-for-omarchy, `session/`). Its client fetches the cookies, client
//! parameters and `webservices` from icloud-sessiond's D-Bus `Session()`, sends each request to
//! Apple with them, hands rotated cookies back and reports a refused session, so there is no
//! password, token or sign-in of cloud-calendar's own. Signing in asks icloud-sessiond to open
//! its sign-in window (`SignIn()`).
//!
//! Apple doesn't document this service. What is used here, and where it comes from:
//!
//! - Base URL `webservices["calendar"] + "/ca"`; reads `GET /ca/allcollections` (calendars:
//!   `Collection[]` with `guid`, `title`, `color`, `readOnly`, `ctag`), `GET /ca/events`
//!   (`Event[]` over `startDate`..`endDate`, both inclusive, as `YYYY-MM-DD`, with `lang` and
//!   `usertz`) and `GET /ca/eventdetail/{pGuid}/{guid}` (`Event[0]`, with its `etag`).
//!   Source: timlaing/pyicloud `pyicloud/services/calendar.py` (commit 850e0ea, 2026-08-31);
//!   picklepete/pyicloud `pyicloud/services/calendar.py` (09fb9ba) for the original reads.
//! - Adding: `POST /ca/events/{pGuid}/{guid}` with `{Event, Invitee, Alarm, ClientState:
//!   {Collection: [{guid, ctag}]}}`, `ctag` from a fresh `allcollections`, `etag` empty, query
//!   `startDate`/`endDate` covering the event. Deleting: the same URL with
//!   `methodOverride=DELETE&ifMatch=<etag>` and an empty `Event`. Sources: timlaing/pyicloud
//!   `add_event`/`remove_event` (above); ai-ecoverse/skills `skills/icloud/references/api-notes.md`
//!   (a write made 2026-08-24) for the query window and all-day encoding.
//! - Changing: the same URL with `methodOverride=PUT&ifMatch=<etag>` and the whole event. Source:
//!   ElyaConrad/iCloud-API `resources/apps/Calendar.js` `changeEvent`/`__event` (commit c284671,
//!   2017); no newer client does it, so this is the least established call here.
//! - Dates are `[YYYYMMDD, year, month, day, hour, minute, n]`; `n` is minutes since midnight on
//!   a start, and not to be trusted on a returned end (ai-ecoverse notes), so only the first six
//!   are read. A timed event's wall-clock times are in its `tz` (timlaing/pyicloud:
//!   "Apple sends wall-clock time with the zone carried separately on the event"); an all-day
//!   event has `tz` null and an exclusive end date, `duration` = days × 1440 (ai-ecoverse notes).
//!
//! Not established from any source, so not relied on: whether `/ca/events` lists every
//! occurrence of a repeating event or only its first (events with a `recurrence` are marked
//! repeating and shown as listed), and how a whole series is changed or deleted (`/this` and
//! `/future` exist per ElyaConrad, but not "the whole series"), so repeating iCloud events are
//! read-only here.
//!
//! IDs: a calendar is `icloud:<pGuid>`, an event `icloud:<pGuid>/<guid>`.

use chrono::{Datelike, NaiveDate, NaiveDateTime, TimeZone, Timelike, Utc};
use chrono_tz::Tz;
use serde_json::{Value, json};
use std::sync::Mutex;

use crate::config::AccountConfig;
use crate::error::{Error, ErrorKind, Result};
use crate::provider::{self, AccountStatus, Provider};
use crate::types::*;

/// icloud-sessiond unreachable: not installed (D-Bus has no such service), or failing.
pub fn service_error(message: &str) -> Error {
    if message.contains("org.freedesktop.DBus.Error.ServiceUnknown") {
        return Error::new(
            ErrorKind::Config,
            "iCloud: icloud-session isn't installed. iCloud calendars use the iCloud sign-in it keeps for your iCloud apps; install it from icloud-for-omarchy (the icloud-session package), then add iCloud again",
        );
    }
    Error::new(ErrorKind::AccountUnavailable, format!("iCloud: icloud-session failed ({message})"))
}

pub struct ICloud {
    name: String,
    session: Mutex<Option<icloud_session::Session>>,
}

/// The local time zone's IANA name and zone: `TZ` when set (as for every other program),
/// else the system's.
pub fn zone() -> Result<(String, Tz)> {
    let name = match std::env::var("TZ").ok().map(|t| t.trim_start_matches(':').to_string()).filter(|t| !t.is_empty()) {
        Some(t) => t,
        None => iana_time_zone::get_timezone().map_err(|e| Error::new(ErrorKind::Config, format!("this computer's time zone is unknown ({e})")))?,
    };
    let tz: Tz = name.parse().map_err(|_| Error::new(ErrorKind::Config, format!("\"{name}\" isn't a known time zone (an IANA name such as Europe/London)")))?;
    Ok((name, tz))
}

/// Apple's date array for a wall-clock time.
pub fn apple_date(t: NaiveDateTime) -> Value {
    let minutes = t.hour() * 60 + t.minute();
    json!([t.format("%Y%m%d").to_string().parse::<u32>().unwrap_or(0), t.year(), t.month(), t.day(), t.hour(), t.minute(), minutes])
}

/// The wall-clock time in an Apple date array (its first six fields).
pub fn read_apple_date(v: &Value) -> Option<NaiveDateTime> {
    let a = v.as_array()?;
    let n = |i: usize| a.get(i)?.as_i64();
    NaiveDate::from_ymd_opt(n(1)? as i32, n(2)? as u32, n(3)? as u32)?.and_hms_opt(n(4)? as u32, n(5)? as u32, 0)
}

/// An event's start and end from its date arrays, `allDay` and `tz`.
pub fn event_span(e: &Value, local: &Tz) -> Result<(Time, Time)> {
    let bad = |what: &str| Error::new(ErrorKind::AccountUnavailable, format!("iCloud: an event's {what} wasn't understood"));
    let start = read_apple_date(&e["startDate"]).ok_or_else(|| bad("start"))?;
    let end = read_apple_date(&e["endDate"]).ok_or_else(|| bad("end"))?;
    if e["allDay"] == json!(true) {
        let (s, x) = (start.date(), end.date());
        let next = s.checked_add_days(chrono::Days::new(1)).ok_or_else(|| bad("start date"))?;
        return Ok((Time::Date(s), Time::Date(x.max(next))));
    }
    let zone: Tz = match e["tz"].as_str().filter(|z| !z.is_empty()) {
        Some(z) => z.parse().map_err(|_| bad(&format!("time zone ({z})")))?,
        // No zone: a floating time, read in your own.
        None => *local,
    };
    let at = |t: NaiveDateTime| zone.from_local_datetime(&t).earliest().map(|x| Time::At(x.with_timezone(&Utc))).ok_or_else(|| bad("time"));
    let (s, x) = (at(start)?, at(end)?);
    Ok((s, x.max(s)))
}

/// The date arrays, `duration`, `allDay` and `tz` for a span, written in `local`.
fn span_fields(start: &Time, end: &Time, local: &(String, Tz)) -> serde_json::Map<String, Value> {
    let mut m = serde_json::Map::new();
    let (s, e, all_day, duration) = match (start, end) {
        (Time::Date(s), Time::Date(e)) => {
            let midnight = |d: &NaiveDate| d.and_hms_opt(0, 0, 0).unwrap_or_default();
            (midnight(s), midnight(e), true, (*e - *s).num_days() * 1440)
        }
        _ => {
            let wall = |t: &Time| t.instant().with_timezone(&local.1).naive_local();
            (wall(start), wall(end), false, (end.instant() - start.instant()).num_minutes())
        }
    };
    for (k, v) in [("startDate", s), ("localStartDate", s)] {
        m.insert(k.into(), apple_date(v));
    }
    for (k, v) in [("endDate", e), ("localEndDate", e)] {
        m.insert(k.into(), apple_date(v));
    }
    m.insert("allDay".into(), json!(all_day));
    m.insert("duration".into(), json!(duration));
    m.insert("tz".into(), if all_day { Value::Null } else { json!(local.0) });
    m
}

fn upper_uuid() -> String {
    // A random-enough v4-shaped GUID: time, process and a counter, never repeated on one machine.
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let mix = nanos ^ (u128::from(std::process::id()) << 64) ^ (u128::from(COUNTER.fetch_add(1, Ordering::Relaxed)) << 96);
    let hex = format!("{mix:032X}");
    format!("{}-{}-4{}-A{}-{}", &hex[0..8], &hex[8..12], &hex[13..16], &hex[17..20], &hex[20..32])
}

fn percent(s: &str) -> String {
    s.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-._~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect()
}

fn query(params: &[(&str, String)]) -> String {
    params.iter().map(|(k, v)| format!("{k}={}", percent(v))).collect::<Vec<_>>().join("&")
}

fn day(d: NaiveDate) -> String {
    d.format("%Y-%m-%d").to_string()
}

impl ICloud {
    pub fn new(name: &str, _cfg: &AccountConfig) -> Self {
        Self { name: name.into(), session: Mutex::new(None) }
    }

    fn fail(&self, kind: ErrorKind, message: impl AsRef<str>) -> Error {
        Error::new(kind, format!("iCloud: {}", message.as_ref()))
    }

    fn map(&self, e: icloud_session::Error) -> Error {
        use icloud_session::Error as S;
        match e {
            S::SignInRequired => {
                *self.session.lock().unwrap() = None;
                self.fail(ErrorKind::AccountAuth, format!("signed out; sign in again (`cloud-calendar account login {}`, or in the app)", self.name))
            }
            // icloud-sessiond is still signed in but can't read its cookie jar from the keyring.
            S::KeyringUnavailable(m) => self.fail(ErrorKind::KeyringUnavailable, format!("icloud-session can't read its sign-in from the keyring ({m}); unlock or start the keyring")),
            S::Http { status: 404, .. } => self.fail(ErrorKind::NotFound, "no such event or calendar (deleted elsewhere?)"),
            S::Http { status: 412, .. } => self.fail(ErrorKind::BadRequest, "the event changed elsewhere while it was being saved; reload and try again"),
            S::Http { status, body } => self.fail(ErrorKind::AccountUnavailable, format!("Apple answered HTTP {status}: {}", body.chars().take(200).collect::<String>())),
            S::Offline(m) | S::Network(m) => self.fail(ErrorKind::AccountUnavailable, format!("couldn't reach iCloud ({m})")),
            S::Service(m) => service_error(&m),
            other => self.fail(ErrorKind::AccountUnavailable, other.to_string()),
        }
    }

    fn session(&self) -> Result<icloud_session::Session> {
        if let Some(s) = self.session.lock().unwrap().clone() {
            return Ok(s);
        }
        let s = icloud_session::Session::connect().map_err(|e| self.map(e))?;
        *self.session.lock().unwrap() = Some(s.clone());
        Ok(s)
    }

    fn base(&self, s: &icloud_session::Session) -> Result<String> {
        let ws = s.webservices().map_err(|e| self.map(e))?;
        let url = ws.url("calendar").ok_or_else(|| self.fail(ErrorKind::AccountUnavailable, "iCloud offers no Calendar service for this account"))?;
        Ok(format!("{}/ca", url.trim_end_matches('/')))
    }

    fn get(&self, path: &str, params: &[(&str, String)]) -> Result<Value> {
        let s = self.session()?;
        let url = format!("{}/{path}?{}", self.base(&s)?, query(params));
        s.get(&url).and_then(|r| r.json()).map_err(|e| self.map(e))
    }

    fn post(&self, path: &str, params: &[(&str, String)], body: &Value) -> Result<Value> {
        let s = self.session()?;
        let url = format!("{}/{path}?{}", self.base(&s)?, query(params));
        let r = s.post_json(&url, body).map_err(|e| self.map(e))?;
        Ok(serde_json::from_slice(&r.body).unwrap_or(Value::Null))
    }

    fn common(&self, zone: &str, from: NaiveDate, to: NaiveDate) -> Vec<(&'static str, String)> {
        vec![("lang", "en-us".into()), ("usertz", zone.into()), ("startDate", day(from)), ("endDate", day(to))]
    }

    fn collections(&self) -> Result<Vec<Value>> {
        let (zone, tz) = zone()?;
        let today = Utc::now().with_timezone(&tz).date_naive();
        let data = self.get("allcollections", &self.common(&zone, today, today))?;
        data["Collection"].as_array().cloned().ok_or_else(|| self.fail(ErrorKind::AccountUnavailable, "unexpected answer to /ca/allcollections (no Collection)"))
    }

    fn calendar_of(&self, c: &Value) -> Option<Calendar> {
        let guid = c["guid"].as_str().filter(|g| !g.is_empty())?;
        Some(Calendar {
            id: format!("{}:{guid}", self.name),
            account: self.name.clone(),
            name: c["title"].as_str().unwrap_or(guid).to_string(),
            color: c["color"].as_str().filter(|c| c.starts_with('#')).map(|c| c.chars().take(7).collect()),
            writable: c["readOnly"] != json!(true),
        })
    }

    /// `(pGuid, guid)` of an event ID, refusing occurrences of a repeating event.
    fn event_ref<'a>(&self, id: &'a str, action: &str) -> Result<(&'a str, &'a str)> {
        let local = provider::local_id(&self.name, "iCloud", id)?;
        let (base, occurrence) = provider::split_occurrence(local);
        if occurrence.is_some() {
            return Err(self.fail(
                ErrorKind::BadRequest,
                format!("this is a repeating event, which Cloud Calendar can't {action} on iCloud yet: icloud.com's way of changing a whole series isn't established; use the Calendar app or icloud.com"),
            ));
        }
        base.split_once('/').filter(|(p, g)| !p.is_empty() && !g.is_empty()).ok_or_else(|| self.fail(ErrorKind::NotFound, format!("{id} isn't an iCloud event ID")))
    }

    fn ctag(&self, pguid: &str) -> Result<Value> {
        let c = self.collections()?.into_iter().find(|c| c["guid"] == json!(pguid)).ok_or_else(|| self.fail(ErrorKind::NotFound, format!("no calendar {}:{pguid}", self.name)))?;
        if c["readOnly"] == json!(true) {
            return Err(self.fail(ErrorKind::BadRequest, format!("{} is read-only", c["title"].as_str().unwrap_or(pguid))));
        }
        Ok(c["ctag"].clone())
    }

    /// The event as `/ca/eventdetail` has it, with its etag.
    fn detail(&self, pguid: &str, guid: &str) -> Result<(Value, Value, String)> {
        let (zone, _) = zone()?;
        let data = self.get(&format!("eventdetail/{pguid}/{guid}"), &[("lang", "en-us".into()), ("usertz", zone)])?;
        let event = data["Event"].get(0).cloned().ok_or_else(|| self.fail(ErrorKind::NotFound, "no such event (deleted elsewhere?)"))?;
        let etag = event["etag"].as_str().filter(|e| !e.is_empty()).ok_or_else(|| self.fail(ErrorKind::AccountUnavailable, "the event came without an etag"))?.to_string();
        Ok((event, data, etag))
    }

    pub fn sign_in(&self) -> Result<()> {
        let r = crate::accounts::icloud_sign_in();
        *self.session.lock().unwrap() = None;
        r
    }
}

impl Provider for ICloud {
    fn name(&self) -> &str {
        &self.name
    }

    fn label(&self) -> &str {
        "iCloud"
    }

    fn status(&self) -> AccountStatus {
        let mut s = AccountStatus { name: self.name.clone(), provider: "icloud".into(), label: "iCloud".into(), ..Default::default() };
        match icloud_session::status() {
            Ok(st) if st.signed_in => {
                s.ok = true;
                let who = st.apple_id.unwrap_or_default();
                s.detail = match st.full_name {
                    Some(name) => format!("signed in as {name} ({who}) through icloud-session"),
                    None => format!("signed in as {who} through icloud-session"),
                };
            }
            Ok(_) => s.detail = "signed out of iCloud; sign in again".into(),
            Err(e) => s.detail = self.map(e).message,
        }
        s
    }

    fn sign_in(&self) -> Result<()> {
        ICloud::sign_in(self)
    }

    fn calendars(&self) -> Result<Vec<Calendar>> {
        Ok(self.collections()?.iter().filter_map(|c| self.calendar_of(c)).collect())
    }

    fn events(&self, range: &Range) -> Result<Vec<Event>> {
        let (zone, tz) = zone()?;
        let calendars = self.calendars()?;
        let from = range.start.with_timezone(&tz).date_naive();
        let to = (range.end - chrono::Duration::seconds(1)).with_timezone(&tz).date_naive().max(from);
        let data = self.get("events", &self.common(&zone, from, to))?;
        let list = data["Event"].as_array().cloned().unwrap_or_default();
        let mut out = Vec::new();
        for e in &list {
            let (Some(pguid), Some(guid)) = (e["pGuid"].as_str(), e["guid"].as_str()) else { continue };
            let (start, end) = event_span(e, &tz)?;
            if !range.overlaps(&start, &end) {
                continue;
            }
            let recurring = e["recurrence"].as_str().is_some_and(|r| !r.is_empty()) || e["recurrenceMaster"] == json!(true);
            let suffix = if recurring { provider::occurrence_suffix(&start) } else { String::new() };
            let cal = calendars.iter().find(|c| c.id == format!("{}:{pguid}", self.name));
            let text = |k: &str| e[k].as_str().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);
            out.push(Event {
                id: format!("{}:{pguid}/{guid}{suffix}", self.name),
                account: self.name.clone(),
                calendar_id: format!("{}:{pguid}", self.name),
                calendar: cal.map(|c| c.name.clone()).unwrap_or_else(|| pguid.to_string()),
                color: cal.and_then(|c| c.color.clone()),
                title: text("title").unwrap_or_else(|| "(no title)".into()),
                all_day: start.is_date(),
                start,
                end,
                location: text("location"),
                notes: text("description"),
                recurring,
            });
        }
        Ok(out)
    }

    fn create(&self, event: &NewEvent) -> Result<String> {
        check_span(&event.start, &event.end)?;
        let local = zone()?;
        let pguid = provider::local_id(&self.name, "iCloud", &event.calendar_id)?;
        let ctag = self.ctag(pguid)?;
        let guid = upper_uuid();
        let now = apple_date(Utc::now().with_timezone(&local.1).naive_local());
        let mut ev = json!({
            "title": event.title, "icon": 0, "pGuid": pguid, "guid": guid,
            "createdDate": now, "lastModifiedDate": now,
            "extendedDetailsAreIncluded": true, "recurrenceException": false, "recurrenceMaster": false,
            "hasAttachments": false, "readOnly": false, "transparent": false,
            "birthdayIsYearlessBday": false, "birthdayShowAsCompany": false, "shouldShowJunkUIWhenAppropriate": false,
            "location": event.location.clone().unwrap_or_default(), "url": "", "description": event.notes.clone().unwrap_or_default(),
            "etag": "", "alarms": [], "attachments": [], "invitees": [], "changeRecurring": null,
        });
        ev.as_object_mut().unwrap().extend(span_fields(&event.start, &event.end, &local));
        let body = json!({ "Event": ev, "Invitee": [], "Alarm": [], "ClientState": { "Collection": [{ "guid": pguid, "ctag": ctag }], "fullState": false } });
        let (from, to) = (event.start.instant().with_timezone(&local.1).date_naive(), event.end.instant().with_timezone(&local.1).date_naive());
        self.post(&format!("events/{pguid}/{guid}"), &self.common(&local.0, from, to), &body)?;
        Ok(format!("{}:{pguid}/{guid}", self.name))
    }

    fn update(&self, id: &str, change: &EventChange) -> Result<()> {
        let (pguid, guid) = self.event_ref(id, "change")?;
        let local = zone()?;
        let (mut ev, detail, etag) = self.detail(pguid, guid)?;
        let (start, end) = event_span(&ev, &local.1)?;
        let (s, e) = if change.moves() { provider::changed_span(change, start, end)? } else { (start, end) };
        let o = ev.as_object_mut().ok_or_else(|| self.fail(ErrorKind::AccountUnavailable, "the event wasn't understood"))?;
        if let Some(t) = &change.title {
            o.insert("title".into(), json!(t));
        }
        if let Some(l) = &change.location {
            o.insert("location".into(), json!(l.trim()));
        }
        if let Some(n) = &change.notes {
            o.insert("description".into(), json!(n.trim()));
        }
        if change.moves() {
            o.extend(span_fields(&s, &e, &local));
        }
        o.insert("lastModifiedDate".into(), apple_date(Utc::now().with_timezone(&local.1).naive_local()));
        // The event's own alarms and invitees go back as they came.
        let mine = |k: &str| Value::Array(detail[k].as_array().into_iter().flatten().filter(|a| a["pGuid"] == json!(guid)).cloned().collect());
        let body = json!({ "Event": ev, "Invitee": mine("Invitee"), "Alarm": mine("Alarm"), "ClientState": { "Collection": [{ "guid": pguid, "ctag": self.ctag(pguid)? }], "fullState": false } });
        let (from, to) = (s.instant().min(start.instant()).with_timezone(&local.1).date_naive(), e.instant().max(end.instant()).with_timezone(&local.1).date_naive());
        let mut params = self.common(&local.0, from, to);
        params.extend([("methodOverride", "PUT".to_string()), ("ifMatch", etag)]);
        self.post(&format!("events/{pguid}/{guid}"), &params, &body).map(|_| ())
    }

    fn delete(&self, id: &str) -> Result<()> {
        let (pguid, guid) = self.event_ref(id, "delete")?;
        let local = zone()?;
        let (ev, _, etag) = self.detail(pguid, guid)?;
        let (start, end) = event_span(&ev, &local.1)?;
        let body = json!({ "Event": {}, "Invitee": [], "Alarm": [], "ClientState": { "Collection": [{ "guid": pguid, "ctag": self.ctag(pguid)? }], "fullState": false } });
        let mut params = self.common(&local.0, start.instant().with_timezone(&local.1).date_naive(), end.instant().with_timezone(&local.1).date_naive());
        params.extend([("methodOverride", "DELETE".to_string()), ("ifMatch", etag)]);
        self.post(&format!("events/{pguid}/{guid}"), &params, &body).map(|_| ())
    }

    fn series_support(&self) -> provider::SeriesSupport {
        provider::SeriesSupport { edit: false, delete: false }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_apple_dates_in_the_event_zone() {
        let local: Tz = "UTC".parse().unwrap();
        // 09:00 in London on 12 October (BST) is 08:00 UTC; the seventh field is ignored.
        let e = json!({"startDate": [20261012, 2026, 10, 12, 9, 0, 540], "endDate": [20261012, 2026, 10, 12, 9, 30, 870], "tz": "Europe/London"});
        let (s, x) = event_span(&e, &local).unwrap();
        assert_eq!(s, Time::At(Utc.with_ymd_and_hms(2026, 10, 12, 8, 0, 0).unwrap()));
        assert_eq!(x, Time::At(Utc.with_ymd_and_hms(2026, 10, 12, 8, 30, 0).unwrap()));
        // All-day: tz null, end exclusive.
        let d = |n| NaiveDate::from_ymd_opt(2027, 7, n).unwrap();
        let e = json!({"allDay": true, "tz": null, "startDate": [20270731, 2027, 7, 31, 0, 0, 0], "endDate": [20270801, 2027, 8, 1, 0, 0, 1440]});
        assert_eq!(event_span(&e, &local).unwrap(), (Time::Date(d(31)), Time::Date(NaiveDate::from_ymd_opt(2027, 8, 1).unwrap())));
    }

    #[test]
    fn writes_spans_as_apple_does() {
        let local = ("Europe/Berlin".to_string(), "Europe/Berlin".parse::<Tz>().unwrap());
        let start = Time::At(Utc.with_ymd_and_hms(2026, 5, 30, 18, 0, 0).unwrap());
        let end = Time::At(Utc.with_ymd_and_hms(2026, 5, 30, 19, 0, 0).unwrap());
        let f = span_fields(&start, &end, &local);
        assert_eq!(f["startDate"], json!([20260530, 2026, 5, 30, 20, 0, 1200]));
        assert_eq!(f["endDate"], json!([20260530, 2026, 5, 30, 21, 0, 1260]));
        assert_eq!((f["duration"].clone(), f["tz"].clone(), f["allDay"].clone()), (json!(60), json!("Europe/Berlin"), json!(false)));
        let d = |y, m, n| Time::Date(NaiveDate::from_ymd_opt(y, m, n).unwrap());
        let f = span_fields(&d(2027, 7, 31), &d(2027, 8, 9), &local);
        assert_eq!((f["duration"].clone(), f["tz"].clone()), (json!(12960), Value::Null));
        assert_eq!(f["endDate"], json!([20270809, 2027, 8, 9, 0, 0, 0]));
    }
}
