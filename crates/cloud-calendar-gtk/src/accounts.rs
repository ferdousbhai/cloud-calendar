//! The Accounts window: link iCloud (icloud-session's sign-in), Google or HEY (the browser), sign
//! in to one again, or unlink one. The same steps as `cloud-calendar account …` (`cloud_calendar_api::accounts`), run off the main thread.

use cloud_calendar_api::{self as api, accounts::Link};
use gtk::{gdk, gio, glib, prelude::*};
use std::cell::Cell;
use std::rc::Rc;

use crate::ui::{Ui, text};

struct Win {
    window: gtk::Window,
    list: gtk::Box,
    /// The add buttons, off while a step runs.
    add_row: gtk::Box,
    status: gtk::Label,
    ui: Rc<Ui>,
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
            let result = gio::spawn_blocking(job).await.unwrap_or_else(|_| {
                Err(api::Error::new(
                    api::ErrorKind::AccountUnavailable,
                    "failed unexpectedly",
                ))
            });
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
        omui::clear(&self.list);
        self.list.append(&text("Checking accounts…", "meta"));
        let win = self.clone();
        glib::MainContext::default().spawn_local(async move {
            let read = gio::spawn_blocking(|| {
                api::config::load()
                    .map(|c| api::Calendars::from_config(&c))
                    .map(|c| (c.statuses(), c.broken))
            })
            .await;
            omui::clear(&win.list);
            let (statuses, broken) = match read {
                Ok(Ok(r)) => r,
                Ok(Err(e)) => return win.list.append(&text(&e.message, "error")),
                Err(_) => return,
            };
            if statuses.is_empty() && broken.is_empty() {
                win.list.append(&text("No accounts linked yet.", "meta"));
            }
            for s in statuses {
                let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
                let about = gtk::Box::new(gtk::Orientation::Vertical, 2);
                about.set_hexpand(true);
                about.append(&text(
                    &format!("{} {} ({})", if s.ok { "✓" } else { "✗" }, s.label, s.name),
                    "",
                ));
                about.append(&text(&s.detail, if s.ok { "meta" } else { "error" }));
                row.append(&about);
                let (again, remove) = (gtk::Button::new(), gtk::Button::new());
                remove.add_css_class("destructive-action");
                // Each row has the same two buttons; a screen reader hears whose they are.
                let name = s.name.clone();
                label(&again, "Sign in again", &format!("Sign in to {name} again"));
                label(&remove, "Remove", &format!("Remove {name}"));
                again.connect_clicked(glib::clone!(
                    #[strong]
                    win,
                    #[strong]
                    name,
                    move |_| {
                        let name = name.clone();
                        win.run(
                            "Signing in… (finish in the window or browser that opened)",
                            move || api::accounts::login(&name),
                        );
                    }
                ));
                let confirming = Cell::new(false);
                remove.connect_clicked(glib::clone!(
                    #[strong]
                    win,
                    move |b| {
                        if !confirming.replace(true) {
                            label(b, "Really remove?", &format!("Really remove {name}?"));
                            return;
                        }
                        let name = name.clone();
                        win.run("Removing…", move || api::accounts::remove(&name));
                    }
                ));
                for button in [&again, &remove] {
                    // Beside several lines of detail, a button keeps its own height.
                    button.set_valign(gtk::Align::Center);
                    row.append(button);
                }
                win.list.append(&row);
            }
            for b in broken {
                win.list
                    .append(&text(&format!("✗ {}: {}", b.account, b.message), "error"));
            }
        });
    }
}

/// Gives `button` the text it shows and the name a screen reader says. GTK names a button by the label inside it
/// (`set_label` relates the two) before any name it is given, so that relation goes each time the text is set.
fn label(button: &gtk::Button, shown: &str, name: &str) {
    button.set_label(shown);
    button.reset_relation(gtk::AccessibleRelation::LabelledBy);
    button.update_property(&[gtk::accessible::Property::Label(name)]);
}

pub fn open(ui: &Rc<Ui>) {
    let window = gtk::Window::builder()
        .title("Accounts")
        .transient_for(&ui.window)
        .modal(true)
        .default_width(560)
        .build();
    let root = gtk::Box::new(gtk::Orientation::Vertical, 10);
    root.add_css_class("sheet");
    let list = gtk::Box::new(gtk::Orientation::Vertical, 8);
    let status = text("", "meta");
    let add_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let win = Rc::new(Win {
        window: window.clone(),
        list: list.clone(),
        add_row: add_row.clone(),
        status: status.clone(),
        ui: ui.clone(),
    });

    root.append(&text("Linked accounts", "section-title"));
    root.append(&list);
    root.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    root.append(&text("Add an account", "section-title"));

    let icloud = gtk::Button::with_label("iCloud");
    let google = gtk::Button::with_label("Google");
    let hey = gtk::Button::with_label("HEY");
    for b in [
        icloud.upcast_ref::<gtk::Widget>(),
        google.upcast_ref(),
        hey.upcast_ref(),
    ] {
        add_row.append(b);
    }
    root.append(&add_row);
    root.append(&text(
        "iCloud uses the sign-in icloud-session keeps for all your iCloud apps. HEY and Google open your browser.",
        "meta",
    ));

    root.append(&status);

    icloud.connect_clicked(glib::clone!(
        #[strong]
        win,
        move |_| win.run("Signing in to iCloud… (finish in the iCloud window)", || {
            api::accounts::add(None, Link::ICloud).map(|(_, note)| note)
        })
    ));
    hey.connect_clicked(glib::clone!(
        #[strong]
        win,
        move |_| win.run("Signing in to HEY… (finish in your browser)", || api::accounts::add(
            None,
            Link::Hey { account: None }
        )
        .map(|(_, note)| note))
    ));
    google.connect_clicked(glib::clone!(
        #[strong]
        win,
        move |_| win.run("Signing in to Google… (finish in your browser)", || {
            api::accounts::add(None, Link::Google).map(|(_, note)| note)
        })
    ));

    let close = gtk::Button::with_label("Close");
    close.set_halign(gtk::Align::End);
    close.connect_clicked(glib::clone!(
        #[weak]
        window,
        move |_| window.close()
    ));
    root.append(&close);

    let keys = gtk::EventControllerKey::new();
    keys.connect_key_pressed(glib::clone!(
        #[weak]
        window,
        #[upgrade_or]
        glib::Propagation::Proceed,
        move |_, key, _, _| {
            if key == gdk::Key::Escape {
                window.close();
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        }
    ));
    window.add_controller(keys);
    window.set_child(Some(&root));
    win.refresh();
    win.window.present();
}
