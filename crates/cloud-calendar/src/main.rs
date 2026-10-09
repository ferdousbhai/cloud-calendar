mod cli;
mod render;

use clap::Parser;
use cloud_calendar_api::{self as api, Calendars, Error, ErrorKind, EventChange, NewEvent, Range, config, provider};
use serde_json::{Value, json};
use std::io::{IsTerminal, Write};

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
        ErrorKind::Config | ErrorKind::AccountAuth | ErrorKind::KeyringUnavailable => 3,
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
            if !(1..=366).contains(&a.days) {
                return Err(Error::bad_request(format!("--days must be 1 to 366, not {}", a.days)));
            }
            let from = api::time::parse_date(&a.from, today())?;
            agenda(Range::days(from, a.days), format!("{} days from {from}", a.days))
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
            let link = match args.provider.as_str() {
                "icloud" => api::accounts::Link::ICloud,
                "google" => api::accounts::Link::Google,
                "hey" => api::accounts::Link::Hey { account: args.hey_account.clone() },
                other => {
                    let known: Vec<_> = provider::KNOWN_PROVIDERS.iter().map(|(p, d)| format!("{p} ({d})")).collect();
                    return Err(Error::bad_request(format!("unknown provider \"{other}\"; one of: {}", known.join(", "))));
                }
            };
            let (name, note) = api::accounts::add(args.name.as_deref(), link)?;
            Ok(Out::new(json!({ "name": name, "config": config::path() }), format!("linked {name}: {note}")))
        }
        AccountCommand::Login { name } => {
            let note = api::accounts::login(&name)?;
            Ok(Out::new(json!({ "name": name }), note))
        }
        AccountCommand::Remove { name, yes } => {
            if !yes {
                return Err(Error::bad_request(format!("removing {name} forgets what Cloud Calendar kept for it; run again with --yes")));
            }
            let note = api::accounts::remove(&name)?;
            Ok(Out::new(json!({ "name": name }), note))
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
            if series {
                let (support, label) = c.series_support(&id)?;
                if !support.delete {
                    return Err(Error::bad_request(format!("{id} is a repeating {label} event, which Cloud Calendar can't delete; delete it in {label}")));
                }
            }
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
