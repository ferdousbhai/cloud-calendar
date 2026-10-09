use clap::{Args, Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(
    name = "cloud-calendar",
    version,
    about = "Your iCloud, Google and HEY calendars in one place, from the terminal",
    long_about = "cloud-calendar reads and changes events across linked calendar accounts: iCloud (through icloud-session), Google (through `gws`) and HEY (through `hey`).\n\
                  Output is human-readable on a terminal and a JSON envelope {ok, data, summary, meta} when piped.\n\
                  Exit codes: 0 ok, 1 other failure, 2 invalid request, 3 not signed in, not configured or no keyring, 4 not found, 5 an account is unreachable.",
    disable_help_subcommand = true
)]
pub struct Cli {
    #[command(flatten)]
    pub global: GlobalArgs,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Args, Debug, Clone, Default)]
pub struct GlobalArgs {
    /// Output the JSON envelope {ok, data, summary, meta} (default when piped)
    #[arg(long, global = true)]
    pub json: bool,
    /// Output only the result data as JSON, without the envelope
    #[arg(long, global = true)]
    pub quiet: bool,
    /// Output only IDs, one per line
    #[arg(long, global = true)]
    pub ids_only: bool,
    /// Output only the number of results
    #[arg(long, global = true)]
    pub count: bool,
    /// Force human-readable output even when piped
    #[arg(long, global = true)]
    pub styled: bool,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Show the config file and whether each account is signed in and reachable
    Status,
    /// Link, sign in to and unlink calendar accounts
    #[command(subcommand)]
    Account(AccountCommand),
    /// List the calendars of every account
    Calendars,
    /// Events from a day on, every account merged (default: today and the next 6 days)
    Agenda(AgendaArgs),
    /// Today's events
    Today,
    /// The events of the week (Monday to Sunday) a day falls in
    Week {
        /// Any day of the week (YYYY-MM-DD, today, tomorrow, +N); default today
        date: Option<String>,
    },
    /// Add, change or delete an event
    #[command(subcommand)]
    Event(EventCommand),
    /// Send notifications for events starting soon (run every minute by cloud-calendar-notify.timer)
    Notify {
        /// Read the accounts now instead of the cached next day
        #[arg(long)]
        refresh: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum AccountCommand {
    /// List linked accounts and whether they work
    List,
    /// Link an account and sign in: icloud, google or hey
    Add(AddArgs),
    /// Sign in to a linked account again
    Login { name: String },
    /// Unlink an account and forget what Cloud Calendar kept for it
    Remove {
        name: String,
        /// Confirm
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Args, Debug)]
pub struct AddArgs {
    /// icloud (icloud-session's sign-in), google or hey
    pub provider: String,
    /// The account's name, which prefixes its IDs (default: the provider)
    #[arg(long)]
    pub name: Option<String>,
    /// HEY: the hey CLI's linked-account selector
    #[arg(long)]
    pub hey_account: Option<String>,
}

#[derive(Args, Debug)]
pub struct AgendaArgs {
    /// First day (YYYY-MM-DD, today, tomorrow, +N)
    #[arg(long, default_value = "today")]
    pub from: String,
    /// How many days
    #[arg(long, default_value_t = 7)]
    pub days: u32,
}

#[derive(Subcommand, Debug)]
pub enum EventCommand {
    /// Add an event. A date alone (2026-10-09) makes it all-day; a time (2026-10-09 14:00) makes it timed.
    Add(EventAddArgs),
    /// Change an event; only the options given change. Occurrences of a repeating event change the whole series.
    Edit(EventEditArgs),
    /// Delete an event (an occurrence of a repeating event deletes the whole series)
    Delete {
        id: String,
        /// Confirm
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Args, Debug)]
pub struct EventAddArgs {
    /// Calendar ID (see `cloud-calendar calendars`)
    #[arg(long)]
    pub calendar: String,
    #[arg(long)]
    pub title: String,
    /// Start: 2026-10-09 14:00, tomorrow 09:30, 14:00 (today), or a date for all-day
    #[arg(long)]
    pub start: String,
    /// End, in the same form (default: an hour later, or one day for all-day)
    #[arg(long, conflicts_with = "length")]
    pub end: Option<String>,
    /// Length instead of an end: 30m, 1h30m, 2d
    #[arg(long)]
    pub length: Option<String>,
    #[arg(long)]
    pub location: Option<String>,
    #[arg(long)]
    pub notes: Option<String>,
}

#[derive(Args, Debug)]
pub struct EventEditArgs {
    pub id: String,
    #[arg(long)]
    pub title: Option<String>,
    /// New start (keeps the length unless --end is given)
    #[arg(long)]
    pub start: Option<String>,
    #[arg(long)]
    pub end: Option<String>,
    /// New location ("" clears it)
    #[arg(long)]
    pub location: Option<String>,
    /// New notes ("" clears them)
    #[arg(long)]
    pub notes: Option<String>,
}
