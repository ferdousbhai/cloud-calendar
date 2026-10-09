//! `~/.config/cloud-calendar/config.toml`: the linked accounts and notification lead times.
//!
//! ```toml
//! notify_minutes = [10]
//!
//! [accounts.icloud]
//!
//! [accounts.google]
//!
//! [accounts.hey]
//! ```

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::error::{Error, ErrorKind, Result};

/// Minutes before a timed event its notification goes out, unless the config says otherwise.
pub const DEFAULT_NOTIFY_MINUTES: &[u32] = &[10];

/// One account, `[accounts.<name>]`. The name prefixes its IDs (`icloud:…`). Nothing secret is
/// kept here: iCloud's sign-in is icloud-session's, HEY's is hey's, Google's is gws's.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
pub struct AccountConfig {
    /// Which provider serves it ("icloud", "google" or "hey"); defaults to the account's name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// HEY: the CLI's linked-account selector; default all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
}

impl AccountConfig {
    pub fn provider<'a>(&'a self, name: &'a str) -> &'a str {
        self.provider.as_deref().filter(|p| !p.is_empty()).unwrap_or(name)
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
pub struct Config {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notify_minutes: Option<Vec<u32>>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub accounts: BTreeMap<String, AccountConfig>,
}

impl Config {
    pub fn notify_minutes(&self) -> Vec<u32> {
        self.notify_minutes.clone().unwrap_or_else(|| DEFAULT_NOTIFY_MINUTES.to_vec())
    }
}

pub fn config_dir() -> PathBuf {
    dirs::config_dir().unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".config")).join("cloud-calendar")
}

pub fn state_dir() -> PathBuf {
    dirs::state_dir().unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".local/state")).join("cloud-calendar")
}

pub fn path() -> PathBuf {
    config_dir().join("config.toml")
}

/// The config, or an empty one when there is no file yet.
pub fn load() -> Result<Config> {
    read(&path())
}

pub fn read(path: &Path) -> Result<Config> {
    match std::fs::read_to_string(path) {
        Ok(text) => toml::from_str(&text).map_err(|e| Error::new(ErrorKind::Config, format!("could not parse {}: {e}", path.display()))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
        Err(e) => Err(Error::new(ErrorKind::Config, format!("could not read {}: {e}", path.display()))),
    }
}

pub fn save(config: &Config) -> Result<PathBuf> {
    let p = path();
    let text = toml::to_string(config).map_err(|e| Error::new(ErrorKind::Config, e.to_string()))?;
    write_private(&p, text.as_bytes()).map_err(|e| Error::new(ErrorKind::Config, format!("could not write {}: {e}", p.display())))?;
    Ok(p)
}

/// Loads, changes and saves the config under a lock on `config.lock` beside it, so two changes at
/// once (the app and the CLI, or two sign-ins) can't drop each other's accounts.
pub fn update<T>(change: impl FnOnce(&mut Config) -> Result<T>) -> Result<T> {
    let p = path();
    let lock_path = p.with_file_name("config.lock");
    let lock_err = |e: std::io::Error| Error::new(ErrorKind::Config, format!("could not lock {}: {e}", lock_path.display()));
    if let Some(dir) = lock_path.parent() {
        private_dir(dir).map_err(lock_err)?;
    }
    let lock = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(&lock_path).map_err(lock_err)?;
    lock.lock().map_err(lock_err)?;
    let mut config = read(&p)?;
    let out = change(&mut config)?;
    save(&config)?;
    Ok(out)
}

/// Creates a directory (and its parents) readable only by you.
pub fn private_dir(dir: &Path) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(dir)
}

/// Writes a file only you can read, replacing it in one step.
pub fn write_private(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        private_dir(dir)?;
    }
    let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
    std::io::Write::write_all(&mut opts.open(&tmp)?, contents)?;
    std::fs::rename(&tmp, path)
}
