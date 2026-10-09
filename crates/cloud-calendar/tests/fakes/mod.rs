//! What the CLI meets in the tests instead of this machine's: a private dbus-daemon carrying a
//! fake icloud-session (`io.github.ferdousbhai.ICloudSession`, as icloud-sessiond serves it), and
//! a fake icloud.com calendar web service.

use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use zbus::object_server::SignalEmitter;

// ------------------------------------------------------------------ bus

pub struct Bus {
    pub address: String,
    daemon: Child,
}

impl Bus {
    /// A dbus-daemon of its own, with no service directories: nothing installed is activated.
    pub fn start(dir: &Path) -> Bus {
        std::fs::create_dir_all(dir).unwrap();
        let conf = dir.join("bus.conf");
        std::fs::write(
            &conf,
            format!(
                "<!DOCTYPE busconfig PUBLIC \"-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN\" \"http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd\">\
                 <busconfig><type>session</type><listen>unix:dir={}</listen><auth>EXTERNAL</auth>\
                 <policy context=\"default\"><allow send_destination=\"*\" eavesdrop=\"true\"/><allow eavesdrop=\"true\"/><allow own=\"*\"/></policy></busconfig>",
                dir.display()
            ),
        )
        .unwrap();
        let mut daemon = Command::new("dbus-daemon")
            .arg(format!("--config-file={}", conf.display()))
            .args(["--nofork", "--print-address=1"])
            .stdout(Stdio::piped())
            .spawn()
            .expect("dbus-daemon (the dbus package) runs the tests' private bus");
        let mut line = String::new();
        BufReader::new(daemon.stdout.take().unwrap()).read_line(&mut line).unwrap();
        Bus { address: line.trim().to_string(), daemon }
    }
}

impl Drop for Bus {
    fn drop(&mut self) {
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
    }
}

// ------------------------------------------------------- icloud-session

#[derive(Default)]
pub struct SessionState {
    pub signed_in: bool,
    pub signing_in: bool,
    pub sign_ins: usize,
    pub calendar_url: String,
    /// Signed in, but the daemon can't read its cookie jar from the keyring.
    pub keyring_unavailable: bool,
}

#[derive(Debug, zbus::DBusError)]
#[zbus(prefix = "io.github.ferdousbhai.ICloudSession.Error")]
enum SessionError {
    #[zbus(error)]
    ZBus(zbus::Error),
    SignInRequired(String),
    KeyringUnavailable(String),
}

struct FakeSession(Arc<Mutex<SessionState>>);

/// `Session()`'s reply: cookie header, client params, webservices.
type SessionReply = (String, HashMap<String, String>, HashMap<String, String>);

#[zbus::interface(name = "io.github.ferdousbhai.ICloudSession")]
impl FakeSession {
    #[zbus(property, name = "SignedIn")]
    fn signed_in(&self) -> bool {
        self.0.lock().unwrap().signed_in
    }
    #[zbus(property, name = "AppleId")]
    fn apple_id(&self) -> String {
        if self.0.lock().unwrap().signed_in { "me@icloud.com".into() } else { String::new() }
    }
    #[zbus(property, name = "FullName")]
    fn full_name(&self) -> String {
        if self.0.lock().unwrap().signed_in { "Ada Lovelace".into() } else { String::new() }
    }
    #[zbus(property, name = "Dsid")]
    fn dsid(&self) -> String {
        if self.0.lock().unwrap().signed_in { "123".into() } else { String::new() }
    }
    #[zbus(property, name = "ExpiresAt")]
    fn expires_at(&self) -> u64 {
        0
    }
    #[zbus(property, name = "SigningIn")]
    fn signing_in(&self) -> bool {
        self.0.lock().unwrap().signing_in
    }
    #[zbus(property, name = "FindMyAuthorized")]
    fn find_my_authorized(&self) -> bool {
        false
    }
    #[zbus(property, name = "FindMyPasswordStored")]
    fn find_my_password_stored(&self) -> bool {
        false
    }

    #[zbus(name = "Session", out_args("cookie_header", "client_params", "webservices"))]
    fn session(&self) -> Result<SessionReply, SessionError> {
        let st = self.0.lock().unwrap();
        if !st.signed_in {
            return Err(SessionError::SignInRequired("sign in to iCloud required".into()));
        }
        if st.keyring_unavailable {
            return Err(SessionError::KeyringUnavailable("the keyring is locked".into()));
        }
        let params = HashMap::from([("clientBuildNumber".into(), "2618Build21".into()), ("clientMasteringNumber".into(), "2618Build21".into()), ("clientId".into(), "client-1".into())]);
        Ok(("X-APPLE-WEBAUTH-TOKEN=t0k3n".into(), params, HashMap::from([("calendar".into(), st.calendar_url.clone())])))
    }

    #[zbus(name = "MergeCookies")]
    fn merge_cookies(&self, _set_cookies: Vec<String>) {}

    #[zbus(name = "ReportSignInRequired", out_args("still_signed_in"))]
    fn report_sign_in_required(&self) -> bool {
        self.0.lock().unwrap().signed_in
    }

    /// Opens "the window", and the user signs in.
    #[zbus(name = "SignIn")]
    async fn sign_in(&self, #[zbus(signal_emitter)] emitter: SignalEmitter<'_>) {
        {
            let mut st = self.0.lock().unwrap();
            st.sign_ins += 1;
            st.signing_in = true;
        }
        let _ = self.signing_in_changed(&emitter).await;
        {
            let mut st = self.0.lock().unwrap();
            st.signing_in = false;
            st.signed_in = true;
        }
        let _ = self.signed_in_changed(&emitter).await;
        let _ = self.signing_in_changed(&emitter).await;
    }
}

/// The fake services on the bus, and what they hold.
pub struct Services {
    pub session: Arc<Mutex<SessionState>>,
    _conn: Option<zbus::blocking::Connection>,
}

/// The fake icloud-session on the bus, or none (`installed` false: as if it isn't installed).
pub fn serve(bus: &Bus, installed: bool, signed_in: bool, calendar_url: &str) -> Services {
    let session = Arc::new(Mutex::new(SessionState { signed_in, calendar_url: calendar_url.into(), ..Default::default() }));
    if !installed {
        return Services { session, _conn: None };
    }
    let icloud = zbus::blocking::connection::Builder::address(bus.address.as_str())
        .unwrap()
        .name("io.github.ferdousbhai.ICloudSession")
        .unwrap()
        .serve_at("/io/github/ferdousbhai/ICloudSession", FakeSession(session.clone()))
        .unwrap()
        .build()
        .unwrap();
    Services { session, _conn: Some(icloud) }
}

// ------------------------------------------------- icloud.com calendar

#[derive(Debug, Clone)]
pub struct Req {
    pub method: String,
    pub path: String,
    pub query: HashMap<String, String>,
    pub cookie: String,
    pub body: Value,
}

/// The calendar web service: two calendars ("Home", writable; "Birthdays", read-only) and the
/// events in them, kept as `/ca/eventdetail` would hand them out.
pub struct Web {
    pub url: String,
    pub log: Arc<Mutex<Vec<Req>>>,
    pub events: Arc<Mutex<HashMap<String, Value>>>,
}

fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => {
                out.push(u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap(), 16).unwrap());
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8(out).unwrap()
}

pub fn seed_events() -> HashMap<String, Value> {
    let ev = |guid: &str, title: &str, start: Value, end: Value, extra: Value| {
        let mut e = json!({"guid": guid, "pGuid": "home", "title": title, "startDate": start, "endDate": end, "etag": format!("C={guid}@U=1"), "allDay": false, "tz": "Europe/London"});
        e.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        e
    };
    HashMap::from([
        ("dentist".into(), ev("dentist", "Dentist", json!([20261014, 2026, 10, 14, 10, 0, 600]), json!([20261014, 2026, 10, 14, 11, 0, 780]), json!({"location": "Main St", "description": "bring forms"}))),
        ("standup".into(), ev("standup", "Standup", json!([20261013, 2026, 10, 13, 9, 0, 540]), json!([20261013, 2026, 10, 13, 9, 15, 885]), json!({"recurrence": "standup*MME-RID"}))),
        ("trip".into(), ev("trip", "Trip", json!([20261016, 2026, 10, 16, 0, 0, 0]), json!([20261018, 2026, 10, 18, 0, 0, 1440]), json!({"allDay": true, "tz": null}))),
    ])
}

pub fn web() -> Web {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let log = Arc::new(Mutex::new(Vec::new()));
    let events = Arc::new(Mutex::new(seed_events()));
    let (log2, events2) = (log.clone(), events.clone());
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let (log, events) = (log2.clone(), events2.clone());
            std::thread::spawn(move || {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 {
                        return;
                    }
                    let mut parts = line.split_whitespace();
                    let (method, target) = (parts.next().unwrap_or("").to_string(), parts.next().unwrap_or("").to_string());
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
                    let (path, q) = target.split_once('?').unwrap_or((&target, ""));
                    let query = q.split('&').filter_map(|kv| kv.split_once('=')).map(|(k, v)| (k.to_string(), decode(v))).collect();
                    let req = Req { method, path: path.to_string(), query, cookie: headers.get("cookie").cloned().unwrap_or_default(), body: serde_json::from_slice(&body).unwrap_or(Value::Null) };
                    log.lock().unwrap().push(req.clone());
                    let (status, reply) = handle(&req, &events);
                    let text = reply.to_string();
                    if stream.write_all(format!("HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{text}", text.len()).as_bytes()).is_err() {
                        return;
                    }
                }
            });
        }
    });
    Web { url, log, events }
}

fn handle(req: &Req, events: &Mutex<HashMap<String, Value>>) -> (u16, Value) {
    if !req.cookie.contains("X-APPLE-WEBAUTH-TOKEN=t0k3n") || req.query.get("dsid").map(String::as_str) != Some("123") || req.query.get("clientId").map(String::as_str) != Some("client-1") {
        return (421, json!({}));
    }
    let mut events = events.lock().unwrap();
    let parts: Vec<&str> = req.path.trim_start_matches("/ca/").split('/').collect();
    match (req.method.as_str(), parts.as_slice()) {
        ("GET", ["allcollections"]) => (
            200,
            json!({"Collection": [
                {"guid": "home", "title": "Home", "color": "#FF2968", "readOnly": false, "ctag": "FT=-@RU=home@S=7"},
                {"guid": "birthdays", "title": "Birthdays", "color": "#8E8E93", "readOnly": true, "ctag": "FT=-@RU=b@S=1"},
            ]}),
        ),
        ("GET", ["events"]) => {
            let mut list: Vec<Value> = events.values().cloned().collect();
            list.sort_by_key(|e| e["guid"].as_str().unwrap().to_string());
            (200, json!({"Event": list}))
        }
        ("GET", ["eventdetail", _, guid]) => match events.get(*guid) {
            Some(e) => (200, json!({"Event": [e], "Alarm": [{"guid": format!("{guid}:a1"), "pGuid": guid}, {"guid": "other:a1", "pGuid": "other"}]})),
            None => (404, json!({})),
        },
        ("POST", ["events", _, guid]) => {
            let current = events.get(*guid).cloned();
            match req.query.get("methodOverride").map(String::as_str) {
                None => {
                    if current.is_some() {
                        return (409, json!({}));
                    }
                    let mut e = req.body["Event"].clone();
                    e["etag"] = json!(format!("C={guid}@U=1"));
                    events.insert(guid.to_string(), e);
                    (200, json!({"Event": [], "ChangeSet": {}}))
                }
                Some(m @ ("PUT" | "DELETE")) => {
                    let Some(cur) = current else { return (404, json!({})) };
                    if req.query.get("ifMatch") != cur["etag"].as_str().map(str::to_string).as_ref() {
                        return (412, json!({}));
                    }
                    if m == "DELETE" {
                        events.remove(*guid);
                    } else {
                        let mut e = req.body["Event"].clone();
                        e["etag"] = json!(format!("C={guid}@U=2"));
                        events.insert(guid.to_string(), e);
                    }
                    (200, json!({}))
                }
                Some(_) => (400, json!({})),
            }
        }
        _ => (404, json!({})),
    }
}
