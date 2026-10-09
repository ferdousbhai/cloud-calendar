//! iCloud Calendar over CalDAV (RFC 4791) at caldav.icloud.com, signed in with an Apple Account
//! email and an app-specific password (account.apple.com → Sign-In and Security → App-Specific
//! Passwords). The password lives in the keyring (`secret.rs`); the config holds only the email.
//!
//! Discovery: the server root names your principal (`current-user-principal`), the principal
//! names your calendar home (`calendar-home-set`, on your iCloud partition's host), and the home
//! lists your calendars. Events are read with a `calendar-query` REPORT that asks the server to
//! expand repeating events into the window's occurrences.
//!
//! IDs: a calendar is `icloud:<path>`, an event `icloud:<path of its .ics>`, an occurrence of a
//! repeating one `icloud:<path>~<UTC start>`. Advanced Data Protection doesn't cover calendars, so
//! this works with it on.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use chrono::Utc;
use std::sync::Mutex;
use std::time::Duration;

use crate::config::AccountConfig;
use crate::error::{Error, ErrorKind, Result};
use crate::ics;
use crate::provider::{self, AccountStatus, Provider};
use crate::secret;
use crate::types::*;

pub const DEFAULT_URL: &str = "https://caldav.icloud.com";
const DAV: &str = "DAV:";
const CALDAV: &str = "urn:ietf:params:xml:ns:caldav";
const APPLE: &str = "http://apple.com/ns/ical/";
const CALENDARSERVER: &str = "http://calendarserver.org/ns/";

#[derive(Debug, Clone)]
struct CalInfo {
    /// Absolute URL of the collection.
    url: String,
    path: String,
    name: String,
    color: Option<String>,
    writable: bool,
}

pub struct ICloud {
    name: String,
    base: String,
    username: Option<String>,
    agent: ureq::Agent,
    password: Mutex<Option<String>>,
    /// The calendar home's absolute URL, once found.
    home: Mutex<Option<String>>,
    calendars: Mutex<Option<Vec<CalInfo>>>,
}

struct Response {
    status: u16,
    etag: Option<String>,
    location: Option<String>,
    body: String,
}

/// `href` made absolute against `base` (an absolute URL).
pub fn resolve(base: &str, href: &str) -> String {
    if href.starts_with("http://") || href.starts_with("https://") {
        return href.to_string();
    }
    let origin_end = base.find("://").map(|i| i + 3).and_then(|i| base[i..].find('/').map(|j| i + j)).unwrap_or(base.len());
    if href.starts_with('/') {
        return format!("{}{href}", &base[..origin_end]);
    }
    let dir = match base.rfind('/') {
        Some(i) if i >= origin_end => &base[..=i],
        _ => return format!("{}/{href}", &base[..origin_end]),
    };
    format!("{dir}{href}")
}

/// The path part of an absolute URL (or the href itself when it is a path).
pub fn path_of(url: &str) -> String {
    match url.find("://") {
        Some(i) => url[i + 3..].find('/').map(|j| url[i + 3 + j..].to_string()).unwrap_or_else(|| "/".into()),
        None => url.to_string(),
    }
}

fn is(n: &roxmltree::Node, ns: &str, name: &str) -> bool {
    n.is_element() && n.tag_name().name() == name && n.tag_name().namespace() == Some(ns)
}

fn child<'a, 'i>(n: &roxmltree::Node<'a, 'i>, ns: &str, name: &str) -> Option<roxmltree::Node<'a, 'i>> {
    n.children().find(|c| is(c, ns, name))
}

/// One `<response>` of a multistatus: its href and the properties found (status 200).
#[derive(Debug, Default)]
struct DavResponse {
    href: String,
    props: Vec<(String, String, String)>,
    /// Element children of `resourcetype` and `supported-calendar-component-set` names.
    resourcetypes: Vec<String>,
    components: Vec<String>,
    href_props: Vec<(String, String)>,
}

impl DavResponse {
    fn prop(&self, ns: &str, name: &str) -> Option<&str> {
        self.props.iter().find(|(n, l, _)| n == ns && l == name).map(|(_, _, v)| v.as_str())
    }

    fn href_prop(&self, name: &str) -> Option<&str> {
        self.href_props.iter().find(|(l, _)| l == name).map(|(_, v)| v.as_str())
    }
}

fn multistatus(xml: &str) -> Result<Vec<DavResponse>> {
    let doc = roxmltree::Document::parse(xml).map_err(|e| Error::new(ErrorKind::AccountUnavailable, format!("iCloud: the CalDAV answer isn't XML ({e})")))?;
    let mut out = Vec::new();
    for resp in doc.descendants().filter(|n| is(n, DAV, "response")) {
        let mut r = DavResponse { href: child(&resp, DAV, "href").and_then(|h| h.text()).unwrap_or("").trim().to_string(), ..Default::default() };
        for ps in resp.children().filter(|n| is(n, DAV, "propstat")) {
            let ok = child(&ps, DAV, "status").and_then(|s| s.text()).is_none_or(|s| s.contains(" 200"));
            let Some(prop) = child(&ps, DAV, "prop").filter(|_| ok) else { continue };
            for p in prop.children().filter(|n| n.is_element()) {
                let (ns, name) = (p.tag_name().namespace().unwrap_or("").to_string(), p.tag_name().name().to_string());
                if let Some(h) = child(&p, DAV, "href") {
                    r.href_props.push((name.clone(), h.text().unwrap_or("").trim().to_string()));
                }
                if name == "resourcetype" {
                    r.resourcetypes = p.children().filter(|c| c.is_element()).map(|c| c.tag_name().name().to_string()).collect();
                }
                if name == "supported-calendar-component-set" {
                    r.components = p.children().filter(|c| c.is_element()).filter_map(|c| c.attribute("name")).map(str::to_ascii_uppercase).collect();
                }
                let text: String = p.descendants().filter(|d| d.is_text()).filter_map(|d| d.text()).collect();
                r.props.push((ns, name, text));
            }
        }
        out.push(r);
    }
    Ok(out)
}

fn caldav_time(t: chrono::DateTime<Utc>) -> String {
    t.format("%Y%m%dT%H%M%SZ").to_string()
}

/// `#RRGGBBAA` (Apple's) as `#RRGGBB`.
fn color(c: &str) -> Option<String> {
    let c = c.trim();
    (c.starts_with('#') && (c.len() == 7 || c.len() == 9) && c[1..].chars().all(|x| x.is_ascii_hexdigit())).then(|| c[..7].to_string())
}

/// A fresh UID: time, process and a counter, which never repeat on one machine.
fn new_uid() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    format!("{nanos:x}-{:x}-{:x}-cloud-calendar", std::process::id(), COUNTER.fetch_add(1, Ordering::Relaxed))
}

impl ICloud {
    pub fn new(name: &str, cfg: &AccountConfig) -> Self {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .allow_non_standard_methods(true)
            .max_redirects(0)
            .timeout_global(Some(Duration::from_secs(60)))
            .user_agent("cloud-calendar")
            .build()
            .into();
        let base = cfg.url.clone().filter(|u| !u.trim().is_empty()).unwrap_or_else(|| DEFAULT_URL.into()).trim_end_matches('/').to_string();
        Self { name: name.into(), base, username: cfg.username.clone().filter(|u| !u.trim().is_empty()), agent, password: Mutex::new(None), home: Mutex::new(None), calendars: Mutex::new(None) }
    }

    fn fail(&self, kind: ErrorKind, message: impl AsRef<str>) -> Error {
        Error::new(kind, format!("iCloud: {}", message.as_ref()))
    }

    fn username(&self) -> Result<&str> {
        self.username.as_deref().ok_or_else(|| self.fail(ErrorKind::AccountAuth, format!("no Apple Account email set; run `cloud-calendar account add {} --username you@icloud.com`", self.name)))
    }

    fn password(&self) -> Result<String> {
        if let Some(p) = self.password.lock().unwrap().clone() {
            return Ok(p);
        }
        let user = self.username()?;
        let p = secret::lookup(&self.name, user)?.ok_or_else(|| self.fail(ErrorKind::AccountAuth, format!("no app-specific password in the keyring; run `cloud-calendar account login {}`", self.name)))?;
        *self.password.lock().unwrap() = Some(p.clone());
        Ok(p)
    }

    /// Checks an Apple Account email and app-specific password against the server.
    pub fn verify(&self, password: &str) -> Result<usize> {
        *self.password.lock().unwrap() = Some(password.to_string());
        *self.home.lock().unwrap() = None;
        *self.calendars.lock().unwrap() = None;
        let n = self.calendar_infos().map(|c| c.len());
        if n.is_err() {
            *self.password.lock().unwrap() = None;
        }
        n
    }

    fn request(&self, method: &str, url: &str, depth: Option<&str>, body: Option<&str>, headers: &[(&str, &str)]) -> Result<Response> {
        let auth = format!("Basic {}", STANDARD.encode(format!("{}:{}", self.username()?, self.password()?)));
        let mut url = url.to_string();
        for _ in 0..4 {
            let mut b = ureq::http::Request::builder().method(method).uri(&url).header("Authorization", &auth);
            if let Some(d) = depth {
                b = b.header("Depth", d);
            }
            for (k, v) in headers {
                b = b.header(*k, *v);
            }
            let sent = match body {
                Some(text) => {
                    let ct = if method == "PUT" { "text/calendar; charset=utf-8" } else { "application/xml; charset=utf-8" };
                    b.header("Content-Type", ct).body(text.to_string()).map_err(|e| self.fail(ErrorKind::BadRequest, e.to_string())).and_then(|r| self.agent.run(r).map_err(|e| self.network(e)))
                }
                None => b.body(()).map_err(|e| self.fail(ErrorKind::BadRequest, e.to_string())).and_then(|r| self.agent.run(r).map_err(|e| self.network(e))),
            };
            let mut resp = sent?;
            let status = resp.status().as_u16();
            let header = |n: &str| resp.headers().get(n).and_then(|v| v.to_str().ok()).map(str::to_string);
            let (etag, location) = (header("etag"), header("location"));
            if matches!(status, 301 | 302 | 307 | 308)
                && let Some(loc) = &location
            {
                url = resolve(&url, loc);
                continue;
            }
            let body = resp.body_mut().with_config().limit(64 * 1024 * 1024).read_to_string().unwrap_or_default();
            if status == 401 {
                *self.password.lock().unwrap() = None;
                return Err(self.fail(ErrorKind::AccountAuth, format!("Apple refused the sign-in for {}; check the app-specific password with `cloud-calendar account login {}`", self.username.as_deref().unwrap_or(""), self.name)));
            }
            return Ok(Response { status, etag, location, body });
        }
        Err(self.fail(ErrorKind::AccountUnavailable, "too many redirects"))
    }

    fn network(&self, e: ureq::Error) -> Error {
        self.fail(ErrorKind::AccountUnavailable, format!("couldn't reach iCloud ({e})"))
    }

    fn propfind(&self, url: &str, depth: &str, props: &str) -> Result<Vec<DavResponse>> {
        let body = format!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?><d:propfind xmlns:d=\"{DAV}\" xmlns:c=\"{CALDAV}\" xmlns:a=\"{APPLE}\" xmlns:cs=\"{CALENDARSERVER}\"><d:prop>{props}</d:prop></d:propfind>"
        );
        let r = self.request("PROPFIND", url, Some(depth), Some(&body), &[])?;
        if r.status != 207 {
            return Err(self.fail(ErrorKind::AccountUnavailable, format!("PROPFIND {} answered {}", path_of(url), r.status)));
        }
        multistatus(&r.body)
    }

    /// The calendar home's absolute URL.
    fn home(&self) -> Result<String> {
        if let Some(h) = self.home.lock().unwrap().clone() {
            return Ok(h);
        }
        let root = format!("{}/", self.base);
        let principal = self
            .propfind(&root, "0", "<d:current-user-principal/>")?
            .iter()
            .find_map(|r| r.href_prop("current-user-principal").map(str::to_string))
            .filter(|h| !h.is_empty())
            .ok_or_else(|| self.fail(ErrorKind::AccountUnavailable, "the server didn't name your account (no current-user-principal)"))?;
        let principal = resolve(&root, &principal);
        let home = self
            .propfind(&principal, "0", "<c:calendar-home-set/>")?
            .iter()
            .find_map(|r| r.href_prop("calendar-home-set").map(str::to_string))
            .filter(|h| !h.is_empty())
            .ok_or_else(|| self.fail(ErrorKind::AccountUnavailable, "the server didn't name your calendars (no calendar-home-set)"))?;
        let home = resolve(&principal, &home);
        *self.home.lock().unwrap() = Some(home.clone());
        Ok(home)
    }

    fn calendar_infos(&self) -> Result<Vec<CalInfo>> {
        if let Some(c) = self.calendars.lock().unwrap().clone() {
            return Ok(c);
        }
        let home = self.home()?;
        let list = self.propfind(&home, "1", "<d:displayname/><d:resourcetype/><a:calendar-color/><c:supported-calendar-component-set/>")?;
        let mut out = Vec::new();
        for r in list {
            if !r.resourcetypes.iter().any(|t| t == "calendar") {
                continue;
            }
            if !r.components.is_empty() && !r.components.iter().any(|c| c == "VEVENT") {
                continue; // a Reminders list
            }
            let url = resolve(&home, &r.href);
            let path = path_of(&url);
            let name = r.prop(DAV, "displayname").map(str::trim).filter(|n| !n.is_empty()).unwrap_or(&path).to_string();
            let writable = !r.resourcetypes.iter().any(|t| t == "subscribed" || t == "shared");
            out.push(CalInfo { url, path, name, color: r.prop(APPLE, "calendar-color").and_then(color), writable });
        }
        *self.calendars.lock().unwrap() = Some(out.clone());
        Ok(out)
    }

    fn calendar_by_path(&self, path: &str) -> Result<CalInfo> {
        self.calendar_infos()?.into_iter().find(|c| c.path == path).ok_or_else(|| self.fail(ErrorKind::NotFound, format!("no calendar {}:{path}", self.name)))
    }

    fn calendar_events(&self, cal: &CalInfo, range: &Range) -> Result<Vec<Event>> {
        let (s, e) = (caldav_time(range.start), caldav_time(range.end));
        let body = format!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?><c:calendar-query xmlns:d=\"{DAV}\" xmlns:c=\"{CALDAV}\"><d:prop><d:getetag/><c:calendar-data><c:expand start=\"{s}\" end=\"{e}\"/></c:calendar-data></d:prop><c:filter><c:comp-filter name=\"VCALENDAR\"><c:comp-filter name=\"VEVENT\"><c:time-range start=\"{s}\" end=\"{e}\"/></c:comp-filter></c:comp-filter></c:filter></c:calendar-query>"
        );
        let r = self.request("REPORT", &cal.url, Some("1"), Some(&body), &[])?;
        if r.status != 207 {
            return Err(self.fail(ErrorKind::AccountUnavailable, format!("reading {} answered {}", cal.name, r.status)));
        }
        let mut out = Vec::new();
        for resp in multistatus(&r.body)? {
            let Some(data) = resp.prop(CALDAV, "calendar-data") else { continue };
            let path = path_of(&resolve(&cal.url, &resp.href));
            for ev in ics::events(data) {
                if ev.cancelled || !range.overlaps(&ev.start, &ev.end) {
                    continue;
                }
                let recurring = ev.rrule || ev.recurrence_id.is_some();
                let suffix = if recurring { provider::occurrence_suffix(&ev.start) } else { String::new() };
                out.push(Event {
                    id: format!("{}:{path}{suffix}", self.name),
                    account: self.name.clone(),
                    calendar_id: format!("{}:{}", self.name, cal.path),
                    calendar: cal.name.clone(),
                    color: cal.color.clone(),
                    title: if ev.summary.is_empty() { "(no title)".into() } else { ev.summary },
                    all_day: ev.start.is_date(),
                    start: ev.start,
                    end: ev.end,
                    location: ev.location,
                    notes: ev.description,
                    recurring,
                });
            }
        }
        Ok(out)
    }

    /// `(absolute URL of the .ics, is an occurrence)` from an event ID.
    fn event_url(&self, id: &str) -> Result<(String, bool)> {
        let local = provider::local_id(&self.name, "iCloud", id)?;
        let (path, occurrence) = provider::split_occurrence(local);
        if !path.starts_with('/') || !path.ends_with(".ics") {
            return Err(self.fail(ErrorKind::NotFound, format!("{id} isn't an iCloud event ID")));
        }
        Ok((resolve(&self.home()?, path), occurrence.is_some()))
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
        match self.calendar_infos() {
            Ok(c) => {
                s.ok = true;
                s.detail = format!("{} ({} calendars)", self.username.as_deref().unwrap_or(""), c.len());
            }
            Err(e) => s.detail = e.message,
        }
        s
    }

    fn calendars(&self) -> Result<Vec<Calendar>> {
        Ok(self
            .calendar_infos()?
            .into_iter()
            .map(|c| Calendar { id: format!("{}:{}", self.name, c.path), account: self.name.clone(), name: c.name, color: c.color, writable: c.writable })
            .collect())
    }

    fn events(&self, range: &Range) -> Result<Vec<Event>> {
        let cals = self.calendar_infos()?;
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
        let cal = self.calendar_by_path(provider::local_id(&self.name, "iCloud", &event.calendar_id)?)?;
        if !cal.writable {
            return Err(self.fail(ErrorKind::BadRequest, format!("{} is read-only", cal.name)));
        }
        let uid = new_uid();
        let url = format!("{}/{uid}.ics", cal.url.trim_end_matches('/'));
        let r = self.request("PUT", &url, None, Some(&ics::new_event(&uid, event, Utc::now())), &[("If-None-Match", "*")])?;
        match r.status {
            200..=299 => Ok(format!("{}:{}", self.name, path_of(&r.location.map(|l| resolve(&url, &l)).unwrap_or(url)))),
            403 => Err(self.fail(ErrorKind::BadRequest, format!("{} doesn't accept new events", cal.name))),
            s => Err(self.fail(ErrorKind::AccountUnavailable, format!("saving the event answered {s}: {}", r.body.chars().take(200).collect::<String>()))),
        }
    }

    fn update(&self, id: &str, change: &EventChange) -> Result<()> {
        let (url, occurrence) = self.event_url(id)?;
        if occurrence {
            provider::refuse_series_move("iCloud", change)?;
        }
        let current = self.request("GET", &url, None, None, &[])?;
        match current.status {
            200 => {}
            404 => return Err(self.fail(ErrorKind::NotFound, format!("no event {id} (deleted elsewhere?)"))),
            s => return Err(self.fail(ErrorKind::AccountUnavailable, format!("reading the event answered {s}"))),
        }
        let text = ics::change_event(&current.body, change, Utc::now())?;
        let mut headers = Vec::new();
        if let Some(etag) = &current.etag {
            headers.push(("If-Match", etag.as_str()));
        }
        let r = self.request("PUT", &url, None, Some(&text), &headers)?;
        match r.status {
            200..=299 => Ok(()),
            412 => Err(self.fail(ErrorKind::BadRequest, "the event changed elsewhere while it was being saved; reload and try again")),
            403 => Err(self.fail(ErrorKind::BadRequest, "this calendar doesn't allow changes")),
            s => Err(self.fail(ErrorKind::AccountUnavailable, format!("saving the event answered {s}"))),
        }
    }

    fn delete(&self, id: &str) -> Result<()> {
        let (url, _) = self.event_url(id)?;
        let r = self.request("DELETE", &url, None, None, &[])?;
        match r.status {
            200..=299 => Ok(()),
            404 => Err(self.fail(ErrorKind::NotFound, format!("no event {id} (deleted elsewhere?)"))),
            403 => Err(self.fail(ErrorKind::BadRequest, "this calendar doesn't allow deleting")),
            s => Err(self.fail(ErrorKind::AccountUnavailable, format!("deleting the event answered {s}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_hrefs() {
        assert_eq!(resolve("https://caldav.icloud.com/", "/123/principal/"), "https://caldav.icloud.com/123/principal/");
        assert_eq!(resolve("https://caldav.icloud.com/123/principal/", "https://p52-caldav.icloud.com:443/123/calendars/"), "https://p52-caldav.icloud.com:443/123/calendars/");
        assert_eq!(resolve("https://h/123/calendars/home/", "a.ics"), "https://h/123/calendars/home/a.ics");
        assert_eq!(path_of("https://p52-caldav.icloud.com:443/123/calendars/home/"), "/123/calendars/home/");
    }

    #[test]
    fn colors() {
        assert_eq!(color("#FF2968FF").as_deref(), Some("#FF2968"));
        assert_eq!(color("#1badf8").as_deref(), Some("#1badf8"));
        assert_eq!(color("red"), None);
    }

    #[test]
    fn parses_multistatus() {
        let xml = r#"<?xml version="1.0"?><multistatus xmlns="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav" xmlns:A="http://apple.com/ns/ical/">
          <response><href>/1/calendars/home/</href><propstat><prop><displayname>Home</displayname><resourcetype><collection/><C:calendar/></resourcetype><A:calendar-color>#FF2968FF</A:calendar-color><C:supported-calendar-component-set><C:comp name="VEVENT"/></C:supported-calendar-component-set></prop><status>HTTP/1.1 200 OK</status></propstat>
          <propstat><prop><getctag/></prop><status>HTTP/1.1 404 Not Found</status></propstat></response>
          <response><href>/1/principal/</href><propstat><prop><current-user-principal><href>/1/principal/</href></current-user-principal></prop><status>HTTP/1.1 200 OK</status></propstat></response>
        </multistatus>"#;
        let r = multistatus(xml).unwrap();
        assert_eq!(r[0].href, "/1/calendars/home/");
        assert_eq!(r[0].prop(DAV, "displayname"), Some("Home"));
        assert_eq!(r[0].resourcetypes, vec!["collection", "calendar"]);
        assert_eq!(r[0].components, vec!["VEVENT"]);
        assert_eq!(r[0].prop(APPLE, "calendar-color"), Some("#FF2968FF"));
        assert_eq!(r[1].href_prop("current-user-principal"), Some("/1/principal/"));
    }
}
