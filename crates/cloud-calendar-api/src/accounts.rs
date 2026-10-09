//! Linking, signing in to and unlinking accounts: the same steps for the CLI and the app.
//!
//! - iCloud: the one sign-in icloud-session holds for every app. Linking asks it to sign in
//!   (its own window) when it isn't; unlinking leaves it signed in, since other apps share it.
//! - Google: `gws auth login` in the browser, with Cloud Calendar's built-in OAuth client.
//! - HEY: `hey auth login` in the browser, kept in the keyring by hey itself.

use std::sync::mpsc;
use std::time::Duration;

use crate::config::{self, AccountConfig, Config};
use crate::error::{Error, ErrorKind, Result};
use crate::google::Google;
use crate::hey::Hey;
use crate::provider;

/// How long a browser or window sign-in may take.
pub const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(600);

/// What to link.
#[derive(Debug, Clone, PartialEq)]
pub enum Link {
    ICloud,
    Google,
    Hey { account: Option<String> },
}

impl Link {
    pub fn provider(&self) -> &'static str {
        match self {
            Link::ICloud => "icloud",
            Link::Google => "google",
            Link::Hey { .. } => "hey",
        }
    }
}

fn session_error(e: icloud_session::Error) -> Error {
    crate::icloud::session_error(e, "icloud")
}

/// Asks icloud-session to sign in, unless it is, and waits until it is (or its window closes).
pub fn icloud_sign_in() -> Result<()> {
    if icloud_session::status().map_err(session_error)?.signed_in {
        return Ok(());
    }
    // Watching before asking, so the answer can't come in between.
    let watch = icloud_session::watch().map_err(session_error)?;
    icloud_session::sign_in().map_err(session_error)?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut opened = false;
        for st in watch {
            if st.signed_in {
                let _ = tx.send(Ok(()));
                return;
            }
            if st.signing_in {
                opened = true;
            } else if opened {
                let _ = tx.send(Err(Error::new(ErrorKind::AccountAuth, "iCloud: the sign-in window was closed before signing in")));
                return;
            }
        }
        let _ = tx.send(Err(Error::new(ErrorKind::AccountUnavailable, "iCloud: icloud-session went away during the sign-in")));
    });
    rx.recv_timeout(SIGN_IN_TIMEOUT).unwrap_or_else(|_| Err(Error::new(ErrorKind::AccountAuth, "iCloud: the sign-in wasn't finished within 10 minutes")))
}

fn sign_in(name: &str, cfg: &AccountConfig) -> Result<String> {
    match cfg.provider(name) {
        "icloud" => {
            icloud_sign_in()?;
            let who = icloud_session::status().map_err(session_error)?.apple_id.unwrap_or_default();
            Ok(format!("signed in to iCloud as {who}"))
        }
        "google" => Google::new(name, cfg).login().map(|_| "signed in to Google".into()),
        "hey" => {
            let hey = Hey::new(name, cfg);
            if !hey.signed_in()? {
                hey.login()?;
            }
            hey.check_keyring()?;
            Ok("signed in to HEY".into())
        }
        other => Err(Error::new(ErrorKind::Config, format!("unknown provider {other}"))),
    }
}

/// Links an account and signs in to it; returns its name and what happened.
pub fn add(name: Option<&str>, link: Link) -> Result<(String, String)> {
    let provider = link.provider();
    let name = name.map(str::to_string).unwrap_or_else(|| provider.to_string());
    if !provider::valid_name(&name) {
        return Err(Error::bad_request(format!("\"{name}\" can't be an account name: use lowercase letters, digits and dashes")));
    }
    // Checked before the sign-in, and again after it under the config lock: the sign-in can take
    // minutes, and accounts may be linked or removed meanwhile.
    let free = |config: &Config| {
        if config.accounts.contains_key(&name) {
            return Err(Error::bad_request(format!("an account named {name} is already linked")));
        }
        if provider == "icloud" && config.accounts.iter().any(|(n, c)| c.provider(n) == "icloud") {
            return Err(Error::bad_request("iCloud is already linked: icloud-session holds one iCloud sign-in for every app"));
        }
        Ok(())
    };
    free(&config::load()?)?;
    let mut cfg = AccountConfig { provider: (name != provider).then(|| provider.to_string()), ..Default::default() };
    match &link {
        Link::ICloud => {}
        // Before anything is saved: a build without a Google client can't sign in.
        Link::Google => Google::new(&name, &cfg).require_client()?,
        Link::Hey { account } => cfg.account = account.clone().filter(|a| !a.trim().is_empty()),
    }
    let note = sign_in(&name, &cfg)?;
    config::update(|config| {
        free(config)?;
        config.accounts.insert(name.clone(), cfg);
        Ok(())
    })?;
    Ok((name, note))
}

/// Signs in to a linked account again.
pub fn login(name: &str) -> Result<String> {
    let config = config::load()?;
    let cfg = config.accounts.get(name).ok_or_else(|| Error::new(ErrorKind::NotFound, format!("no account named {name}")))?;
    sign_in(name, cfg)
}

/// Unlinks an account and forgets what Cloud Calendar kept for it.
pub fn remove(name: &str) -> Result<String> {
    config::update(|config| {
        let cfg = config.accounts.remove(name).ok_or_else(|| Error::new(ErrorKind::NotFound, format!("no account named {name}")))?;
        let note = match cfg.provider(name) {
            "google" => {
                Google::new(name, &cfg).forget().map_err(|e| Error::new(ErrorKind::AccountUnavailable, format!("could not remove the Google sign-in: {e}")))?;
                "signed out of Google on this computer"
            }
            "icloud" => "iCloud stays signed in for your other apps (icloud-session)",
            _ => "hey stays signed in for its own use",
        };
        Ok(format!("unlinked {name}: {note}; its calendars are untouched"))
    })
}
