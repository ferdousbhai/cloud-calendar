//! Every linked account as one calendar. An account's failure never fails a listing: it comes
//! back as a warning next to what the other accounts returned.

use std::sync::Arc;

use crate::config::Config;
use crate::error::{Error, ErrorKind, Result};
use crate::provider::{self, AccountStatus, AccountWarning, Provider, SeriesSupport};
use crate::types::*;

#[derive(Clone)]
pub struct Calendars {
    pub accounts: Vec<Arc<dyn Provider>>,
    /// Configured accounts that couldn't be opened (unknown provider, …).
    pub broken: Vec<AccountWarning>,
}

#[derive(Debug, Clone)]
pub struct Listing<T> {
    pub items: Vec<T>,
    pub warnings: Vec<AccountWarning>,
}

impl<T> Default for Listing<T> {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            warnings: Vec::new(),
        }
    }
}

impl Calendars {
    pub fn new(accounts: Vec<Arc<dyn Provider>>) -> Self {
        Self {
            accounts,
            broken: Vec::new(),
        }
    }

    pub fn from_config(config: &Config) -> Self {
        let mut out = Self::new(Vec::new());
        for (name, cfg) in &config.accounts {
            match provider::open(name, cfg) {
                Ok(p) => out.accounts.push(p),
                Err(e) => out.broken.push(AccountWarning::new(name, &e)),
            }
        }
        out
    }

    pub fn is_empty(&self) -> bool {
        self.accounts.is_empty()
    }

    /// The account an ID belongs to.
    pub fn account_for(&self, id: &str) -> Result<&dyn Provider> {
        self.accounts
            .iter()
            .find(|p| p.owns(id))
            .map(|p| p.as_ref())
            .ok_or_else(|| {
                let account = id.split(':').next().unwrap_or("");
                Error::new(
                    ErrorKind::NotFound,
                    format!("no linked account \"{account}\" for {id} (see `cloud-calendar account list`)"),
                )
            })
    }

    fn each<T: Send>(&self, f: impl Fn(&dyn Provider) -> Result<Vec<T>> + Sync) -> Listing<T> {
        let results = crate::concurrency::map(&self.accounts, |p| f(p.as_ref()));
        let mut out = Listing {
            items: Vec::new(),
            warnings: self.broken.clone(),
        };
        for (account, result) in self.accounts.iter().zip(results) {
            let name = account.name();
            let result = result.unwrap_or_else(|_| {
                Err(Error::new(
                    ErrorKind::AccountUnavailable,
                    format!("{name}: failed unexpectedly"),
                ))
            });
            match result {
                Ok(items) => out.items.extend(items),
                Err(e) => out.warnings.push(AccountWarning::new(name, &e)),
            }
        }
        out
    }

    pub fn statuses(&self) -> Vec<AccountStatus> {
        crate::concurrency::map(&self.accounts, |p| p.status())
            .into_iter()
            .filter_map(|r| r.ok())
            .collect()
    }

    pub fn calendars(&self) -> Listing<Calendar> {
        let mut l = self.each(|p| p.calendars());
        l.items
            .sort_by_cached_key(|c| (c.account.clone(), c.name.to_lowercase()));
        l
    }

    /// Events in the range across every account, in start order.
    pub fn events(&self, range: &Range) -> Listing<Event> {
        let mut l = self.each(|p| p.events(range));
        l.items.sort_by_cached_key(Event::sort_key);
        l
    }

    pub fn create(&self, event: &NewEvent) -> Result<String> {
        if event.title.trim().is_empty() {
            return Err(Error::bad_request("an event needs a title"));
        }
        self.account_for(&event.calendar_id)?.create(event)
    }

    /// Changes an event; returns its ID afterwards (see `Provider::update`).
    pub fn update(&self, id: &str, change: &EventChange) -> Result<String> {
        if change.title.as_deref().is_some_and(|t| t.trim().is_empty()) {
            return Err(Error::bad_request("an event's title can't be empty"));
        }
        self.account_for(id)?.update(id, change)
    }

    /// What can be done here to the repeating event `id` belongs to, and the service's name.
    pub fn series_support(&self, id: &str) -> Result<(SeriesSupport, String)> {
        let a = self.account_for(id)?;
        Ok((a.series_support(), a.label().to_string()))
    }

    /// What an edit of `id` does beyond the change asked for, if anything.
    pub fn edit_caveat(&self, id: &str) -> Option<String> {
        self.account_for(id).ok()?.edit_caveat().map(str::to_string)
    }

    pub fn delete(&self, id: &str) -> Result<()> {
        self.account_for(id)?.delete(id)
    }
}
