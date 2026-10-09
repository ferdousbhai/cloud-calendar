//! End-to-end tests: the real `cloud-calendar` binary against an in-process mock CalDAV server and
//! fake `hey`, `gws`, `secret-tool` and notification commands. Nothing touches a real account,
//! keyring or desktop: HOME and the XDG directories are a fresh temporary directory per test.

use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex};

const USER: &str = "me@icloud.com";
const PASSWORD: &str = "abcd-efgh-ijkl-mnop";

#[derive(Debug, Clone)]
struct Req {
    method: String,
    path: String,
    headers: HashMap<String, String>,
    body: String,
}

/// A CalDAV server holding one account: a principal, a calendar home with a writable calendar
/// ("Home"), a subscribed one ("Holidays") and a Reminders list, and the objects in "Home".
struct Dav {
    url: String,
    log: Arc<Mutex<Vec<Req>>>,
    objects: Arc<Mutex<HashMap<String, (u32, String)>>>,
}

const SINGLE: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:single\r\nDTSTART:20261014T090000Z\r\nDTEND:20261014T100000Z\r\nSUMMARY:Dentist\r\nLOCATION:Main St\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
const SERIES: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:series\r\nDTSTART;TZID=Europe/London:20261012T090000\r\nDTEND;TZID=Europe/London:20261012T091500\r\nRRULE:FREQ=DAILY;COUNT=2\r\nSUMMARY:Standup\r\nBEGIN:VALARM\r\nTRIGGER:-PT5M\r\nACTION:DISPLAY\r\nDESCRIPTION:x\r\nEND:VALARM\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
/// What a server's `expand` makes of SERIES.
const SERIES_EXPANDED: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:series\r\nRECURRENCE-ID:20261012T080000Z\r\nDTSTART:20261012T080000Z\r\nDTEND:20261012T081500Z\r\nSUMMARY:Standup\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nUID:series\r\nRECURRENCE-ID:20261013T080000Z\r\nDTSTART:20261013T080000Z\r\nDTEND:20261013T081500Z\r\nSUMMARY:Standup\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

fn multistatus(responses: &str) -> String {
    format!("<?xml version=\"1.0\"?><d:multistatus xmlns:d=\"DAV:\" xmlns:c=\"urn:ietf:params:xml:ns:caldav\" xmlns:a=\"http://apple.com/ns/ical/\" xmlns:cs=\"http://calendarserver.org/ns/\">{responses}</d:multistatus>")
}

fn ok_prop(href: &str, props: &str) -> String {
    format!("<d:response><d:href>{href}</d:href><d:propstat><d:prop>{props}</d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>")
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

fn dav() -> Dav {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let log = Arc::new(Mutex::new(Vec::new()));
    let objects: Arc<Mutex<HashMap<String, (u32, String)>>> = Arc::new(Mutex::new(HashMap::from([
        ("/1/calendars/home/single.ics".to_string(), (1, SINGLE.to_string())),
        ("/1/calendars/home/series.ics".to_string(), (1, SERIES.to_string())),
    ])));
    let (log2, objects2, url2) = (log.clone(), objects.clone(), url.clone());
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let (log, objects, url) = (log2.clone(), objects2.clone(), url2.clone());
            std::thread::spawn(move || {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 {
                        return;
                    }
                    let mut parts = line.split_whitespace();
                    let (method, path) = (parts.next().unwrap_or("").to_string(), parts.next().unwrap_or("").to_string());
                    let mut headers = HashMap::new();
                    loop {
                        let mut h = String::new();
                        reader.read_line(&mut h).unwrap();
                        let h = h.trim_end();
                        if h.is_empty() {
                            break;
                        }
                        if let Some((k, v)) = h.split_once(':') {
                            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
                        }
                    }
                    let len = headers.get("content-length").and_then(|l| l.parse().ok()).unwrap_or(0);
                    let mut body = vec![0; len];
                    reader.read_exact(&mut body).unwrap();
                    let req = Req { method, path, headers, body: String::from_utf8_lossy(&body).into_owned() };
                    log.lock().unwrap().push(req.clone());
                    let (status, etag, body) = handle(&req, &url, &objects);
                    let mut head = format!("HTTP/1.1 {status} X\r\nContent-Length: {}\r\nContent-Type: application/xml\r\n", body.len());
                    if let Some(e) = etag {
                        head.push_str(&format!("ETag: \"{e}\"\r\n"));
                    }
                    if stream.write_all(format!("{head}\r\n{body}").as_bytes()).is_err() {
                        return;
                    }
                }
            });
        }
    });
    Dav { url, log, objects }
}

fn handle(req: &Req, url: &str, objects: &Mutex<HashMap<String, (u32, String)>>) -> (u16, Option<u32>, String) {
    use base64::Engine;
    let expected = format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(format!("{USER}:{PASSWORD}")));
    if req.headers.get("authorization") != Some(&expected) {
        return (401, None, String::new());
    }
    let mut objects = objects.lock().unwrap();
    match (req.method.as_str(), req.path.as_str()) {
        ("PROPFIND", "/") => (207, None, multistatus(&ok_prop("/", "<d:current-user-principal><d:href>/1/principal/</d:href></d:current-user-principal>"))),
        // The home lives on another "partition" host, named absolutely, as iCloud does.
        ("PROPFIND", "/1/principal/") => (207, None, multistatus(&ok_prop("/1/principal/", &format!("<c:calendar-home-set><d:href>{url}/1/calendars/</d:href></c:calendar-home-set>")))),
        ("PROPFIND", "/1/calendars/") => {
            let cal = |href: &str, name: &str, extra: &str, comp: &str| {
                ok_prop(href, &format!("<d:displayname>{name}</d:displayname><d:resourcetype><d:collection/><c:calendar/>{extra}</d:resourcetype><a:calendar-color>#FF2968FF</a:calendar-color><c:supported-calendar-component-set><c:comp name=\"{comp}\"/></c:supported-calendar-component-set>"))
            };
            let body = [
                ok_prop("/1/calendars/", "<d:resourcetype><d:collection/></d:resourcetype>"),
                cal("/1/calendars/home/", "Home", "", "VEVENT"),
                cal("/1/calendars/holidays/", "Holidays", "<cs:subscribed/>", "VEVENT"),
                cal("/1/calendars/tasks/", "Reminders", "", "VTODO"),
                ok_prop("/1/calendars/inbox/", "<d:resourcetype><d:collection/><c:schedule-inbox/></d:resourcetype>"),
            ]
            .concat();
            (207, None, multistatus(&body))
        }
        ("REPORT", "/1/calendars/home/") => {
            assert!(req.body.contains("<c:expand ") && req.body.contains("time-range"), "asks for expansion in a window");
            let mut body = String::new();
            for (path, (etag, data)) in objects.iter() {
                let data = if data.contains("RRULE") { SERIES_EXPANDED } else { data.as_str() };
                body.push_str(&ok_prop(path, &format!("<d:getetag>\"{etag}\"</d:getetag><c:calendar-data>{}</c:calendar-data>", escape(data))));
            }
            (207, None, multistatus(&body))
        }
        ("REPORT", _) => (207, None, multistatus("")),
        ("GET", p) => match objects.get(p) {
            Some((etag, data)) => (200, Some(*etag), data.clone()),
            None => (404, None, String::new()),
        },
        ("PUT", p) => {
            let current = objects.get(p).cloned();
            if req.headers.get("if-none-match").is_some_and(|v| v == "*") && current.is_some() {
                return (412, None, String::new());
            }
            if let Some(m) = req.headers.get("if-match")
                && current.as_ref().is_none_or(|(e, _)| format!("\"{e}\"") != *m)
            {
                return (412, None, String::new());
            }
            let etag = current.map(|(e, _)| e + 1).unwrap_or(1);
            objects.insert(p.to_string(), (etag, req.body.clone()));
            (201, Some(etag), String::new())
        }
        ("DELETE", p) => match objects.remove(p) {
            Some(_) => (204, None, String::new()),
            None => (404, None, String::new()),
        },
        _ => (405, None, String::new()),
    }
}

/// A fresh home for one test: config, state and fakes' logs all live in it.
struct Home {
    dir: PathBuf,
}

impl Home {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("cloud-calendar-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("config/cloud-calendar")).unwrap();
        Self { dir }
    }

    fn config(&self, toml: &str) {
        std::fs::write(self.dir.join("config/cloud-calendar/config.toml"), toml).unwrap();
    }

    fn file(&self, name: &str) -> String {
        std::fs::read_to_string(self.dir.join(name)).unwrap_or_default()
    }

    fn run(&self, args: &[&str], stdin: Option<&str>, env: &[(&str, &str)]) -> Output {
        let tests = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_cloud-calendar"));
        cmd.args(args)
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("HOME", &self.dir)
            .env("XDG_CONFIG_HOME", self.dir.join("config"))
            .env("XDG_STATE_HOME", self.dir.join("state"))
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .env("CLOUD_CALENDAR_SECRET_TOOL", tests.join("fake-secret-tool"))
            .env("FAKE_SECRET_DIR", self.dir.join("secrets"))
            .env("CLOUD_CALENDAR_HEY_COMMAND", tests.join("fake-hey"))
            .env("FAKE_HEY_LOG", self.dir.join("hey.log"))
            .env("CLOUD_CALENDAR_GWS_COMMAND", tests.join("fake-gws"))
            .env("FAKE_GWS_LOG", self.dir.join("gws.log"))
            .env("CLOUD_CALENDAR_NOTIFY_COMMAND", tests.join("fake-notify"))
            .env("FAKE_NOTIFY_LOG", self.dir.join("notify.log"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (k, v) in env {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn().unwrap();
        let mut pipe = child.stdin.take().unwrap();
        pipe.write_all(stdin.unwrap_or("").as_bytes()).unwrap();
        drop(pipe);
        child.wait_with_output().unwrap()
    }

    /// Runs with `--json` and returns the envelope and exit code.
    fn json(&self, args: &[&str], env: &[(&str, &str)]) -> (Value, i32) {
        let mut all = vec!["--json"];
        all.extend(args);
        let out = self.run(&all, None, env);
        let v = serde_json::from_slice(&out.stdout).unwrap_or_else(|_| panic!("not JSON: {}\nstderr: {}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)));
        (v, out.status.code().unwrap_or(-1))
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn ids(v: &Value) -> Vec<String> {
    v["data"].as_array().unwrap().iter().map(|e| e["id"].as_str().unwrap().to_string()).collect()
}

#[test]
fn no_accounts_says_how_to_add_one() {
    let home = Home::new("empty");
    let (v, code) = home.json(&["agenda"], &[]);
    assert_eq!(code, 3);
    assert_eq!(v["error"]["code"], "not_configured");
    assert!(v["error"]["message"].as_str().unwrap().contains("account add"));
}

#[test]
fn icloud_sign_in_checks_the_password_and_keeps_it_in_the_keyring() {
    let server = dav();
    let home = Home::new("icloud-add");
    let add = |pw: &str| home.run(&["--json", "account", "add", "icloud", "--username", USER, "--url", &server.url, "--password-stdin"], Some(pw), &[]);
    let wrong = add("wrong");
    assert_eq!(wrong.status.code(), Some(3), "{}", String::from_utf8_lossy(&wrong.stdout));
    assert!(home.file("config/cloud-calendar/config.toml").is_empty(), "nothing saved after a refused password");
    let right = add(&format!("{PASSWORD}\n"));
    assert_eq!(right.status.code(), Some(0), "{}", String::from_utf8_lossy(&right.stderr));
    let config = home.file("config/cloud-calendar/config.toml");
    assert!(config.contains(USER) && !config.contains(PASSWORD), "the password stays out of the config: {config}");
    let secrets: Vec<_> = std::fs::read_dir(home.dir.join("secrets")).unwrap().collect();
    assert_eq!(secrets.len(), 1);

    let (v, code) = home.json(&["calendars"], &[]);
    assert_eq!(code, 0, "{v}");
    let cals = v["data"].as_array().unwrap();
    let names: Vec<_> = cals.iter().map(|c| (c["name"].as_str().unwrap(), c["writable"].as_bool().unwrap())).collect();
    assert_eq!(names, vec![("Holidays", false), ("Home", true)], "the Reminders list and inbox aren't calendars");
    assert_eq!(cals[1]["id"], "icloud:/1/calendars/home/");
    assert_eq!(cals[1]["color"], "#FF2968");

    let (v, code) = home.json(&["account", "remove", "icloud", "--yes"], &[]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(std::fs::read_dir(home.dir.join("secrets")).unwrap().count(), 0, "the keyring entry is cleared");
}

fn icloud_home(name: &str, server: &Dav) -> Home {
    let home = Home::new(name);
    home.config(&format!("[accounts.icloud]\nusername = \"{USER}\"\nurl = \"{}\"\n", server.url));
    let out = home.run(&["account", "login", "icloud", "--password-stdin"], Some(PASSWORD), &[]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    home
}

#[test]
fn icloud_agenda_add_edit_delete() {
    let server = dav();
    let home = icloud_home("icloud-flow", &server);

    let (v, code) = home.json(&["agenda", "--from", "2026-10-12", "--days", "7"], &[]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(
        ids(&v),
        vec!["icloud:/1/calendars/home/series.ics~20261012T080000Z", "icloud:/1/calendars/home/series.ics~20261013T080000Z", "icloud:/1/calendars/home/single.ics"]
    );
    let first = &v["data"][0];
    assert_eq!((first["title"].as_str(), first["recurring"].as_bool(), first["calendar"].as_str()), (Some("Standup"), Some(true), Some("Home")));
    assert_eq!(first["start"], "2026-10-12T08:00:00+00:00");
    assert_eq!(v["data"][2]["location"], "Main St");

    // Add a timed event and an all-day one.
    let (v, code) = home.json(&["event", "add", "--calendar", "icloud:/1/calendars/home/", "--title", "Lunch; with Sam", "--start", "2026-10-15 12:30", "--length", "45m", "--location", "Café"], &[]);
    assert_eq!(code, 0, "{v}");
    let new_id = v["data"]["id"].as_str().unwrap().to_string();
    assert!(new_id.starts_with("icloud:/1/calendars/home/") && new_id.ends_with(".ics"), "{new_id}");
    let put = server.log.lock().unwrap().iter().rev().find(|r| r.method == "PUT").cloned().unwrap();
    assert_eq!(put.headers.get("if-none-match").map(String::as_str), Some("*"));
    assert!(put.body.contains(r"SUMMARY:Lunch\; with Sam") && put.body.contains("DTSTART:20261015T123000Z") && put.body.contains("DTEND:20261015T131500Z"), "{}", put.body);
    let (v, _) = home.json(&["event", "add", "--calendar", "icloud:/1/calendars/home/", "--title", "Off", "--start", "2026-10-16"], &[]);
    let off = v["data"]["id"].as_str().unwrap().to_string();
    let (v, _) = home.json(&["agenda", "--from", "2026-10-12", "--days", "7"], &[]);
    let titles: Vec<_> = v["data"].as_array().unwrap().iter().map(|e| (e["title"].as_str().unwrap().to_string(), e["all_day"].as_bool().unwrap())).collect();
    assert!(titles.contains(&("Lunch; with Sam".into(), false)) && titles.contains(&("Off".into(), true)), "{titles:?}");

    // Read-only calendars refuse new events.
    let (v, code) = home.json(&["event", "add", "--calendar", "icloud:/1/calendars/holidays/", "--title", "x", "--start", "2026-10-15"], &[]);
    assert_eq!((code, v["error"]["code"].as_str()), (2, Some("bad_request")));

    // An occurrence's title changes the series, keeping its rule and alarm; moving it is refused.
    let occurrence = "icloud:/1/calendars/home/series.ics~20261013T080000Z";
    let (v, code) = home.json(&["event", "edit", occurrence, "--title", "Daily standup"], &[]);
    assert_eq!(code, 0, "{v}");
    let (_, stored) = server.objects.lock().unwrap()["/1/calendars/home/series.ics"].clone();
    assert!(stored.contains("SUMMARY:Daily standup") && stored.contains("RRULE:FREQ=DAILY;COUNT=2") && stored.contains("TRIGGER:-PT5M"), "{stored}");
    let put = server.log.lock().unwrap().iter().rev().find(|r| r.method == "PUT").cloned().unwrap();
    assert_eq!(put.headers.get("if-match").map(String::as_str), Some("\"1\""));
    let (v, code) = home.json(&["event", "edit", occurrence, "--start", "2026-10-13 10:00"], &[]);
    assert_eq!(code, 2, "{v}");
    assert!(v["error"]["message"].as_str().unwrap().contains("whole series"));

    // Moving a single event keeps its length.
    let (v, code) = home.json(&["event", "edit", "icloud:/1/calendars/home/single.ics", "--start", "2026-10-14 15:00"], &[]);
    assert_eq!(code, 0, "{v}");
    let (_, stored) = server.objects.lock().unwrap()["/1/calendars/home/single.ics"].clone();
    assert!(stored.contains("DTSTART:20261014T150000Z") && stored.contains("DTEND:20261014T160000Z"), "{stored}");

    // Deleting asks for --yes, and says when it reaches a whole series.
    let (v, code) = home.json(&["event", "delete", &off], &[]);
    assert_eq!(code, 2, "{v}");
    let (v, code) = home.json(&["event", "delete", &off, "--yes"], &[]);
    assert_eq!(code, 0, "{v}");
    let (v, code) = home.json(&["event", "delete", &off, "--yes"], &[]);
    assert_eq!((code, v["error"]["code"].as_str()), (4, Some("not_found")));
    let (v, code) = home.json(&["event", "delete", occurrence, "--yes"], &[]);
    assert_eq!((code, v["data"]["series"].as_bool()), (0, Some(true)));
    assert!(!server.objects.lock().unwrap().contains_key("/1/calendars/home/series.ics"));
}

#[test]
fn hey_and_google_merge_into_one_agenda() {
    let home = Home::new("merge");
    home.config("[accounts.hey]\n\n[accounts.google]\nclient_id = \"x\"\nclient_secret = \"y\"\n");
    std::fs::create_dir_all(home.dir.join("config/cloud-calendar/gws/google")).unwrap();
    std::fs::write(home.dir.join("config/cloud-calendar/gws/google/credentials.enc"), "x").unwrap();

    let (v, code) = home.json(&["agenda", "--from", "2026-10-12", "--days", "7"], &[]);
    assert_eq!(code, 0, "{v}");
    let got: Vec<_> = v["data"].as_array().unwrap().iter().map(|e| (e["id"].as_str().unwrap(), e["title"].as_str().unwrap())).collect();
    assert_eq!(
        got,
        vec![
            ("google:en.usa#holiday@group.v.calendar.google.com/hol1", "Columbus Day"),
            ("google:me@gmail.com/s1_20261012T080000Z~20261012T080000Z", "Standup"),
            ("hey:500~20261013T060000Z", "Gym"),
            ("google:me@gmail.com/g1", "Design review"),
            ("hey:401", "Lunch with Sam"),
            ("hey:402", "Trip"),
        ]
    );
    let trip = v["data"].as_array().unwrap().iter().find(|e| e["title"] == "Trip").unwrap();
    assert_eq!((trip["start"].as_str(), trip["end"].as_str(), trip["all_day"].as_bool()), (Some("2026-10-15"), Some("2026-10-16"), Some(true)));
    assert!(!v.to_string().contains("Should not show"), "calendars hidden in Google aren't read");
    assert!(home.file("gws.log").contains("\"singleEvents\":true"));

    let (v, _) = home.json(&["calendars"], &[]);
    let hey_cals: Vec<_> = v["data"].as_array().unwrap().iter().filter(|c| c["account"] == "hey").map(|c| (c["id"].as_str().unwrap(), c["writable"].as_bool().unwrap())).collect();
    assert_eq!(hey_cals, vec![("hey:11", true), ("hey:12", false)]);

    // A signed-out account is a warning; the others still answer.
    let (v, code) = home.json(&["agenda", "--from", "2026-10-12", "--days", "7"], &[("FAKE_HEY_MODE", "logged_out")]);
    assert_eq!(code, 0);
    assert_eq!(v["meta"]["warnings"][0]["account"], "hey");
    assert_eq!(v["meta"]["warnings"][0]["code"], "account_unauthorized");
    assert!(ids(&v).iter().all(|i| i.starts_with("google:")) && !ids(&v).is_empty());
    let (v, _) = home.json(&["today"], &[("FAKE_GWS_MODE", "expired"), ("FAKE_HEY_MODE", "crash")]);
    let codes: Vec<_> = v["meta"]["warnings"].as_array().unwrap().iter().map(|w| w["code"].as_str().unwrap()).collect();
    assert_eq!(codes.len(), 2);
    assert!(codes.contains(&"account_unauthorized") && codes.contains(&"account_unavailable"), "{codes:?}");
}

#[test]
fn hey_event_changes_use_the_hey_cli() {
    let home = Home::new("hey-write");
    home.config("[accounts.hey]\n");
    let (v, code) = home.json(&["event", "add", "--calendar", "hey:11", "--title", "Dinner", "--start", "2026-10-15 19:00", "--end", "2026-10-15 21:00", "--notes", "bring wine"], &[]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["data"]["id"], "hey:777");
    let (v, code) = home.json(&["event", "add", "--calendar", "hey:11", "--title", "Away", "--start", "2026-10-20", "--end", "2026-10-23"], &[]);
    assert_eq!(code, 0, "{v}");
    let (v, code) = home.json(&["event", "edit", "hey:500~20261013T060000Z", "--title", "Gym!"], &[]);
    assert_eq!(code, 0, "{v}");
    let (v, code) = home.json(&["event", "edit", "hey:401", "--start", "2026-10-14 13:00"], &[]);
    assert_eq!(code, 2, "a HEY move names both ends: {v}");
    let (v, code) = home.json(&["event", "edit", "hey:401", "--start", "2026-10-14 13:00", "--end", "2026-10-14 14:30", "--location", ""], &[]);
    assert_eq!(code, 0, "{v}");
    let (v, code) = home.json(&["event", "delete", "hey:500~20261013T060000Z", "--yes"], &[]);
    assert_eq!(code, 0, "{v}");
    let (v, code) = home.json(&["event", "delete", "hey:404", "--yes"], &[]);
    assert_eq!((code, v["error"]["code"].as_str()), (4, Some("not_found")));
    let log = home.file("hey.log");
    let lines: Vec<&str> = log.lines().collect();
    assert_eq!(
        lines,
        vec![
            "event\tadd\tDinner\t--calendar\t11\t--starts-on\t2026-10-15\t--start-time\t19:00\t--ends-on\t2026-10-15\t--end-time\t21:00\t--notes\tbring wine",
            "event\tadd\tAway\t--calendar\t11\t--all-day\t--starts-on\t2026-10-20\t--ends-on\t2026-10-22",
            "event\tedit\t500\t--title\tGym!",
            "event\tedit\t401\t--location\t\t--all-day=false\t--starts-on\t2026-10-14\t--start-time\t13:00\t--ends-on\t2026-10-14\t--end-time\t14:30",
            "event\tdelete\t500",
            "event\tdelete\t404",
        ]
    );
}

#[test]
fn google_event_changes_go_to_the_series() {
    let home = Home::new("google-write");
    home.config("[accounts.google]\n");
    std::fs::create_dir_all(home.dir.join("config/cloud-calendar/gws/google")).unwrap();
    std::fs::write(home.dir.join("config/cloud-calendar/gws/google/credentials.enc"), "x").unwrap();
    let (v, code) = home.json(&["event", "add", "--calendar", "google:me@gmail.com", "--title", "Call", "--start", "2026-10-15 09:00"], &[]);
    assert_eq!((code, v["data"]["id"].as_str()), (0, Some("google:me@gmail.com/new1")), "{v}");
    let (v, code) = home.json(&["event", "edit", "google:me@gmail.com/s1_20261012T080000Z~20261012T080000Z", "--location", "Room 2"], &[]);
    assert_eq!(code, 0, "{v}");
    let (v, code) = home.json(&["event", "edit", "google:me@gmail.com/g1", "--start", "2026-10-13 16:00"], &[]);
    assert_eq!(code, 0, "{v}");
    let (v, code) = home.json(&["event", "delete", "google:me@gmail.com/s1_20261012T080000Z~20261012T080000Z", "--yes"], &[]);
    assert_eq!(code, 0, "{v}");
    let log = home.file("gws.log");
    let calls: Vec<Value> = log
        .lines()
        .map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            let params: Value = serde_json::from_str(f[4]).unwrap();
            let body = f.get(6).map(|b| serde_json::from_str(b).unwrap()).unwrap_or(Value::Null);
            json!({ "call": format!("{} {}", f[1], f[2]), "params": params, "body": body })
        })
        .collect();
    assert_eq!(calls[0]["call"], "events insert");
    assert_eq!(calls[0]["body"]["start"]["dateTime"], "2026-10-15T09:00:00Z");
    assert_eq!(calls[0]["body"]["end"]["dateTime"], "2026-10-15T10:00:00Z");
    assert_eq!((calls[2]["call"].as_str(), calls[2]["params"]["eventId"].as_str()), (Some("events patch"), Some("s1")), "an occurrence's edit goes to its series");
    assert_eq!(calls[2]["body"], json!({ "location": "Room 2" }));
    let patch = calls.iter().rev().find(|c| c["call"] == "events patch").unwrap();
    assert_eq!(patch["body"]["end"]["dateTime"], "2026-10-13T17:00:00Z", "a new start keeps the length");
    let last = calls.last().unwrap();
    assert_eq!((last["call"].as_str(), last["params"]["eventId"].as_str()), (Some("events delete"), Some("s1")));

    // Signed out: the error says how to sign in.
    std::fs::remove_file(home.dir.join("config/cloud-calendar/gws/google/credentials.enc")).unwrap();
    let (v, code) = home.json(&["calendars"], &[]);
    assert_eq!(code, 0);
    assert!(v["meta"]["warnings"][0]["message"].as_str().unwrap().contains("account login google"));
}

#[test]
fn notifications_go_out_once() {
    let home = Home::new("notify");
    home.config("notify_minutes = [10]\n\n[accounts.hey]\n");
    let soon = [("FAKE_HEY_SOON", "1")];
    let (v, code) = home.json(&["notify"], &soon);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["data"]["sent"][0]["title"], "Dentist");
    let log = home.file("notify.log");
    let fields: Vec<&str> = log.lines().next().unwrap().split('\t').collect();
    assert_eq!(&fields[..6], &["--app-name", "Cloud Calendar", "-g", "󰃭", "-u", "normal"]);
    assert_eq!(fields[6], "Dentist");
    assert!(fields[7].starts_with("in 5 min · ") && fields[7].contains("Main St"), "{}", fields[7]);
    assert_eq!(&fields[8..], &["--exec", "cloud-calendar-gtk"]);
    // Again a minute later: from the cache, and nothing twice.
    let weeks = |h: &Home| h.file("hey.log").lines().filter(|l| l.starts_with("event\tweek")).count();
    let read = weeks(&home);
    let (v, _) = home.json(&["notify"], &soon);
    assert_eq!((v["data"]["sent"].as_array().unwrap().len(), v["data"]["refreshed"].as_bool()), (0, Some(false)));
    assert_eq!(home.file("notify.log").lines().count(), 1);
    assert_eq!(weeks(&home), read, "the cache spared a second read");
}

#[test]
fn human_output_groups_by_day() {
    let home = Home::new("human");
    home.config("[accounts.hey]\n");
    let out = home.run(&["--styled", "agenda", "--from", "2026-10-12", "--days", "7"], None, &[]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("Tue 13 Oct\n  06:00–07:00 Gym ↻  · Personal (hey)\n    hey:500~20261013T060000Z"), "{text}");
    assert!(text.contains("Thu 15 Oct\n  all day     Trip"), "{text}");
    let out = home.run(&["--ids-only", "agenda", "--from", "2026-10-12", "--days", "7"], None, &[]);
    assert_eq!(String::from_utf8_lossy(&out.stdout).lines().count(), 3);
}
