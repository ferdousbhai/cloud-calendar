//! Passwords in the desktop keyring (the Secret Service: GNOME Keyring, KWallet, KeePassXC…)
//! through libsecret's `secret-tool`, so no password is ever written to a file of ours.
//! `CLOUD_CALENDAR_SECRET_TOOL` points at another program with the same interface (tests use a fake).

use std::process::Command;
use std::time::Duration;

use crate::error::{Error, ErrorKind, Result};
use crate::provider::{Run, run_command};

pub const COMMAND_ENV: &str = "CLOUD_CALENDAR_SECRET_TOOL";
const TIMEOUT: Duration = Duration::from_secs(30);

fn command() -> String {
    std::env::var(COMMAND_ENV).ok().filter(|c| !c.trim().is_empty()).unwrap_or_else(|| "secret-tool".into())
}

fn attributes<'a>(account: &'a str, username: &'a str) -> [&'a str; 6] {
    ["service", "cloud-calendar", "account", account, "username", username]
}

fn run(args: &[&str], stdin: Option<&str>) -> Result<Vec<u8>> {
    let program = command();
    match run_command(Command::new(&program).args(args), stdin, TIMEOUT) {
        Run::Done { status, stdout, .. } if status.success() => Ok(stdout),
        // `lookup` exits 1 with nothing on stdout when there is no such secret.
        Run::Done { status, stderr, .. } if args.first() == Some(&"lookup") && status.code() == Some(1) && stderr.is_empty() => Ok(Vec::new()),
        Run::Done { status, stderr, .. } => Err(Error::new(ErrorKind::AccountUnavailable, format!("the keyring refused `{program} {}` ({status}): {stderr}", args[0]))),
        Run::Missing => Err(Error::new(ErrorKind::AccountUnavailable, format!("`{program}` isn't installed; it comes with libsecret (`pacman -S libsecret`) and needs a keyring such as gnome-keyring"))),
        Run::TimedOut => Err(Error::new(ErrorKind::AccountUnavailable, "the keyring didn't answer (is it locked, or is no keyring running?)")),
        Run::Failed(e) => Err(Error::new(ErrorKind::AccountUnavailable, format!("could not run {program}: {e}"))),
    }
}

pub fn store(account: &str, username: &str, label: &str, password: &str) -> Result<()> {
    let mut args = vec!["store", "--label", label];
    args.extend(attributes(account, username));
    run(&args, Some(password)).map(|_| ())
}

/// The stored password, or None when there is none.
pub fn lookup(account: &str, username: &str) -> Result<Option<String>> {
    let mut args = vec!["lookup"];
    args.extend(attributes(account, username));
    let out = run(&args, None)?;
    let password = String::from_utf8_lossy(&out).trim_end_matches(['\n', '\r']).to_string();
    Ok(Some(password).filter(|p| !p.is_empty()))
}

pub fn clear(account: &str) -> Result<()> {
    run(&["clear", "service", "cloud-calendar", "account", account], None).map(|_| ())
}
