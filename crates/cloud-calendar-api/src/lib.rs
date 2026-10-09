//! Calendar providers (iCloud over CalDAV, Google through `gws`, HEY through `hey`), the merged
//! view across them, config and notifications, shared by the cloud-calendar CLI and GTK app.

pub mod caldav;
pub mod config;
pub mod error;
pub mod google;
pub mod hey;
pub mod ics;
pub mod notify;
pub mod provider;
pub mod secret;
pub mod time;
pub mod types;
pub mod unified;

pub use config::Config;
pub use error::{Error, ErrorKind, Result};
pub use provider::{AccountStatus, AccountWarning, Provider};
pub use types::*;
pub use unified::{Calendars, Listing};
