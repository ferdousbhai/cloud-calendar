//! Calendar providers (iCloud through icloud-session, Google through `gws`, HEY through `hey`),
//! the merged view across them, accounts, config and notifications, shared by the cloud-calendar
//! CLI and GTK app.

pub mod accounts;
pub mod config;
pub mod error;
pub mod google;
pub mod hey;
pub mod icloud;
pub mod keyring;
pub mod notify;
pub mod provider;
pub mod time;
pub mod types;
pub mod unified;

pub use config::Config;
pub use error::{Error, ErrorKind, Result};
pub use provider::{AccountStatus, AccountWarning, Provider};
pub use types::*;
pub use unified::{Calendars, Listing};
