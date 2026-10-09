mod cli;
mod render;

use clap::Parser;
use cloud_calendar_api::{self as api, Calendars, Error, ErrorKind, EventChange, NewEvent, Range, config, provider};
use serde_json::{Value, json};
use std::io::{IsTerminal, Read, Write};

use cli::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Human,
    Json,
    Quiet,
    Ids,
    Count,
}

impl Mode {
    fn from(g: &GlobalArgs) -> Self {
        if g.ids_only {
            Mode::Ids
        } else if g.count {
            Mode::Count
        } else if g.quiet {
            Mode::Quiet
        } else if g.json {
            Mode::Json
        } else if g.styled || std::io::stdout().is_terminal() {
            Mode::Human
        } else {
            Mode::Json
        }
    }
}

/// A command's result: data for machines, text for people, and accounts that failed on the way.
pub struct Out {
    pub data: Value,
    pub summary: String,
    pub human: String,
    pub ids: Vec<String>,
    pub warnings: Vec<api::AccountWarning>,
}

impl Out {
    fn new(data: Value, summary: impl Into<String>) -> Self {
        let summary = summary.into();
        Self { data, human: summary.clone(), summary, ids: Vec::new(), warnings: Vec::new() }
    }
}

fn exit_code(kind: ErrorKind) -> i32 {
    match kind {
        ErrorKind::BadRequest => 2,
        ErrorKind::Config | ErrorKind::AccountAuth => 3,
        ErrorKind::NotFound => 4,
        ErrorKind::AccountUnavailable => 5,
    }
}

fn main() {
    let cli = Cli::parse();
    let mode = Mode::from(&cli.global);
    let Some(command) = cli.command else {
        let _ = <Cli as clap::CommandFactory>::command().print_help();
        return;
    };
    match run(command) {
        Ok(out) => {
            let mut stdout = std::io::stdout().lock();
            let _ = match mode {
                Mode::Human => {
                    for w in &out.warnings {
                        eprintln!("warning: {}: {}", w.account, w.message);
                    }
                    writeln!(stdout, "{}", out.human.trim_end())
                }
                Mode::Json => writeln!(stdout, "{}", json!({ "ok": true, "data": out.data, "summary": out.summary, "meta": { "warnings": out.warnings } })),
                Mode::Quiet => writeln!(stdout, "{}", out.data),
                Mode::Ids => out.ids.iter().try_for_each(|i| writeln!(stdout, "{i}")),
                Mode::Count => writeln!(stdout, "{}", out.data.as_array().map(Vec::len).unwrap_or(out.ids.len())),
            };
            if mode != Mode::Human {
                for w in &out.warnings {
                    eprintln!("warning: {}: {}", w.account, w.message);
                }
            }
        }
        Err(e) => {
            if mode == Mode::Human {
                eprintln!("error: {}", e.message);
            } else {
                println!("{}", json!({ "ok": false, "error": { "code": e.kind.code(), "message": e.message } }));
            }
            std::process::exit(exit_code(e.kind));
        }
    }
}

fn today() -> chrono::NaiveDate {
    chrono::Local::now().date_naive()
}

fn load() -> api::Result<(api::Config, Calendars)> {
    let config = config::load()?;
    let calendars = Calendars::from_config(&config);
    Ok((config, calendars))
}

fn need_accounts(c: &Calendars) -> api::Result<()> {
    if c.is_empty() && c.broken.is_empty() {
        return Err(Error::new(ErrorKind::Config, "no calendar accounts linked yet; add one with `cloud-calendar account add icloud|google|hey`"));
    }
    Ok(())
}

fn run(command: Command) -> api::Result<Out> {
    match command {
        Command::Status => status(),
        Command::Account(a) => account(a),
        Command::Calendars => {
            let (_, c) = load()?;
            need_accounts(&c)?;
            let l = c.calendars();
            let mut out = Out::new(serde_json::to_value(&l.items).unwrap_or_default(), format!("{} calendars", l.items.len()));
            out.human = render::calendars(&l.items);
            out.ids = l.items.iter().map(|c| c.id.clone()).collect();
            out.warnings = l.warnings;
            Ok(out)
        }
        Command::Agenda(a) => {
            let from = api::time::parse_date(&a.from, today())?;
            agenda(Range::days(from, a.days.clamp(1, 366)), format!("{} days from {from}", a.days))
        }
        Command::Today => agenda(Range::days(today(), 1), format!("today, {}", today())),
        Command::Week { date } => {
            let day = match date {
                Some(d) => api::time::parse_date(&d, today())?,
                None => today(),
            };
            let monday = day - chrono::Days::new(u64::from(chrono::Datelike::weekday(&day).num_days_from_monday()));
            agenda(Range::days(monday, 7), format!("the week of {monday}"))
        }
        Command::Event(e) => event(e),
        Command::Notify { refresh } => {
            let (config, c) = load()?;
            let report = api::notify::run(&c, &config.notify_minutes(), chrono::Utc::now(), refresh);
            let mut out = Out::new(serde_json::to_value(&report).unwrap_or_default(), format!("{} notifications sent", report.sent.len()));
            out.ids = report.sent.iter().map(|s| s.id.clone()).collect();
            out.warnings = report.warnings;
            Ok(out)
        }
    }
}

fn agenda(range: Range, what: String) -> api::Result<Out> {
    let (_, c) = load()?;
    need_accounts(&c)?;
    let l = c.events(&range);
    let mut out = Out::new(serde_json::to_value(&l.items).unwrap_or_default(), format!("{} events in {what}", l.items.len()));
    out.human = render::agenda(&l.items, &range);
    out.ids = l.items.iter().map(|e| e.id.clone()).collect();
    out.warnings = l.warnings;
    Ok(out)
}

fn status() -> api::Result<Out> {
    let (config, c) = load()?;
    let statuses = c.statuses();
    let path = config::path();
    let data = json!({
        "config": path,
        "notify_minutes": config.notify_minutes(),
        "accounts": statuses,
        "broken": c.broken,
    });
    let mut out = Out::new(data, format!("{} accounts, {} working", statuses.len(), statuses.iter().filter(|s| s.ok).count()));
    out.human = format!("config: {}\n{}", path.display(), render::accounts(&statuses, &c.broken));
    Ok(out)
}

fn read_password(from_stdin: bool, prompt: &str) -> api::Result<String> {
    let mut password = String::new();
    if from_stdin || !std::io::stdin().is_terminal() {
        std::io::stdin().read_to_string(&mut password).map_err(|e| Error::bad_request(format!("could not read the password: {e}")))?;
    } else {
        eprint!("{prompt}");
        let _ = std::io::stderr().flush();
        let tty = || std::fs::File::open("/dev/tty").map(std::process::Stdio::from);
        let echo = |on: bool| tty().map(|t| std::process::Command::new("stty").arg(if on { "echo" } else { "-echo" }).stdin(t).status());
        let _ = echo(false);
        let r = std::io::stdin().read_line(&mut password);
        let _ = echo(true);
        eprintln!();
        r.map_err(|e| Error::bad_request(format!("could not read the password: {e}")))?;
    }
    let password = password.trim().to_string();
    if password.is_empty() {
        return Err(Error::bad_request("no password given"));
    }
    Ok(password)
}

/// Checks an iCloud app-specific password against Apple and keeps it in the keyring.
fn icloud_sign_in(name: &str, cfg: &config::AccountConfig, from_stdin: bool) -> api::Result<String> {
    let user = cfg.username.clone().ok_or_else(|| Error::bad_request("iCloud needs --username <your Apple Account email>"))?;
    eprintln!("Make an app-specific password at https://account.apple.com (Sign-In and Security → App-Specific Passwords).");
    let password = read_password(from_stdin, &format!("App-specific password for {user}: "))?;
    let n = api::caldav::ICloud::new(name, cfg).verify(&password)?;
    api::secret::store(name, &user, &format!("Cloud Calendar: iCloud ({user})"), &password)?;
    Ok(format!("signed in to iCloud as {user} ({n} calendars)"))
}

fn sign_in(name: &str, cfg: &config::AccountConfig, from_stdin: bool) -> api::Result<String> {
    match cfg.provider(name) {
        "icloud" => icloud_sign_in(name, cfg, from_stdin),
        "google" => api::google::Google::new(name, cfg).login().map(|_| "signed in to Google".to_string()),
        "hey" => {
            let hey = api::hey::Hey::new(name, cfg);
            if hey.signed_in()? {
                return Ok("the hey CLI is signed in".into());
            }
            hey.login().map(|_| "signed in to HEY".to_string())
        }
        other => Err(Error::new(ErrorKind::Config, format!("unknown provider {other}"))),
    }
}

fn account(a: AccountCommand) -> api::Result<Out> {
    match a {
        AccountCommand::List => {
            let (_, c) = load()?;
            let statuses = c.statuses();
            let mut out = Out::new(json!(statuses), format!("{} accounts", statuses.len()));
            out.human = render::accounts(&statuses, &c.broken);
            out.ids = statuses.iter().map(|s| s.name.clone()).collect();
            Ok(out)
        }
        AccountCommand::Add(args) => {
            if !provider::KNOWN_PROVIDERS.iter().any(|(p, _)| *p == args.provider) {
                let known: Vec<_> = provider::KNOWN_PROVIDERS.iter().map(|(p, d)| format!("{p} ({d})")).collect();
                return Err(Error::bad_request(format!("unknown provider \"{}\"; one of: {}", args.provider, known.join(", "))));
            }
            let name = args.name.clone().unwrap_or_else(|| args.provider.clone());
            if !provider::valid_name(&name) {
                return Err(Error::bad_request(format!("\"{name}\" can't be an account name: use lowercase letters, digits and dashes")));
            }
            let mut config = config::load()?;
            if config.accounts.contains_key(&name) {
                return Err(Error::bad_request(format!("an account named {name} is already linked; pick another --name, or `cloud-calendar account login {name}`")));
            }
            if args.provider == "icloud" && args.username.as_deref().is_none_or(|u| !u.contains('@')) {
                return Err(Error::bad_request("iCloud needs --username <your Apple Account email>"));
            }
            let cfg = config::AccountConfig {
                provider: (name != args.provider).then(|| args.provider.clone()),
                command: args.command,
                account: args.hey_account,
                username: args.username,
                url: args.url,
                client_id: args.client_id,
                client_secret: args.client_secret,
            };
            let note = if args.no_login { "saved without signing in".to_string() } else { sign_in(&name, &cfg, args.password_stdin)? };
            config.accounts.insert(name.clone(), cfg);
            let path = config::save(&config)?;
            Ok(Out::new(json!({ "name": name, "config": path }), format!("linked {name}: {note}")))
        }
        AccountCommand::Login { name, password_stdin } => {
            let config = config::load()?;
            let cfg = config.accounts.get(&name).ok_or_else(|| Error::new(ErrorKind::NotFound, format!("no account named {name}")))?;
            let note = sign_in(&name, cfg, password_stdin)?;
            Ok(Out::new(json!({ "name": name }), note))
        }
        AccountCommand::Remove { name, yes } => {
            if !yes {
                return Err(Error::bad_request(format!("removing {name} forgets its sign-in on this computer; run again with --yes")));
            }
            let mut config = config::load()?;
            let cfg = config.accounts.remove(&name).ok_or_else(|| Error::new(ErrorKind::NotFound, format!("no account named {name}")))?;
            match cfg.provider(&name) {
                "icloud" => api::secret::clear(&name)?,
                "google" => {
                    let _ = api::google::Google::new(&name, &cfg).forget();
                }
                _ => {}
            }
            config::save(&config)?;
            Ok(Out::new(json!({ "name": name }), format!("unlinked {name}; its calendars are untouched")))
        }
    }
}

fn event(e: EventCommand) -> api::Result<Out> {
    let (_, c) = load()?;
    need_accounts(&c)?;
    let today = today();
    match e {
        EventCommand::Add(a) => {
            let start = api::time::parse_time(&a.start, today)?;
            let end = match (&a.end, &a.length) {
                (Some(e), _) => api::time::parse_time(e, start.local_date())?,
                (None, Some(l)) => api::time::add_length(&start, api::time::parse_length(l)?),
                (None, None) => api::time::add_length(&start, chrono::Duration::hours(if start.is_date() { 24 } else { 1 })),
            };
            api::check_span(&start, &end)?;
            let id = c.create(&NewEvent { calendar_id: a.calendar, title: a.title.clone(), start, end, location: a.location, notes: a.notes })?;
            let mut out = Out::new(json!({ "id": id }), format!("added \"{}\" ({start} – {end})", a.title));
            out.ids = vec![id];
            Ok(out)
        }
        EventCommand::Edit(a) => {
            let start = a.start.as_deref().map(|s| api::time::parse_time(s, today)).transpose()?;
            let end = a.end.as_deref().map(|s| api::time::parse_time(s, start.map(|s| s.local_date()).unwrap_or(today))).transpose()?;
            let change = EventChange { title: a.title, start, end, location: a.location, notes: a.notes };
            if change.is_empty() {
                return Err(Error::bad_request("nothing to change: give --title, --start, --end, --location or --notes"));
            }
            c.update(&a.id, &change)?;
            let mut out = Out::new(json!({ "id": a.id }), format!("changed {}", a.id));
            out.ids = vec![a.id];
            Ok(out)
        }
        EventCommand::Delete { id, yes } => {
            let series = provider::split_occurrence(id.split_once(':').map(|(_, l)| l).unwrap_or("")).1.is_some();
            if !yes {
                let what = if series { "every occurrence of this repeating event" } else { "this event" };
                return Err(Error::bad_request(format!("this deletes {what}; run again with --yes")));
            }
            c.delete(&id)?;
            let summary = if series { format!("deleted the series {id} belongs to") } else { format!("deleted {id}") };
            let mut out = Out::new(json!({ "id": id, "series": series }), summary);
            out.ids = vec![id];
            Ok(out)
        }
    }
}
