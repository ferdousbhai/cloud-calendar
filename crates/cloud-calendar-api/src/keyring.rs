//! Secrets in the desktop keyring, the Secret Service (`org.freedesktop.secrets`: GNOME Keyring,
//! KWallet, KeePassXC), and nowhere else: no file, no environment variable. Spoken to directly
//! over D-Bus, as icloud-sessiond's `secrets.rs` does: one "plain" session (the secret crosses
//! only the local session bus) in the collection with the `default` alias.
//!
//! The only secret cloud-calendar keeps itself is a Google account's OAuth client secret. Items
//! carry `application=cloud-calendar`, `account=<account name>` and `secret=<kind>`.

use std::collections::HashMap;

use zbus::blocking::Connection;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

use crate::error::{Error, ErrorKind, Result};

const APPLICATION: &str = "cloud-calendar";
const BUS_NAME: &str = "org.freedesktop.secrets";
const SERVICE_PATH: &str = "/org/freedesktop/secrets";
const SERVICE: &str = "org.freedesktop.Secret.Service";
const COLLECTION: &str = "org.freedesktop.Secret.Collection";
const ITEM: &str = "org.freedesktop.Secret.Item";
const PROMPT: &str = "org.freedesktop.Secret.Prompt";
const PROPERTIES: &str = "org.freedesktop.DBus.Properties";
const DEFAULT_ALIAS: &str = "default";

/// The Secret Service's `(session, parameters, value, content_type)`.
type Secret = (OwnedObjectPath, Vec<u8>, Vec<u8>, String);

fn attributes(account: &str, kind: &str) -> HashMap<&'static str, String> {
    HashMap::from([("application", APPLICATION.to_string()), ("account", account.to_string()), ("secret", kind.to_string())])
}

fn keyring_error(e: zbus::Error) -> Error {
    let unavailable = matches!(&e, zbus::Error::MethodError(name, _, _) if name.as_str() == "org.freedesktop.DBus.Error.ServiceUnknown");
    if unavailable {
        return Error::new(ErrorKind::KeyringUnavailable, "no keyring is running (the Secret Service, e.g. gnome-keyring): Cloud Calendar keeps secrets only there");
    }
    Error::new(ErrorKind::KeyringUnavailable, format!("the keyring failed: {e}"))
}

struct Open {
    bus: Connection,
    session: OwnedObjectPath,
}

impl Open {
    fn new() -> zbus::Result<Open> {
        let bus = Connection::session()?;
        let (_, session): (OwnedValue, OwnedObjectPath) = bus.call_method(Some(BUS_NAME), SERVICE_PATH, Some(SERVICE), "OpenSession", &("plain", Value::from("")))?.body().deserialize()?;
        Ok(Open { bus, session })
    }

    fn call<R>(&self, path: &str, interface: &str, method: &str, body: &(impl serde::Serialize + zbus::zvariant::DynamicType)) -> zbus::Result<R>
    where
        R: for<'d> serde::Deserialize<'d> + zbus::zvariant::Type,
    {
        self.bus.call_method(Some(BUS_NAME), path, Some(interface), method, body)?.body().deserialize()
    }

    fn locked(&self, path: &str, interface: &str) -> zbus::Result<bool> {
        let value: OwnedValue = self.call(path, PROPERTIES, "Get", &(interface, "Locked"))?;
        Ok(bool::try_from(value)?)
    }

    /// Runs the prompt at `path` ("/": none) and waits for its answer.
    fn prompt(&self, path: &OwnedObjectPath) -> zbus::Result<Option<OwnedValue>> {
        if path.as_str() == "/" {
            return Ok(None);
        }
        let prompt = zbus::blocking::proxy::Builder::<zbus::blocking::Proxy<'_>>::new(&self.bus)
            .destination(BUS_NAME)?
            .path(path.as_str())?
            .interface(PROMPT)?
            .cache_properties(zbus::proxy::CacheProperties::No)
            .build()?;
        let mut completed = prompt.receive_signal("Completed")?;
        prompt.call_method("Prompt", &("",))?;
        let message = completed.next().ok_or_else(|| zbus::Error::Failure("the keyring prompt went away".into()))?;
        let (dismissed, result): (bool, OwnedValue) = message.body().deserialize()?;
        if dismissed {
            return Err(zbus::Error::Failure("the keyring prompt was dismissed".into()));
        }
        Ok(Some(result))
    }

    fn unlock(&self, path: &OwnedObjectPath) -> zbus::Result<()> {
        let (_, prompt): (Vec<OwnedObjectPath>, OwnedObjectPath) = self.call(SERVICE_PATH, SERVICE, "Unlock", &(vec![path],))?;
        self.prompt(&prompt).map(|_| ())
    }

    /// The `default` collection, made if there is none.
    fn collection(&self) -> zbus::Result<OwnedObjectPath> {
        let path: OwnedObjectPath = self.call(SERVICE_PATH, SERVICE, "ReadAlias", &(DEFAULT_ALIAS,))?;
        if path.as_str() != "/" {
            return Ok(path);
        }
        let properties = HashMap::from([("org.freedesktop.Secret.Collection.Label", Value::from("Default"))]);
        let (path, prompt): (OwnedObjectPath, OwnedObjectPath) = self.call(SERVICE_PATH, SERVICE, "CreateCollection", &(properties, DEFAULT_ALIAS))?;
        match self.prompt(&prompt)? {
            Some(made) => Ok(OwnedObjectPath::try_from(made)?),
            None => Ok(path),
        }
    }

    fn search(&self, collection: &str, attributes: &HashMap<&str, String>) -> zbus::Result<Vec<OwnedObjectPath>> {
        self.call(collection, COLLECTION, "SearchItems", &(attributes,))
    }
}

/// The secret of `kind` stored for `account`, if any.
pub fn get(account: &str, kind: &str) -> Result<Option<String>> {
    let k = Open::new().map_err(keyring_error)?;
    let found = (|| {
        for item in k.search(&k.collection()?, &attributes(account, kind))? {
            if k.locked(&item, ITEM)? {
                k.unlock(&item)?;
            }
            let (_, _, value, _): Secret = k.call(&item, ITEM, "GetSecret", &(&k.session,))?;
            if let Ok(text) = String::from_utf8(value) {
                return Ok(Some(text));
            }
        }
        Ok(None)
    })();
    found.map_err(keyring_error)
}

/// Stores (or replaces) the secret of `kind` for `account`.
pub fn set(account: &str, kind: &str, label: &str, secret: &str) -> Result<()> {
    let k = Open::new().map_err(keyring_error)?;
    (|| {
        let collection = k.collection()?;
        if k.locked(&collection, COLLECTION)? {
            k.unlock(&collection)?;
        }
        let properties = HashMap::from([("org.freedesktop.Secret.Item.Label", Value::from(label)), ("org.freedesktop.Secret.Item.Attributes", Value::from(attributes(account, kind)))]);
        let value = (&k.session, Vec::<u8>::new(), secret.as_bytes(), "text/plain");
        let (_, prompt): (OwnedObjectPath, OwnedObjectPath) = k.call(&collection, COLLECTION, "CreateItem", &(properties, value, true))?;
        k.prompt(&prompt).map(|_| ())
    })()
    .map_err(keyring_error)
}

/// Removes every secret stored for `account`; returns how many.
pub fn forget(account: &str) -> Result<usize> {
    let k = Open::new().map_err(keyring_error)?;
    (|| {
        let ours = HashMap::from([("application", APPLICATION.to_string()), ("account", account.to_string())]);
        let mut n = 0;
        for item in k.search(&k.collection()?, &ours)? {
            let prompt: OwnedObjectPath = k.call(&item, ITEM, "Delete", &())?;
            k.prompt(&prompt)?;
            n += 1;
        }
        Ok(n)
    })()
    .map_err(keyring_error)
}
