//! End-to-end tests: the real `cloud-calendar` binary on a private D-Bus with a fake
//! icloud-session, a fake icloud.com calendar service, and fake `hey`, `gws`
//! and notification commands. HOME and the XDG directories are a fresh directory per test, the
//! locale is C: nothing touches a real account, keyring, bus or desktop.

mod fakes;

use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

/// The Google OAuth client the tests use in place of the built-in one.
const GOOGLE_SECRET: &str = "GOCSPX-test-secret";

struct Home {
    dir: PathBuf,
    bus: fakes::Bus,
    web: fakes::Web,
    services: fakes::Services,
}

impl Home {
    /// A fresh home; icloud-session starts signed in or out.
    fn new(name: &str, icloud_signed_in: bool) -> Self {
        Self::with(name, true, icloud_signed_in)
    }

    fn with(name: &str, icloud_session_installed: bool, icloud_signed_in: bool) -> Self {
        let dir = std::env::temp_dir().join(format!("cloud-calendar-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("config/cloud-calendar")).unwrap();
        let bus = fakes::Bus::start(&dir.join("bus"));
        let web = fakes::web();
        let services = fakes::serve(&bus, icloud_session_installed, icloud_signed_in, &web.url);
        Self {
            dir,
            bus,
            web,
            services,
        }
    }

    fn config(&self, toml: &str) {
        std::fs::write(self.dir.join("config/cloud-calendar/config.toml"), toml).unwrap();
    }

    fn file(&self, name: &str) -> String {
        std::fs::read_to_string(self.dir.join(name)).unwrap_or_default()
    }

    fn run(&self, args: &[&str], stdin: &str, env: &[(&str, &str)]) -> Output {
        let tests = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_cloud-calendar"));
        cmd.args(args)
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("HOME", &self.dir)
            .env("XDG_CONFIG_HOME", self.dir.join("config"))
            .env("XDG_STATE_HOME", self.dir.join("state"))
            .env("DBUS_SESSION_BUS_ADDRESS", &self.bus.address)
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .env("CLOUD_CALENDAR_HEY_COMMAND", tests.join("fake-hey"))
            .env("FAKE_HEY_LOG", self.dir.join("hey.log"))
            .env("CLOUD_CALENDAR_GWS_COMMAND", tests.join("fake-gws"))
            .env("FAKE_GWS_LOG", self.dir.join("gws.log"))
            .env("FAKE_GWS_SECRET", GOOGLE_SECRET)
            .env("CLOUD_CALENDAR_GOOGLE_CLIENT_ID", "123.apps.googleusercontent.com")
            .env("CLOUD_CALENDAR_GOOGLE_CLIENT_SECRET", GOOGLE_SECRET)
            .env("CLOUD_CALENDAR_BROWSER", "true")
            .env("CLOUD_CALENDAR_NOTIFY_COMMAND", tests.join("fake-notify"))
            .env("FAKE_NOTIFY_LOG", self.dir.join("notify.log"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (k, v) in env {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn().unwrap();
        std::io::Write::write_all(&mut child.stdin.take().unwrap(), stdin.as_bytes()).unwrap();
        child.wait_with_output().unwrap()
    }

    /// Runs with `--json`: the envelope and the exit code. A success's envelope is all of stdout; a failure's is on
    /// stderr, with nothing on stdout, and its exit code is 1 whatever went wrong.
    fn json(&self, args: &[&str], env: &[(&str, &str)]) -> (Value, i32) {
        self.json_in(args, "", env)
    }

    fn json_in(&self, args: &[&str], stdin: &str, env: &[(&str, &str)]) -> (Value, i32) {
        let mut all = vec!["--json"];
        all.extend(args);
        let out = self.run(&all, stdin, env);
        let code = out.status.code().unwrap_or(-1);
        assert!(matches!(code, 0 | 1), "exit status {code} is neither 0 nor 1");
        let envelope = if code == 0 {
            &out.stdout
        } else {
            assert!(
                out.stdout.is_empty(),
                "a failure wrote to stdout: {}",
                String::from_utf8_lossy(&out.stdout)
            );
            &out.stderr
        };
        let v = serde_json::from_slice(envelope).unwrap_or_else(|_| {
            panic!(
                "not JSON: {}\nstderr: {}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            )
        });
        (v, code)
    }

    fn link_google(&self) {
        let (v, code) = self.json(&["account", "add", "google"], &[]);
        assert_eq!(code, 0, "{v}");
    }

    fn last_post(&self) -> fakes::Req {
        self.web
            .log
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|r| r.method == "POST")
            .cloned()
            .unwrap()
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn ids(v: &Value) -> Vec<String> {
    v["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["id"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn icloud_signs_in_through_icloud_session_and_reads_the_web_calendar() {
    let home = Home::new("icloud-read", false);
    let (v, code) = home.json(&["account", "add", "icloud"], &[]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(
        home.services.session.lock().unwrap().sign_ins,
        1,
        "asked icloud-session to sign in"
    );
    assert!(v["summary"].as_str().unwrap().contains("me@icloud.com"), "{v}");
    let (v, _) = home.json(&["account", "list"], &[]);
    assert_eq!(
        v["data"][0]["detail"],
        "signed in as Ada Lovelace (me@icloud.com) through icloud-session"
    );
    assert_eq!(
        home.file("config/cloud-calendar/config.toml").trim(),
        "[accounts.icloud]",
        "nothing of iCloud's is kept here"
    );

    let (v, code) = home.json(&["calendars"], &[]);
    assert_eq!(code, 0, "{v}");
    let cals: Vec<_> = v["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| (c["id"].as_str().unwrap(), c["writable"].as_bool().unwrap()))
        .collect();
    assert_eq!(cals, vec![("icloud:birthdays", false), ("icloud:home", true)]);

    let (v, code) = home.json(&["agenda", "--from", "2026-10-12", "--days", "7"], &[]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(
        ids(&v),
        vec![
            "icloud:home/standup~20261013T080000Z",
            "icloud:home/dentist",
            "icloud:home/trip"
        ]
    );
    let dentist = &v["data"][1];
    // 10:00 in London (BST) is 09:00 UTC.
    assert_eq!(
        (
            dentist["start"].as_str(),
            dentist["location"].as_str(),
            dentist["notes"].as_str()
        ),
        (Some("2026-10-14T09:00:00+00:00"), Some("Main St"), Some("bring forms"))
    );
    assert_eq!(
        (v["data"][2]["start"].as_str(), v["data"][2]["end"].as_str()),
        (Some("2026-10-16"), Some("2026-10-18"))
    );
    assert_eq!(v["data"][0]["recurring"], true);
    let get = home
        .web
        .log
        .lock()
        .unwrap()
        .iter()
        .find(|r| r.path == "/ca/events")
        .cloned()
        .unwrap();
    assert_eq!(
        (
            get.query["startDate"].as_str(),
            get.query["endDate"].as_str(),
            get.query["usertz"].as_str()
        ),
        ("2026-10-12", "2026-10-18", "UTC"),
        "an inclusive window in your zone"
    );

    // A keyring icloud-session can't read is its own warning, not "signed out".
    home.services.session.lock().unwrap().keyring_unavailable = true;
    let (v, code) = home.json(&["today"], &[]);
    assert_eq!(
        (code, v["meta"]["warnings"][0]["code"].as_str()),
        (0, Some("keyring_unavailable")),
        "{v}"
    );
    home.services.session.lock().unwrap().keyring_unavailable = false;

    // Signed out since: a clear warning, and the command still answers.
    home.services.session.lock().unwrap().signed_in = false;
    let (v, code) = home.json(&["today"], &[]);
    assert_eq!(code, 0);
    assert_eq!(
        (
            v["meta"]["warnings"][0]["account"].as_str(),
            v["meta"]["warnings"][0]["code"].as_str()
        ),
        (Some("icloud"), Some("account_unauthorized"))
    );
}

#[test]
fn icloud_without_icloud_session_says_to_install_it() {
    let home = Home::with("no-session", false, false);
    let (v, code) = home.json(&["account", "add", "icloud"], &[]);
    assert_eq!((code, v["error"]["code"].as_str()), (1, Some("not_configured")), "{v}");
    assert!(
        v["error"]["message"]
            .as_str()
            .unwrap()
            .contains("icloud-session isn't installed"),
        "{v}"
    );
}

#[test]
fn icloud_add_change_delete() {
    let home = Home::new("icloud-write", true);
    home.config("[accounts.icloud]\n");

    let (v, code) = home.json(
        &[
            "event",
            "add",
            "--calendar",
            "icloud:home",
            "--title",
            "Lunch",
            "--start",
            "2026-10-15 12:30",
            "--length",
            "45m",
            "--location",
            "Café",
        ],
        &[],
    );
    assert_eq!(code, 0, "{v}");
    let id = v["data"]["id"].as_str().unwrap().to_string();
    let guid = id.strip_prefix("icloud:home/").unwrap().to_string();
    let post = home.last_post();
    assert_eq!(post.path, format!("/ca/events/home/{guid}"));
    assert_eq!(
        post.body["ClientState"]["Collection"],
        json!([{"guid": "home", "ctag": "FT=-@RU=home@S=7"}]),
        "the calendar's fresh ctag"
    );
    let ev = &post.body["Event"];
    assert_eq!(
        (ev["startDate"].clone(), ev["endDate"].clone()),
        (
            json!([20261015, 2026, 10, 15, 12, 30, 750]),
            json!([20261015, 2026, 10, 15, 13, 15, 795])
        )
    );
    assert_eq!(
        (
            ev["duration"].clone(),
            ev["tz"].clone(),
            ev["etag"].clone(),
            ev["location"].clone()
        ),
        (json!(45), json!("UTC"), json!(""), json!("Café"))
    );
    assert_eq!(
        (post.query["startDate"].as_str(), post.query["endDate"].as_str()),
        ("2026-10-15", "2026-10-15")
    );

    let (v, code) = home.json(
        &[
            "event",
            "add",
            "--calendar",
            "icloud:birthdays",
            "--title",
            "x",
            "--start",
            "2026-10-15",
        ],
        &[],
    );
    assert_eq!(
        (code, v["error"]["code"].as_str()),
        (1, Some("bad_request")),
        "read-only calendars refuse"
    );

    // A move keeps the length and sends the whole event back with its etag and its own alarms.
    let (v, code) = home.json(
        &[
            "event",
            "edit",
            "icloud:home/dentist",
            "--start",
            "2026-10-14 15:00",
            "--title",
            "Dentist (moved)",
        ],
        &[],
    );
    assert_eq!(code, 0, "{v}");
    let put = home.last_post();
    assert_eq!(
        (put.query["methodOverride"].as_str(), put.query["ifMatch"].as_str()),
        ("PUT", "C=dentist@U=1")
    );
    assert_eq!(put.body["Alarm"], json!([{"guid": "dentist:a1", "pGuid": "dentist"}]));
    let stored = home.web.events.lock().unwrap()["dentist"].clone();
    assert_eq!(
        (
            stored["title"].clone(),
            stored["location"].clone(),
            stored["description"].clone()
        ),
        (json!("Dentist (moved)"), json!("Main St"), json!("bring forms"))
    );
    assert_eq!(
        (
            stored["startDate"].clone(),
            stored["endDate"].clone(),
            stored["tz"].clone()
        ),
        (
            json!([20261014, 2026, 10, 14, 15, 0, 900]),
            json!([20261014, 2026, 10, 14, 16, 0, 960]),
            json!("UTC")
        )
    );

    // Repeating events are read-only on iCloud.
    let (v, code) = home.json(
        &["event", "edit", "icloud:home/standup~20261013T080000Z", "--title", "x"],
        &[],
    );
    assert_eq!((code, v["error"]["code"].as_str()), (1, Some("bad_request")), "{v}");
    assert!(v["error"]["message"].as_str().unwrap().contains("repeating"));

    let (v, code) = home.json(&["event", "delete", &id, "--yes"], &[]);
    assert_eq!(code, 0, "{v}");
    let del = home.last_post();
    assert_eq!(
        (
            del.query["methodOverride"].clone(),
            del.query["ifMatch"].clone(),
            del.body["Event"].clone()
        ),
        ("DELETE".to_string(), format!("C={guid}@U=1"), json!({}))
    );
    assert!(!home.web.events.lock().unwrap().contains_key(&guid));
    let (v, code) = home.json(&["event", "delete", &id, "--yes"], &[]);
    assert_eq!((code, v["error"]["code"].as_str()), (1, Some("not_found")));
}

#[test]
fn google_signs_in_with_the_built_in_client() {
    let home = Home::new("google", true);
    // With no client of its own set, gws gets the built-in one.
    let builtin = [
        ("CLOUD_CALENDAR_GOOGLE_CLIENT_ID", ""),
        ("CLOUD_CALENDAR_GOOGLE_CLIENT_SECRET", ""),
        ("FAKE_GWS_SECRET", cloud_calendar_api::google::GOOGLE_CLIENT_SECRET),
    ];
    let (v, code) = home.json(&["account", "add", "google"], &builtin);
    assert_eq!(code, 0, "{v}");
    assert_eq!(
        home.file("config/cloud-calendar/config.toml").trim(),
        "[accounts.google]",
        "no client details kept"
    );

    let (v, code) = home.json(
        &[
            "event",
            "add",
            "--calendar",
            "google:me@gmail.com",
            "--title",
            "Call",
            "--start",
            "2026-10-15 09:00",
        ],
        &[],
    );
    assert_eq!(
        (code, v["data"]["id"].as_str()),
        (0, Some("google:me@gmail.com/new1")),
        "{v}"
    );
    let (v, code) = home.json(
        &[
            "event",
            "edit",
            "google:me@gmail.com/s1_20261012T080000Z~20261012T080000Z",
            "--location",
            "Room 2",
        ],
        &[],
    );
    assert_eq!(code, 0, "{v}");
    let (v, code) = home.json(
        &["event", "edit", "google:me@gmail.com/g1", "--start", "2026-10-13 16:00"],
        &[],
    );
    assert_eq!(code, 0, "{v}");
    let (v, code) = home.json(
        &[
            "event",
            "delete",
            "google:me@gmail.com/s1_20261012T080000Z~20261012T080000Z",
            "--yes",
        ],
        &[],
    );
    assert_eq!(code, 0, "{v}");
    let calls: Vec<Value> = home
        .file("gws.log")
        .lines()
        .filter(|l| l.starts_with("calendar\tevents"))
        .map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            json!({ "call": format!("{} {}", f[1], f[2]), "params": serde_json::from_str::<Value>(f[4]).unwrap(), "body": f.get(6).map(|b| serde_json::from_str(b).unwrap()).unwrap_or(Value::Null) })
        })
        .collect();
    assert_eq!(calls[0]["body"]["end"]["dateTime"], "2026-10-15T10:00:00Z");
    let patches: Vec<&Value> = calls.iter().filter(|c| c["call"] == "events patch").collect();
    assert_eq!(
        (patches[0]["params"]["eventId"].as_str(), patches[0]["body"].clone()),
        (Some("s1"), json!({ "location": "Room 2" })),
        "an occurrence's edit goes to its series"
    );
    assert_eq!(
        patches[1]["body"]["end"]["dateTime"], "2026-10-13T17:00:00Z",
        "a new start keeps the length"
    );
    assert_eq!(calls.last().unwrap()["params"]["eventId"], "s1");

    let (v, code) = home.json(&["account", "remove", "google", "--yes"], &[]);
    assert_eq!(code, 0, "{v}");
    assert!(
        !home.dir.join("config/cloud-calendar/gws/google").exists(),
        "gws's sign-in is removed"
    );
}

/// A Google sign-in as fake-gws keeps one, in another Cloud app's directory.
fn sibling_sign_in(home: &Home, app_account: &str, status: &str) {
    let dir = home.dir.join("config").join(app_account);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(".encryption_key"), "key").unwrap();
    std::fs::write(dir.join("credentials.enc"), status).unwrap();
}

const ALL_SCOPES: &str = r#""https://www.googleapis.com/auth/gmail.modify","https://www.googleapis.com/auth/calendar.events","https://www.googleapis.com/auth/calendar.calendarlist.readonly""#;

#[test]
fn google_takes_over_another_cloud_apps_sign_in() {
    let home = Home::new("google-adopt", true);
    let builtin = [
        ("CLOUD_CALENDAR_GOOGLE_CLIENT_ID", ""),
        ("CLOUD_CALENDAR_GOOGLE_CLIENT_SECRET", ""),
        ("FAKE_GWS_SECRET", cloud_calendar_api::google::GOOGLE_CLIENT_SECRET),
    ];
    let logins = |home: &Home| {
        home.file("gws.log")
            .lines()
            .filter(|l| l.starts_with("auth\tlogin"))
            .count()
    };
    let status = format!(r#"{{"token_valid":true,"user":"Sam@Gmail.com","scopes":[{ALL_SCOPES}]}}"#);
    sibling_sign_in(&home, "cloudmail/gws/gmail", &status);
    // One Google no longer takes, and one without the calendar permissions: neither counts.
    sibling_sign_in(
        &home,
        "cloud-contacts/gws/google",
        r#"{"token_valid":false,"token_error":"Token has been expired or revoked."}"#,
    );
    sibling_sign_in(
        &home,
        "cloud-contacts/gws/work",
        r#"{"token_valid":true,"user":"other@gmail.com","scopes":["https://www.googleapis.com/auth/contacts"]}"#,
    );

    let (v, code) = home.json(&["account", "add", "google"], &builtin);
    assert_eq!(code, 0, "{v}");
    assert!(
        v["summary"]
            .as_str()
            .unwrap()
            .contains("as sam@gmail.com, with Cloudmail's sign-in"),
        "{v}"
    );
    assert_eq!(logins(&home), 0, "no browser sign-in");
    let own = home.dir.join("config/cloud-calendar/gws/google");
    assert_eq!(std::fs::read_to_string(own.join("credentials.enc")).unwrap(), status);
    assert_eq!(std::fs::read_to_string(own.join(".encryption_key")).unwrap(), "key");
    let (v, code) = home.json(&["calendars"], &builtin);
    assert_eq!(code, 0, "{v}");

    // That address is linked here now, so a second account signs in in the browser, asking for
    // what the three apps use.
    let (v, code) = home.json(&["account", "add", "google", "--name", "work"], &builtin);
    assert_eq!(code, 0, "{v}");
    assert_eq!(logins(&home), 1);
    assert!(
        home.file("gws.log").lines().any(|l| l.starts_with("auth\tlogin")
            && l.contains("auth/gmail.modify")
            && l.contains("auth/calendar.events")
            && l.contains("auth/contacts")),
        "{}",
        home.file("gws.log")
    );

    // Signing in again to an account whose sign-in works is a browser sign-in: the way to another address.
    let (v, code) = home.json(&["account", "login", "google"], &builtin);
    assert_eq!(code, 0, "{v}");
    assert_eq!(logins(&home), 2);

    // Unlinking removes the copy only.
    let (v, code) = home.json(&["account", "remove", "google", "--yes"], &builtin);
    assert_eq!(code, 0, "{v}");
    assert!(home.dir.join("config/cloudmail/gws/gmail/credentials.enc").is_file());
}

#[test]
fn several_google_accounts_each_take_over_their_own_address() {
    let home = Home::new("google-adopt-two", true);
    let builtin = [
        ("CLOUD_CALENDAR_GOOGLE_CLIENT_ID", ""),
        ("CLOUD_CALENDAR_GOOGLE_CLIENT_SECRET", ""),
        ("FAKE_GWS_SECRET", cloud_calendar_api::google::GOOGLE_CLIENT_SECRET),
    ];
    let logins = |home: &Home| {
        home.file("gws.log")
            .lines()
            .filter(|l| l.starts_with("auth\tlogin"))
            .count()
    };
    let status = |user: &str| format!(r#"{{"token_valid":true,"user":"{user}","scopes":[{ALL_SCOPES}]}}"#);
    sibling_sign_in(&home, "cloudmail/gws/gmail", &status("sam@gmail.com"));
    sibling_sign_in(&home, "cloud-contacts/gws/google", &status("work@example.com"));
    let own = |name: &str, file: &str| home.file(&format!("config/cloud-calendar/gws/{name}/{file}"));

    // `--address` says which one a new account is for.
    let (v, code) = home.json(
        &[
            "account",
            "add",
            "google",
            "--name",
            "picked",
            "--address",
            "Work@Example.com",
        ],
        &builtin,
    );
    assert_eq!(code, 0, "{v}");
    assert!(v["summary"].as_str().unwrap().contains("as work@example.com"), "{v}");
    let (v, code) = home.json(&["account", "add", "google", "--address", "work@example.com"], &builtin);
    assert_eq!((code, v["error"]["code"].as_str()), (1, Some("bad_request")), "{v}");
    assert!(
        v["error"]["message"]
            .as_str()
            .unwrap()
            .contains("work@example.com is already linked here as picked"),
        "{v}"
    );
    let (v, code) = home.json(&["account", "add", "icloud", "--address", "a@b.c"], &builtin);
    assert_eq!((code, v["error"]["code"].as_str()), (1, Some("bad_request")), "{v}");
    let (v, code) = home.json(&["account", "remove", "picked", "--yes"], &builtin);
    assert_eq!(code, 0, "{v}");
    assert_eq!(logins(&home), 0);
    // An address no other app has is signed in to in the browser, and must be the one it signs in to.
    let out = home.run(
        &["--json", "account", "add", "google", "--address", "new@example.com"],
        "",
        &builtin,
    );
    assert_eq!(out.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("the browser signed in to me@gmail.com, not new@example.com"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!home.dir.join("config/cloud-calendar/gws/google").exists());
    let _ = std::fs::remove_file(home.dir.join("gws.log"));

    // One address at a time, Cloudmail's first; then nothing is left to take over.
    let (v, code) = home.json(&["account", "add", "google"], &builtin);
    assert_eq!(code, 0, "{v}");
    assert!(v["summary"].as_str().unwrap().contains("as sam@gmail.com"), "{v}");
    let (v, code) = home.json(&["account", "add", "google"], &builtin);
    assert_eq!((code, v["error"]["code"].as_str()), (1, Some("bad_request")));
    assert!(v["error"]["message"].as_str().unwrap().contains("--name"), "{v}");
    let (v, code) = home.json(&["account", "add", "google", "--name", "work"], &builtin);
    assert_eq!(code, 0, "{v}");
    assert!(
        v["summary"]
            .as_str()
            .unwrap()
            .contains("as work@example.com, with Cloud Contacts's sign-in"),
        "{v}"
    );
    assert_eq!(logins(&home), 0);
    assert_eq!(
        (own("google", "address"), own("work", "address")),
        ("sam@gmail.com".into(), "work@example.com".into())
    );
    let (v, code) = home.json(&["account", "list"], &builtin);
    assert_eq!(code, 0, "{v}");
    assert!(v.to_string().contains("work@example.com, signed in via"), "{v}");

    // A third account signs in in the browser, and not to an address linked here already.
    let third = |user: &'static str| {
        let mut env = builtin.to_vec();
        env.push(("FAKE_GWS_USER", user));
        home.run(&["--json", "account", "add", "google", "--name", "third"], "", &env)
    };
    // (The sign-in link is on stderr before the error.)
    let out = third("Sam@gmail.com");
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("sam@gmail.com is already linked here as google"),
        "{stderr}"
    );
    assert!(!home.dir.join("config/cloud-calendar/gws/third").exists());
    assert!(!home.file("config/cloud-calendar/config.toml").contains("third"));
    let out = third("third@example.com");
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(logins(&home), 2);
    assert_eq!(own("third", "address"), "third@example.com");

    // An account Google stopped taking is signed in again with its own address's sign-in, not the first there is.
    let dead = home.dir.join("config/cloud-calendar/gws/work/credentials.enc");
    std::fs::write(&dead, r#"{"token_valid":false}"#).unwrap();
    let (v, code) = home.json(&["account", "login", "work"], &builtin);
    assert_eq!(code, 0, "{v}");
    assert_eq!(logins(&home), 2, "{v}");
    assert_eq!(std::fs::read_to_string(&dead).unwrap(), status("work@example.com"));

    // A client of your own never takes over a sign-in made with the built-in one.
    let home = Home::new("google-adopt-own-client", true);
    sibling_sign_in(&home, "cloudmail/gws/gmail", &status("sam@gmail.com"));
    home.link_google();
    assert_eq!(logins(&home), 1);
}

#[test]
fn hey_insists_on_the_keyring_and_writes_through_the_cli() {
    let home = Home::new("hey", true);
    let (v, code) = home.json(&["account", "add", "hey"], &[("FAKE_HEY_STORAGE", "file")]);
    assert_eq!(
        (code, v["error"]["code"].as_str()),
        (1, Some("account_unauthorized")),
        "{v}"
    );
    assert!(v["error"]["message"].as_str().unwrap().contains("plain file"));
    let (v, code) = home.json(&["account", "add", "hey"], &[("HEY_NO_KEYRING", "1")]);
    assert_eq!(code, 0, "HEY_NO_KEYRING never reaches hey: {v}");

    let (v, code) = home.json(
        &[
            "event",
            "add",
            "--calendar",
            "hey:11",
            "--title",
            "Dinner",
            "--start",
            "2026-10-15 19:00",
            "--end",
            "2026-10-15 21:00",
            "--notes",
            "bring wine",
        ],
        &[],
    );
    assert_eq!((code, v["data"]["id"].as_str()), (0, Some("hey:777@2026-10-15")), "{v}");
    let (v, code) = home.json(
        &[
            "event",
            "add",
            "--calendar",
            "hey:11",
            "--title=-Away",
            "--start",
            "2026-10-20",
            "--end",
            "2026-10-23",
        ],
        &[],
    );
    assert_eq!(code, 0, "{v}");
    let (v, code) = home.json(&["event", "edit", "hey:500~20261013T060000Z", "--title", "Gym!"], &[]);
    assert_eq!(
        (code, v["error"]["code"].as_str()),
        (1, Some("bad_request")),
        "a repeating HEY event is read-only: {v}"
    );
    let (v, code) = home.json(
        &["event", "edit", "hey:401@2026-10-14", "--start", "2026-10-14 13:00"],
        &[],
    );
    assert_eq!(
        (code, v["error"]["code"].as_str()),
        (1, Some("bad_request")),
        "a HEY move names both ends: {v}"
    );
    // Without the day it starts on, hey looks only a year either side of today: its error and
    // hint come through (hey writes them to stderr).
    let (v, code) = home.json(&["event", "edit", "hey:403", "--title", "Far"], &[]);
    assert_eq!((code, v["error"]["code"].as_str()), (1, Some("not_found")), "{v}");
    assert!(
        v["error"]["message"]
            .as_str()
            .unwrap()
            .contains("event \"403\" not found (hey event edit 403 <YYYY-MM-DD>"),
        "{v}"
    );
    let (v, code) = home.json(&["event", "edit", "hey:401@2026-10-14", "--title", "Lunch!"], &[]);
    assert_eq!((code, v["data"]["id"].as_str()), (0, Some("hey:401@2026-10-14")), "{v}");
    // A move to another day returns the ID that carries the new day, and the next edit uses it.
    let (v, code) = home.json(
        &[
            "event",
            "edit",
            "hey:401@2026-10-14",
            "--start",
            "2026-10-20 13:00",
            "--end",
            "2026-10-20 14:30",
            "--location",
            "",
        ],
        &[],
    );
    assert_eq!((code, v["data"]["id"].as_str()), (0, Some("hey:401@2026-10-20")), "{v}");
    assert_eq!(
        v["meta"]["warnings"][0]["code"], "edit_side_effects",
        "a HEY edit says what it drops: {v}"
    );
    let (v, code) = home.json(&["event", "edit", "hey:401@2026-10-20", "--title", "Lunch moved"], &[]);
    assert_eq!(code, 0, "the returned ID finds the moved event: {v}");
    let (v, code) = home.json(&["event", "edit", "hey:401@2026-10-14", "--title", "Stale"], &[]);
    assert_eq!(
        (code, v["error"]["code"].as_str()),
        (1, Some("not_found")),
        "the old day no longer finds it: {v}"
    );
    let (v, code) = home.json(&["event", "delete", "hey:500~20261013T060000Z", "--yes"], &[]);
    assert_eq!(code, 0, "{v}");
    let writes: Vec<String> = home
        .file("hey.log")
        .lines()
        .filter(|l| l.starts_with("event\t"))
        .map(str::to_string)
        .collect();
    assert_eq!(
        writes,
        vec![
            "event\tadd\t--title=Dinner\t--calendar\t11\t--time-zone\tUTC\t--starts-on\t2026-10-15\t--start-time\t19:00\t--ends-on\t2026-10-15\t--end-time\t21:00\t--notes\tbring wine",
            "event\tadd\t--title=-Away\t--calendar\t11\t--all-day\t--starts-on\t2026-10-20\t--ends-on\t2026-10-22",
            "event\tedit\t403\t--title=Far",
            "event\tedit\t401\t2026-10-14\t--title=Lunch!",
            "event\tedit\t401\t2026-10-14\t--location\t\t--time-zone\tUTC\t--all-day=false\t--starts-on\t2026-10-20\t--start-time\t13:00\t--ends-on\t2026-10-20\t--end-time\t14:30",
            "event\tedit\t401\t2026-10-20\t--title=Lunch moved",
            "event\tedit\t401\t2026-10-14\t--title=Stale",
            "event\tdelete\t500",
        ]
    );
}

#[test]
fn accounts_merge_into_one_agenda() {
    let home = Home::new("merge", true);
    home.link_google();
    let mut config = home.file("config/cloud-calendar/config.toml");
    config.push_str("\n[accounts.hey]\n\n[accounts.icloud]\n");
    home.config(&config);

    let (v, code) = home.json(&["agenda", "--from", "2026-10-12", "--days", "7"], &[]);
    assert_eq!(code, 0, "{v}");
    let got: Vec<_> = v["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        got,
        vec![
            "google:en.usa#holiday@group.v.calendar.google.com/hol1",
            "google:me@gmail.com/s1_20261012T080000Z~20261012T080000Z",
            "hey:500~20261013T060000Z",
            "icloud:home/standup~20261013T080000Z",
            "google:me@gmail.com/g1",
            "icloud:home/dentist",
            "hey:401@2026-10-14",
            "hey:402@2026-10-15",
            "icloud:home/trip",
        ]
    );
    let lunch = v["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["id"] == "hey:401@2026-10-14")
        .unwrap();
    assert_eq!(
        lunch["notes"], "Bring the menu",
        "a HEY event's notes are its description"
    );
    let trip = v["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["id"] == "hey:402@2026-10-15")
        .unwrap();
    assert_eq!(
        (trip["start"].as_str(), trip["end"].as_str()),
        (Some("2026-10-15"), Some("2026-10-16")),
        "ending at midnight, a one-day event"
    );
    assert!(
        !v.to_string().contains("Should not show"),
        "calendars hidden in Google aren't read"
    );

    // Failing accounts are warnings; the rest still answer.
    let (v, code) = home.json(
        &["agenda", "--from", "2026-10-12", "--days", "7"],
        &[("FAKE_GWS_MODE", "expired"), ("FAKE_HEY_MODE", "crash")],
    );
    assert_eq!(code, 0);
    let mut codes: Vec<_> = v["meta"]["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| {
            (
                w["account"].as_str().unwrap().to_string(),
                w["code"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    codes.sort();
    assert_eq!(
        codes,
        vec![
            ("google".to_string(), "account_unauthorized".to_string()),
            ("hey".to_string(), "account_unavailable".to_string())
        ]
    );
    assert!(ids(&v).iter().all(|i| i.starts_with("icloud:")) && !ids(&v).is_empty());
}

#[test]
fn notifications_go_out_once() {
    let home = Home::new("notify", true);
    home.config("notify_minutes = [10]\n\n[accounts.hey]\n");
    let soon = [("FAKE_HEY_SOON", "1")];
    let (v, code) = home.json(&["notify"], &soon);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["data"]["sent"][0]["title"], "Dentist");
    let log = home.file("notify.log");
    let fields: Vec<&str> = log.lines().next().unwrap().split('\t').collect();
    assert_eq!(
        &fields[..6],
        &["--app-name", "Cloud Calendar", "-g", "󰃭", "-u", "normal"]
    );
    assert_eq!(fields[6], "Dentist");
    assert!(
        fields[7].starts_with("in 5 min · ") && fields[7].contains("Main St"),
        "{}",
        fields[7]
    );
    // A click opens the window by the app's one name.
    assert_eq!(&fields[8..], &["--exec", "cloud-calendar", "open"]);
    // A minute later: from the cache, and nothing twice.
    let weeks = |h: &Home| {
        h.file("hey.log")
            .lines()
            .filter(|l| l.starts_with("event\tweek"))
            .count()
    };
    let read = weeks(&home);
    let (v, _) = home.json(&["notify"], &soon);
    assert_eq!(
        (
            v["data"]["sent"].as_array().unwrap().len(),
            v["data"]["refreshed"].as_bool()
        ),
        (0, Some(false))
    );
    assert_eq!(home.file("notify.log").lines().count(), 1);
    assert_eq!(weeks(&home), read, "the cache spared a second read");
}

#[test]
fn agenda_days_must_be_in_range() {
    let home = Home::new("days", true);
    for days in ["0", "367"] {
        let (v, code) = home.json(&["agenda", "--days", days], &[]);
        assert_eq!((code, v["error"]["code"].as_str()), (1, Some("bad_request")), "{v}");
    }
}

#[test]
fn out_of_range_dates_and_lengths_are_refused() {
    let home = Home::new("ranges", true);
    home.config("[accounts.hey]\n");
    for args in [
        &["agenda", "--from", "+999999999999"][..],
        &["agenda", "--from", "+99999999999999999999"],
        &["week", "+999999999999"],
        &[
            "event",
            "add",
            "--calendar",
            "hey:11",
            "--title",
            "X",
            "--start",
            "2026-10-15 09:00",
            "--length",
            "100000000d",
        ],
        &[
            "event",
            "add",
            "--calendar",
            "hey:11",
            "--title",
            "X",
            "--start",
            "2026-10-15 09:00",
            "--length",
            "1000000000000000m",
        ],
        &[
            "event",
            "add",
            "--calendar",
            "hey:11",
            "--title",
            "X",
            "--start",
            "9999-12-31 23:30",
            "--length",
            "1h",
        ],
    ] {
        let (v, code) = home.json(args, &[]);
        assert_eq!(
            (code, v["error"]["code"].as_str()),
            (1, Some("bad_request")),
            "{args:?}: {v}"
        );
    }
}

#[test]
fn accounts_linked_at_once_both_stay() {
    let home = Home::new("race", true);
    std::thread::scope(|s| {
        // The first add's sign-in is slow; the second links meanwhile.
        let slow = s.spawn(|| {
            home.json(
                &["account", "add", "hey", "--name", "slow"],
                &[("FAKE_HEY_STATUS_DELAY", "2")],
            )
        });
        std::thread::sleep(std::time::Duration::from_millis(500));
        let (v, code) = home.json(&["account", "add", "hey", "--name", "quick"], &[]);
        assert_eq!(code, 0, "{v}");
        let (v, code) = slow.join().unwrap();
        assert_eq!(code, 0, "{v}");
    });
    let config = home.file("config/cloud-calendar/config.toml");
    assert!(
        config.contains("[accounts.slow]") && config.contains("[accounts.quick]"),
        "{config}"
    );
}

#[test]
fn usage_errors_are_json_on_stderr_with_exit_1_when_piped() {
    let home = Home::new("usage", false);
    let (v, code) = home.json(&["no-such-command"], &[]);
    assert_eq!(code, 1, "{v}");
    assert_eq!(v["error"]["code"], "usage", "{v}");
    assert!(v["error"]["hint"].as_str().unwrap().contains("--help"), "{v}");
}

/// Every failure exits 1 with stdout empty, so a pipeline never reads an error as data: as text with `--styled`
/// (clap's usage errors included, which would otherwise exit 2), and as a JSON error on stderr with `--quiet`.
#[test]
fn failures_exit_1_on_stderr_in_every_mode() {
    let home = Home::new("failures", false);
    for (args, kind) in [
        (&["--styled", "no-such-command"][..], None),
        (&["--styled", "agenda", "--days", "0"], None),
        (&["--quiet", "agenda", "--days", "0"], Some("bad_request")),
        (
            &["--ids-only", "event", "edit", "hey:1", "--title", "x"],
            Some("not_configured"),
        ),
    ] {
        let out = home.run(args, "", &[]);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(1), "{args:?}: {stderr}");
        assert!(
            out.stdout.is_empty(),
            "{args:?} wrote to stdout: {}",
            String::from_utf8_lossy(&out.stdout)
        );
        match kind {
            None => assert!(stderr.starts_with("error:"), "{args:?}: {stderr}"),
            Some(kind) => {
                let v: Value = serde_json::from_str(&stderr).unwrap_or_else(|_| panic!("{args:?}: not JSON: {stderr}"));
                assert_eq!(
                    (v["ok"].as_bool(), v["error"]["code"].as_str()),
                    (Some(false), Some(kind)),
                    "{args:?}"
                );
            }
        }
    }
    let out = home.run(&["--help"], "", &[]);
    assert_eq!(out.status.code(), Some(0), "help is a success");
}

/// A stand-in for the desktop app's binary (`tests/fake-app`), which writes down in `app.log` that it ran, and with
/// what. It and every other program the tests run are checked in, never written by a test: a test that wrote one and
/// ran it would race another test's fork holding the write end open, and fail with ETXTBSY ("Text file busy").
fn fake_app(home: &Home) -> (PathBuf, PathBuf) {
    (
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fake-app"),
        home.dir.join("app.log"),
    )
}

/// `cloud-calendar open` is the window: it becomes the app's binary, which the command line never links.
#[test]
fn open_runs_the_desktop_app() {
    let home = Home::new("open", true);
    let (app, log) = fake_app(&home);
    let out = home.run(
        &["open"],
        "",
        &[
            ("CLOUD_CALENDAR_GTK_BINARY", app.to_str().unwrap()),
            ("FAKE_APP_LOG", log.to_str().unwrap()),
        ],
    );
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(home.file("app.log"), "ran \n");
    // Without the app it says where it looked, on stderr, and exits 1.
    let missing = home.dir.join("no-such-app");
    let (v, code) = home.json(&["open"], &[("CLOUD_CALENDAR_GTK_BINARY", missing.to_str().unwrap())]);
    assert_eq!((code, v["error"]["code"].as_str()), (1, Some("not_configured")), "{v}");
    assert!(v["error"]["message"].as_str().unwrap().contains("no-such-app"), "{v}");
}

/// The window's old command still opens it, for the key bindings and older notifications that run it: the package
/// installs packaging/compat/cloud-calendar-gtk beside `cloud-calendar`, and it runs `cloud-calendar open`.
#[test]
fn the_old_command_opens_the_window() {
    let home = Home::new("old-command", true);
    let (app, log) = fake_app(&home);
    let bin = home.dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_cloud-calendar"), bin.join("cloud-calendar")).unwrap();
    let shim = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packaging/compat/cloud-calendar-gtk");
    // Linked, not copied: the shim runs the `cloud-calendar` beside the name it was run by.
    std::os::unix::fs::symlink(shim, bin.join("cloud-calendar-gtk")).unwrap();
    let out = Command::new(bin.join("cloud-calendar-gtk"))
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", &home.dir)
        .env("XDG_CONFIG_HOME", home.dir.join("config"))
        .env("CLOUD_CALENDAR_GTK_BINARY", &app)
        .env("FAKE_APP_LOG", &log)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(home.file("app.log"), "ran \n");
}

/// Help and the version need no display, and the command line links no GTK: only `open` reaches the window.
#[test]
fn help_and_version_need_no_display_and_no_gtk() {
    let home = Home::new("help", true);
    for args in [&["--help"][..], &["--version"], &["open", "--help"]] {
        let out = home.run(args, "", &[]);
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(!out.stdout.is_empty(), "{args:?} printed nothing");
    }
    let help = String::from_utf8(home.run(&["--help"], "", &[]).stdout).unwrap();
    assert!(help.contains("open"), "{help}");
    let binary = std::fs::read(env!("CARGO_BIN_EXE_cloud-calendar")).unwrap();
    let links = |library: &[u8]| binary.windows(library.len()).any(|w| w == library);
    assert!(!links(b"libgtk-4.so"), "the command line links GTK");
}
