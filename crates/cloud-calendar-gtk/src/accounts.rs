//! The Accounts window: link iCloud (icloud-session's sign-in), Google or HEY (the browser), sign
//! in to one again, or unlink one. The same steps as `cloud-calendar account …` (`cloud_calendar_api::accounts`), run off the main thread.

use cloud_calendar_api::{self as api, accounts::Link};
use gtk::{gdk, gio, glib, prelude::*};
use std::cell::Cell;
use std::rc::Rc;

use crate::ui::Ui;

struct Win {
    window: gtk::Window,
    list: gtk::Box,
    /// The add buttons, off while a step runs.
    add_row: gtk::Box,
    status: gtk::Label,
    ui: Rc<Ui>,
}

fn text(s: &str, class: &str) -> gtk::Label {
    let l = gtk::Label::new(Some(s));
    l.add_css_class(class);
    l.set_xalign(0.0);
    l.set_wrap(true);
    l.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    l
}

impl Win {
    /// Runs an account step off the main thread; shows its outcome and reads the accounts again.
    fn run(self: &Rc<Self>, busy: &str, job: impl FnOnce() -> api::Result<String> + Send + 'static) {
        self.status.remove_css_class("error");
        self.status.set_text(busy);
        self.list.set_sensitive(false);
        self.add_row.set_sensitive(false);
        let win = self.clone();
        glib::MainContext::default().spawn_local(async move {
            let result = gio::spawn_blocking(job).await.unwrap_or_else(|_| Err(api::Error::new(api::ErrorKind::AccountUnavailable, "failed unexpectedly")));
            win.list.set_sensitive(true);
            win.add_row.set_sensitive(true);
            match result {
                Ok(note) => win.status.set_text(&note),
                Err(e) => {
                    win.status.add_css_class("error");
                    win.status.set_text(&e.message);
                }
            }
            win.ui.accounts_changed();
            win.refresh();
        });
    }

    /// Lists the linked accounts with whether each works.
    fn refresh(self: &Rc<Self>) {
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        self.list.append(&text("Checking accounts…", "dim"));
        let win = self.clone();
        glib::MainContext::default().spawn_local(async move {
            let read = gio::spawn_blocking(|| api::config::load().map(|c| api::Calendars::from_config(&c)).map(|c| (c.statuses(), c.broken))).await;
            while let Some(child) = win.list.first_child() {
                win.list.remove(&child);
            }
            let (statuses, broken) = match read {
                Ok(Ok(r)) => r,
                Ok(Err(e)) => return win.list.append(&text(&e.message, "error")),
                Err(_) => return,
            };
            if statuses.is_empty() && broken.is_empty() {
                win.list.append(&text("No accounts linked yet.", "dim"));
            }
            for s in statuses {
                let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
                let about = gtk::Box::new(gtk::Orientation::Vertical, 2);
                about.set_hexpand(true);
                about.append(&text(&format!("{} {} ({})", if s.ok { "✓" } else { "✗" }, s.label, s.name), "title"));
                about.append(&text(&s.detail, if s.ok { "dim" } else { "error" }));
                row.append(&about);
                let again = gtk::Button::with_label("Sign in again");
                let remove = gtk::Button::with_label("Remove");
                remove.add_css_class("destructive");
                let name = s.name.clone();
                again.connect_clicked(glib::clone!(#[strong] win, #[strong] name, move |_| {
                    let name = name.clone();
                    win.run("Signing in… (finish in the window or browser that opened)", move || api::accounts::login(&name));
                }));
                let confirming = Cell::new(false);
                remove.connect_clicked(glib::clone!(#[strong] win, move |b| {
                    if !confirming.replace(true) {
                        b.set_label("Really remove?");
                        return;
                    }
                    let name = name.clone();
                    win.run("Removing…", move || api::accounts::remove(&name));
                }));
                row.append(&again);
                row.append(&remove);
                win.list.append(&row);
            }
            for b in broken {
                win.list.append(&text(&format!("✗ {}: {}", b.account, b.message), "error"));
            }
        });
    }
}

pub fn open(ui: &Rc<Ui>) {
    let window = gtk::Window::builder().title("Accounts").transient_for(&ui.window).modal(true).default_width(560).build();
    window.add_css_class("editor");
    let root = gtk::Box::new(gtk::Orientation::Vertical, 10);
    root.add_css_class("editor");
    let list = gtk::Box::new(gtk::Orientation::Vertical, 8);
    let status = text("", "note");
    let add_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let win = Rc::new(Win { window: window.clone(), list: list.clone(), add_row: add_row.clone(), status: status.clone(), ui: ui.clone() });

    root.append(&text("Linked accounts", "setup-title"));
    root.append(&list);
    root.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    root.append(&text("Add an account", "setup-title"));

    let icloud = gtk::Button::with_label("iCloud");
    let google = gtk::Button::with_label("Google");
    let hey = gtk::Button::with_label("HEY");
    for b in [icloud.upcast_ref::<gtk::Widget>(), google.upcast_ref(), hey.upcast_ref()] {
        add_row.append(b);
    }
    root.append(&add_row);
    root.append(&text("iCloud uses the sign-in icloud-session keeps for all your iCloud apps. HEY and Google open your browser.", "note"));

    root.append(&status);

    icloud.connect_clicked(glib::clone!(#[strong] win, move |_| win.run("Signing in to iCloud… (finish in the iCloud window)", || api::accounts::add(None, Link::ICloud).map(|(_, note)| note))));
    hey.connect_clicked(glib::clone!(#[strong] win, move |_| win.run("Signing in to HEY… (finish in your browser)", || api::accounts::add(None, Link::Hey { account: None }).map(|(_, note)| note))));
    google.connect_clicked(glib::clone!(#[strong] win, move |_| win.run("Signing in to Google… (finish in your browser)", || api::accounts::add(None, Link::Google).map(|(_, note)| note))));

    let close = gtk::Button::with_label("Close");
    close.set_halign(gtk::Align::End);
    close.connect_clicked(glib::clone!(#[weak] window, move |_| window.close()));
    root.append(&close);

    let keys = gtk::EventControllerKey::new();
    keys.connect_key_pressed(glib::clone!(#[weak] window, #[upgrade_or] glib::Propagation::Proceed, move |_, key, _, _| {
        if key == gdk::Key::Escape {
            window.close();
            return glib::Propagation::Stop;
        }
        glib::Propagation::Proceed
    }));
    window.add_controller(keys);
    window.set_child(Some(&root));
    win.refresh();
    win.window.present();
}
