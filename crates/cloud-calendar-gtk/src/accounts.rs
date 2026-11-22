//! The Accounts window: link iCloud (icloud-session's sign-in), Google or HEY (the browser), sign
//! in to one again, or unlink one. The same steps as `cloud-calendar account …` (`cloud_calendar_api::accounts`), run off the main thread.
//! What it says follows a switch of language; what an account says of itself (its state, a step's outcome) is the
//! library's English.

use cloud_calendar_api::{self as api, accounts::Link};
use gtk::{gio, glib, prelude::*};
use omui::i18n;
use omui::tr;
use std::cell::Cell;
use std::rc::Rc;

use crate::ui::{Live, Ui, text};

struct Win {
    window: gtk::Window,
    list: gtk::Box,
    /// The add buttons, off while a step runs.
    add_row: gtk::Box,
    status: Live,
    ui: Rc<Ui>,
}

impl Win {
    /// Runs an account step off the main thread; shows its outcome and reads the accounts again.
    fn run(self: &Rc<Self>, busy: fn() -> String, job: impl FnOnce() -> api::Result<String> + Send + 'static) {
        self.status.label.remove_css_class("error");
        self.status.set(busy);
        self.list.set_sensitive(false);
        self.add_row.set_sensitive(false);
        let win = self.clone();
        // Each step changes the linked accounts: the application stays until it is done, with this window closed too.
        crate::work::write(job, move |result| {
            win.list.set_sensitive(true);
            win.add_row.set_sensitive(true);
            match result {
                Ok(note) => win.status.set_fixed(&note),
                Err(e) => {
                    win.status.label.add_css_class("error");
                    win.status.set_fixed(&e.message);
                }
            }
            win.ui.accounts_changed();
            win.refresh();
        });
    }

    /// Lists the linked accounts with whether each works.
    fn refresh(self: &Rc<Self>) {
        omui::clear(&self.list);
        self.list.append(&live(|| tr("Checking accounts…"), "meta"));
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
                win.list.append(&live(|| tr("No accounts linked yet."), "meta"));
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
                i18n::bind(
                    &again,
                    glib::clone!(
                        #[strong]
                        name,
                        move |b| label(b, &tr("Sign in again"), &tr!("Sign in to {} again", name))
                    ),
                );
                // Remove asks again on itself: its second label is the question.
                let confirming = Rc::new(Cell::new(false));
                i18n::bind(
                    &remove,
                    glib::clone!(
                        #[strong]
                        name,
                        #[strong]
                        confirming,
                        move |b| match confirming.get() {
                            false => label(b, &tr("Remove"), &tr!("Remove {}", name)),
                            true => label(b, &tr("Really remove?"), &tr!("Really remove {}?", name)),
                        }
                    ),
                );
                again.connect_clicked(glib::clone!(
                    #[strong]
                    win,
                    #[strong]
                    name,
                    move |_| {
                        let name = name.clone();
                        win.run(
                            || tr("Signing in… (finish in the window or browser that opened)"),
                            move || api::accounts::login(&name),
                        );
                    }
                ));
                remove.connect_clicked(glib::clone!(
                    #[strong]
                    win,
                    move |b| {
                        if !confirming.replace(true) {
                            label(b, &tr("Really remove?"), &tr!("Really remove {}?", name));
                            return;
                        }
                        let name = name.clone();
                        win.run(|| tr("Removing…"), move || api::accounts::remove(&name));
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

/// A line of text said again in the new language after a switch.
fn live(say: fn() -> String, class: &str) -> gtk::Label {
    let l = text("", class);
    i18n::bind(&l, move |l| l.set_text(&say()));
    l
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
        .transient_for(&ui.window)
        .modal(true)
        .default_width(560)
        .build();
    i18n::bind(&window, |w| w.set_title(Some(&tr("Accounts"))));
    // Accounts is kept out of the application, so omui names it `cloud-calendar` only when asked to.
    omui::name_window(&window);
    let root = gtk::Box::new(gtk::Orientation::Vertical, 10);
    root.add_css_class("sheet");
    let list = gtk::Box::new(gtk::Orientation::Vertical, 8);
    let status = text("", "meta");
    let add_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let win = Rc::new(Win {
        window: window.clone(),
        list: list.clone(),
        add_row: add_row.clone(),
        status: Live::new(status.clone()),
        ui: ui.clone(),
    });

    root.append(&live(|| tr("Linked accounts"), "section-title"));
    root.append(&list);
    root.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    root.append(&live(|| tr("Add an account"), "section-title"));

    // The providers' names are brands, the same in every language.
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
    root.append(&live(
        || tr("iCloud uses the sign-in icloud-session keeps for all your iCloud apps. HEY and Google open your browser."),
        "meta",
    ));

    root.append(&status);

    icloud.connect_clicked(glib::clone!(
        #[strong]
        win,
        move |_| win.run(
            || tr("Signing in to iCloud… (finish in the iCloud window)"),
            || { api::accounts::add(None, Link::ICloud).map(|(_, note)| note) }
        )
    ));
    hey.connect_clicked(glib::clone!(
        #[strong]
        win,
        move |_| win.run(
            || tr("Signing in to HEY… (finish in your browser)"),
            || api::accounts::add(None, Link::Hey { account: None }).map(|(_, note)| note)
        )
    ));
    google.connect_clicked(glib::clone!(
        #[strong]
        win,
        move |_| win.run(
            || tr("Signing in to Google… (finish in your browser)"),
            || { api::accounts::add(None, Link::Google).map(|(_, note)| note) }
        )
    ));

    let close = gtk::Button::new();
    i18n::bind_label(&close, "Close");
    close.set_halign(gtk::Align::End);
    close.connect_clicked(glib::clone!(
        #[weak]
        window,
        move |_| window.close()
    ));
    root.append(&close);

    // Its keys are the editor's: Escape closes it, and its shortcuts are on F1 and Ctrl+?.
    crate::ui::window_keys(ui, &window, true).on_always(
        "Escape",
        glib::clone!(
            #[weak]
            window,
            move || window.close()
        ),
    );
    window.set_child(Some(&root));
    win.refresh();
    win.window.present();
}
