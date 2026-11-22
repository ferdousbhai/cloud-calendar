//! The main window: a week (Monday to Sunday) or a two-week agenda across every linked account.
//! Keys: h/← and l/→ move a week, t goes to today, a switches week and agenda, n adds an event,
//! r reloads, ? and F1 list every shortcut; none of the single keys acts while a text field has the keyboard. Ctrl+W
//! closes a window and Ctrl+Q quits, from any window (README.md lists them all, and `shortcuts` is the overlay's list).
//!
//! Every word shown is in Omarchy's language and follows a switch: what the toolbar says is bound to it, and the week
//! or agenda, the period and the status lines are built again in the new language.

use chrono::{Datelike, Local, NaiveDate};
use cloud_calendar_api::{self as api, Calendar, Calendars, Event, Range, Time};
use gtk::{gio, glib, prelude::*};
use omui::i18n;
use omui::icons::name as icon;
use omui::tr;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use crate::editor;

const AGENDA_DAYS: u32 = 14;

/// How long a deleted event can be brought back before its delete is sent.
const UNDO_SECONDS: u32 = 7;

struct State {
    /// A day in the week shown, or the agenda's first day.
    anchor: NaiveDate,
    agenda: bool,
    /// Bumped on every load, so a slow answer for an old week is dropped.
    generation: u64,
}

pub struct Ui {
    pub window: gtk::ApplicationWindow,
    /// The window's content under omui's overlay, which shows the toasts.
    shell: Rc<omui::Shell>,
    period: gtk::Label,
    warning: gtk::Label,
    content: gtk::ScrolledWindow,
    agenda: gtk::ToggleButton,
    search: gtk::SearchEntry,
    search_hint: gtk::Label,
    events: RefCell<Vec<Event>>,
    /// The events the view shows now (the period's, or the search's matches), to draw again after a language switch.
    shown: RefCell<Vec<Event>>,
    /// What the view says instead of events (no accounts, loading, a period out of range), built in the language of
    /// the moment; None while it shows events.
    status: RefCell<Option<Rc<dyn Fn() -> String>>>,
    /// The line under the search: what it is doing, in the language of the moment.
    hint: Live,
    /// IDs of events deleted here, which the views leave out. One stays after its delete was sent: a read that began
    /// before the account had it may still carry the event.
    deleted: RefCell<Vec<String>>,
    /// Deleted events (ID and title) whose delete is not sent yet, while their toast offers Undo.
    unsent: RefCell<Vec<(String, String)>>,
    /// Set while `quit` closes the other windows, so one that asks before closing knows to go on quitting.
    quitting: Cell<bool>,
    search_generation: Cell<u64>,
    state: RefCell<State>,
    /// The linked accounts, read again after any account change.
    pub calendars: RefCell<Option<Calendars>>,
    setup_error: RefCell<Option<String>>,
    /// Calendars you can add events to, once read.
    pub writable: RefCell<Vec<Calendar>>,
    /// The last read of calendars answered for every account; until then each reload reads again.
    calendars_complete: Cell<bool>,
    /// Bumped on every read of calendars, so an answer for accounts since changed is dropped.
    calendars_generation: Cell<u64>,
    /// Told when `writable` changes (the open editor rebuilds its calendar list).
    pub on_writable: RefCell<Option<Box<dyn Fn()>>>,
}

pub fn monday(d: NaiveDate) -> NaiveDate {
    d - chrono::Days::new(u64::from(d.weekday().num_days_from_monday()))
}

fn clock(t: &Time) -> String {
    match t {
        Time::At(t) => glib::DateTime::from_unix_local(t.timestamp())
            .map(|when| i18n::format_time_short(&when))
            .unwrap_or_default(),
        Time::Date(_) => String::new(),
    }
}

/// A day as the language writes it, by a `strftime` format: `%A` and `%B` are its names of the day and the month.
pub fn day_text(day: NaiveDate, format: &str) -> String {
    glib::DateTime::from_local(day.year(), day.month() as i32, day.day() as i32, 12, 0, 0.0)
        .map(|when| i18n::format_datetime(&when, format))
        .unwrap_or_else(|_| day.to_string())
}

/// A label whose text is built again in the new language after a switch: a status line or an error, which says
/// something the app decided once and keeps showing. `set` gives it what to say, as a function of the moment's
/// language; a message that comes from an account is English, and stays so.
#[derive(Clone)]
pub struct Live {
    pub label: gtk::Label,
    say: Rc<RefCell<Rc<dyn Fn() -> String>>>,
}

impl Live {
    pub fn new(label: gtk::Label) -> Self {
        let say: Rc<RefCell<Rc<dyn Fn() -> String>>> = Rc::new(RefCell::new(Rc::new(String::new)));
        let (weak, again) = (label.downgrade(), say.clone());
        i18n::on_change(&label, move || {
            if let Some(label) = weak.upgrade() {
                let say = again.borrow().clone();
                label.set_text(&say());
            }
        });
        Self { label, say }
    }

    pub fn set(&self, say: impl Fn() -> String + 'static) {
        self.label.set_text(&say());
        *self.say.borrow_mut() = Rc::new(say);
    }

    /// English that came from elsewhere (an account's error), shown as it is in every language.
    pub fn set_fixed(&self, text: &str) {
        let text = text.to_owned();
        self.set(move || text.clone());
    }
}

/// Every shortcut of the app, for the overlay on `?` and F1. The keys' names are not translated.
pub fn shortcuts() -> Vec<omui::ShortcutSection> {
    vec![
        omui::ShortcutSection::new(
            &tr("Calendar"),
            &[
                ("h / ←", &tr("The week before (two weeks in the agenda)")),
                ("l / →", &tr("The week after")),
                ("t", &tr("Today")),
                ("a", &tr("Switch between the week and the agenda")),
                ("n", &tr("New event")),
                ("r", &tr("Reload")),
            ],
        ),
        omui::ShortcutSection::new(
            &tr("General"),
            &[
                ("Tab", &tr("Move between the buttons, the search and the events")),
                (
                    "Enter",
                    &tr("Open the event, or press the button, that has the keyboard"),
                ),
                ("Escape", &tr("Close the editor or Accounts")),
                ("? / F1", &tr("Show or hide the shortcuts")),
                ("Ctrl+W", &tr("Close the window")),
                ("Ctrl+Q", &tr("Quit")),
            ],
        ),
    ]
}

/// The keys every window of the app shares: F1 (and in a window typed into, Ctrl+?) lists the shortcuts, Ctrl+W
/// closes `window` and Ctrl+Q quits. Both close as the window's own close button does, so an editor with changes asks
/// first; `omui::quit_keys` would close the main window under an editor that asks, so Ctrl+Q is the app's own.
pub fn window_keys(ui: &Rc<Ui>, window: &impl IsA<gtk::Window>, typed_into: bool) -> omui::Keys {
    omui::ShortcutOverlay::new(shortcuts).install(window, typed_into);
    let keys = omui::Keys::new(window.as_ref());
    let weak = window.as_ref().downgrade();
    keys.on_always("<Control>w", move || {
        if let Some(window) = weak.upgrade() {
            window.close();
        }
    });
    let ui = Rc::downgrade(ui);
    keys.on_always("<Control>q", move || {
        if let Some(ui) = ui.upgrade() {
            ui.quit();
        }
    });
    keys
}

/// omui's label, breaking inside a word too long for its line: a week's column is narrow, and a long title or path
/// would otherwise set its width.
pub fn text(text: &str, class: &str) -> gtk::Label {
    let l = omui::label(text, class);
    l.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    l
}

impl Ui {
    pub fn new(app: &gtk::Application) -> Rc<Self> {
        // The title is the app's name, a brand, the same in every language.
        let window = gtk::ApplicationWindow::builder()
            .application(app)
            .title("Cloud Calendar")
            .default_width(1200)
            .default_height(760)
            .build();

        // omui's `toolbar` spaces its children.
        let toolbar = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        toolbar.add_css_class("toolbar");
        let brand = gtk::Label::new(Some("Cloud Calendar"));
        brand.add_css_class("brand");
        let prev = omui::icon_button(icon::BACK, "");
        let today = gtk::Button::new();
        let next = omui::icon_button(icon::FORWARD, "");
        i18n::bind_tooltip(&prev, "Previous (h)");
        i18n::bind_tooltip(&next, "Next (l)");
        // Right to left, the week reads from the right: the earlier week is the one to the right.
        i18n::bind(&prev, |b| {
            b.set_icon_name(if i18n::is_rtl() { icon::FORWARD } else { icon::BACK })
        });
        i18n::bind(&next, |b| {
            b.set_icon_name(if i18n::is_rtl() { icon::BACK } else { icon::FORWARD })
        });
        let period = gtk::Label::new(None);
        period.add_css_class("period");
        let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        spacer.set_hexpand(true);
        let agenda = gtk::ToggleButton::new();
        let reload = omui::icon_button(icon::REFRESH, "");
        let accounts = omui::icon_button(icon::SETTINGS, "");
        i18n::bind_tooltip(&reload, "Reload (r)");
        i18n::bind_tooltip(&accounts, "Manage accounts");
        let add = gtk::Button::new();
        add.add_css_class("suggested-action");
        for w in [
            brand.upcast_ref::<gtk::Widget>(),
            prev.upcast_ref(),
            today.upcast_ref(),
            next.upcast_ref(),
            period.upcast_ref(),
            spacer.upcast_ref(),
            agenda.upcast_ref(),
            reload.upcast_ref(),
            accounts.upcast_ref(),
            add.upcast_ref(),
        ] {
            toolbar.append(w);
        }
        // A button with words is named by them; its tooltip only adds the key.
        i18n::bind_label(&today, "Today");
        i18n::bind_label(&agenda, "Agenda");
        i18n::bind_label(&add, "New event");
        i18n::bind(&today, |b| b.set_tooltip_text(Some(&tr("Today (t)"))));
        i18n::bind(&agenda, |b| b.set_tooltip_text(Some(&tr("Week or agenda (a)"))));
        i18n::bind(&add, |b| b.set_tooltip_text(Some(&tr("New event (n)"))));

        let warning = text("", "error");
        warning.add_css_class("caption");
        warning.set_margin_start(16);
        warning.set_margin_end(16);
        warning.set_margin_bottom(4);
        warning.set_visible(false);
        let content = gtk::ScrolledWindow::new();
        content.set_vexpand(true);
        content.set_hscrollbar_policy(gtk::PolicyType::Never);

        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.append(&toolbar);
        let search = gtk::SearchEntry::builder()
            .margin_start(16)
            .margin_end(16)
            .margin_top(10)
            .margin_bottom(10)
            .build();
        i18n::bind(&search, |s| {
            s.set_placeholder_text(Some(&tr("Search this period · ~ meaning")))
        });
        root.append(&search);
        let search_hint = text("", "meta");
        search_hint.set_visible(false);
        root.append(&search_hint);
        let hint = Live::new(search_hint.clone());
        root.append(&warning);
        root.append(&content);
        let shell = omui::Shell::new(&window, &root);

        let ui = Rc::new(Self {
            window,
            shell,
            period,
            warning,
            content,
            agenda,
            search,
            search_hint,
            events: RefCell::new(Vec::new()),
            shown: RefCell::new(Vec::new()),
            status: RefCell::new(None),
            hint,
            deleted: RefCell::new(Vec::new()),
            unsent: RefCell::new(Vec::new()),
            quitting: Cell::new(false),
            search_generation: Cell::new(0),
            state: RefCell::new(State {
                anchor: Local::now().date_naive(),
                agenda: false,
                generation: 0,
            }),
            calendars: RefCell::new(None),
            setup_error: RefCell::new(None),
            writable: RefCell::new(Vec::new()),
            calendars_complete: Cell::new(false),
            calendars_generation: Cell::new(0),
            on_writable: RefCell::new(None),
        });

        ui.search.connect_search_changed(glib::clone!(
            #[weak]
            ui,
            move |_| ui.filter_events()
        ));
        prev.connect_clicked(glib::clone!(
            #[weak]
            ui,
            move |_| ui.shift(-1)
        ));
        next.connect_clicked(glib::clone!(
            #[weak]
            ui,
            move |_| ui.shift(1)
        ));
        today.connect_clicked(glib::clone!(
            #[weak]
            ui,
            move |_| ui.go_today()
        ));
        reload.connect_clicked(glib::clone!(
            #[weak]
            ui,
            move |_| ui.reload()
        ));
        add.connect_clicked(glib::clone!(
            #[weak]
            ui,
            move |_| ui.new_event(None)
        ));
        ui.agenda.connect_toggled(glib::clone!(
            #[weak]
            ui,
            move |b| {
                ui.state.borrow_mut().agenda = b.is_active();
                ui.reload();
            }
        ));

        window_keys(&ui, &ui.window, false);
        // Single keys, which do nothing while the search has the keyboard: there they are text, and an arrow moves
        // its cursor.
        let keys = omui::Keys::new(&ui.window);
        let on = |key: &str, f: fn(&Rc<Ui>)| {
            let ui = Rc::downgrade(&ui);
            keys.on(key, move || {
                if let Some(ui) = ui.upgrade() {
                    f(&ui);
                }
            });
        };
        on("h", |ui| ui.shift(-1));
        on("l", |ui| ui.shift(1));
        // The arrows point the way the week reads, which is to the left right to left.
        on("Left", |ui| ui.shift(if i18n::is_rtl() { 1 } else { -1 }));
        on("Right", |ui| ui.shift(if i18n::is_rtl() { -1 } else { 1 }));
        on("t", |ui| ui.go_today());
        on("a", |ui| ui.agenda.set_active(!ui.agenda.is_active()));
        on("n", |ui| ui.new_event(None));
        on("r", |ui| ui.reload());
        // After a switch of language, what the window built from words is built again; the theme gives events
        // without a colour of their own theirs.
        let weak = Rc::downgrade(&ui);
        i18n::on_change(&ui.window, move || {
            if let Some(ui) = weak.upgrade() {
                ui.redraw();
            }
        });
        if let Some(theme) = omui::Theme::get() {
            let weak = Rc::downgrade(&ui);
            theme.on_change(&ui.window, move |_| {
                if let Some(ui) = weak.upgrade() {
                    ui.redraw();
                }
            });
        }
        // The window is gone once it closes, and its toasts with it: what they still offered to undo is sent now.
        // `work::write` keeps the application running until each delete has finished.
        ui.window.connect_close_request(glib::clone!(
            #[weak]
            ui,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |_| {
                let unsent = ui.unsent.borrow().clone();
                for (id, _) in unsent {
                    ui.send_delete(&id);
                }
                glib::Propagation::Proceed
            }
        ));

        accounts.connect_clicked(glib::clone!(
            #[weak]
            ui,
            move |_| crate::accounts::open(&ui)
        ));
        ui.accounts_changed();
        ui
    }

    /// Reads the linked accounts again (after one was added, signed in or removed) and reloads.
    pub fn accounts_changed(self: &Rc<Self>) {
        let (calendars, error) = match api::config::load() {
            Ok(c) if c.accounts.is_empty() => (None, Some(setup_text(None))),
            Ok(c) => (Some(Calendars::from_config(&c)), None),
            Err(e) => (None, Some(setup_text(Some(&e.message)))),
        };
        *self.calendars.borrow_mut() = calendars;
        *self.setup_error.borrow_mut() = error;
        self.writable.borrow_mut().clear();
        self.calendars_complete.set(false);
        self.reload();
    }

    pub fn present(&self) {
        self.window.present();
    }

    /// Closes the editor or Accounts and then the main window, which ends the application once its writes are done.
    /// A window that does not close (an editor asking about its changes, or in the middle of a save) keeps the app
    /// open; the editor quits again itself if its changes are then discarded.
    pub fn quit(self: &Rc<Self>) {
        let others: Vec<gtk::Window> = gtk::Window::list_toplevels()
            .into_iter()
            .filter_map(|w| w.downcast::<gtk::Window>().ok())
            .filter(|w| w.transient_for().is_some_and(|parent| parent == self.window))
            .collect();
        self.quitting.set(true);
        for window in &others {
            window.close();
        }
        self.quitting.set(false);
        if others.iter().all(|window| !window.is_visible()) {
            self.window.close();
        }
    }

    /// Whether `quit` is closing the windows right now.
    pub fn is_quitting(&self) -> bool {
        self.quitting.get()
    }

    /// Takes `event` out of the views and offers to bring it back; its delete is sent when the offer has gone,
    /// unless it was taken. Nothing has reached the account until then, so Undo loses nothing of the event.
    pub fn delete_later(self: &Rc<Self>, event: &Event) {
        let id = event.id.clone();
        self.deleted.borrow_mut().push(id.clone());
        self.unsent.borrow_mut().push((id.clone(), event.title.clone()));
        self.filter_events();
        let toast = omui::Toast::new(&tr!("Deleted “{}”", event.title))
            .seconds(UNDO_SECONDS)
            .action(
                &tr("Undo"),
                glib::clone!(
                    #[weak(rename_to = ui)]
                    self,
                    #[strong]
                    id,
                    move || {
                        // Only while the delete is unsent: once it is on its way there is nothing to take back.
                        let before = ui.unsent.borrow().len();
                        ui.unsent.borrow_mut().retain(|(unsent, _)| *unsent != id);
                        if ui.unsent.borrow().len() != before {
                            ui.deleted.borrow_mut().retain(|deleted| *deleted != id);
                            ui.filter_events();
                        }
                    }
                ),
            )
            .on_dismissed(glib::clone!(
                #[weak(rename_to = ui)]
                self,
                move || ui.send_delete(&id)
            ));
        self.shell.add_toast(toast);
    }

    /// Sends the delete of an event `delete_later` took out, once: a second call for the same event, or one after
    /// Undo, does nothing. A delete that fails is said, and the event is shown again.
    fn send_delete(self: &Rc<Self>, id: &str) {
        let title = {
            let mut unsent = self.unsent.borrow_mut();
            let Some(at) = unsent.iter().position(|(unsent, _)| unsent == id) else {
                return;
            };
            unsent.remove(at).1
        };
        // The accounts as they are now: one may have been removed since.
        let cals = self.calendars.borrow().clone();
        let (ui, id) = (self.clone(), id.to_owned());
        crate::work::write(
            {
                let id = id.clone();
                move || match cals {
                    Some(cals) => cals.delete(&id),
                    None => Err(api::Error::bad_request(tr("no accounts are linked any more"))),
                }
            },
            move |result| {
                let Err(e) = result else { return };
                let problem = tr!("Couldn't delete “{}”: {}", title, e.message);
                // With the window closed there is no toast to read; the terminal or the journal has it.
                if !ui.window.is_visible() {
                    eprintln!("cloud-calendar: {problem}");
                }
                ui.shell.toast(&problem);
                ui.deleted.borrow_mut().retain(|deleted| *deleted != id);
                ui.filter_events();
            },
        );
    }

    fn shift(self: &Rc<Self>, by: i64) {
        {
            let mut s = self.state.borrow_mut();
            let days = if s.agenda { i64::from(AGENDA_DAYS) } else { 7 };
            // Stays within the years every calendar here can hold.
            if let Some(a) = chrono::Duration::try_days(by * days)
                .and_then(|d| s.anchor.checked_add_signed(d))
                .and_then(|a| api::time::bounded(a, "").ok())
            {
                s.anchor = a;
            }
        }
        self.reload();
    }

    fn go_today(self: &Rc<Self>) {
        self.state.borrow_mut().anchor = Local::now().date_naive();
        self.reload();
    }

    fn new_event(self: &Rc<Self>, day: Option<NaiveDate>) {
        if self.calendars.borrow().is_some() {
            let day = day.unwrap_or_else(|| {
                let today = Local::now().date_naive();
                let s = self.state.borrow();
                if s.agenda || monday(s.anchor) == monday(today) {
                    today.max(s.anchor)
                } else {
                    monday(s.anchor)
                }
            });
            editor::open(self, None, day);
        }
    }

    fn range(&self) -> api::Result<Range> {
        let s = self.state.borrow();
        if s.agenda {
            Range::days(s.anchor, AGENDA_DAYS)
        } else {
            Range::days(monday(s.anchor), 7)
        }
    }

    /// Reads the calendars you can add to again; an open editor's list follows.
    pub fn load_calendars(self: &Rc<Self>) {
        let Some(cals) = self.calendars.borrow().clone() else {
            return;
        };
        let generation = self.calendars_generation.get() + 1;
        self.calendars_generation.set(generation);
        let ui = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let Ok(listing) = gio::spawn_blocking(move || cals.calendars()).await else {
                return;
            };
            let Some(ui) = ui.upgrade() else { return };
            if ui.calendars_generation.get() != generation {
                return;
            }
            ui.calendars_complete.set(listing.warnings.is_empty());
            *ui.writable.borrow_mut() = listing.items.into_iter().filter(|c| c.writable).collect();
            if let Some(f) = ui.on_writable.borrow().as_ref() {
                f();
            }
        });
    }

    /// Reads the shown week (or agenda) again from every account, and the calendars too until a
    /// read of them answered for every account.
    pub fn reload(self: &Rc<Self>) {
        self.events.borrow_mut().clear();
        self.search_generation.set(self.search_generation.get() + 1);
        if self.search.text().starts_with('~') {
            self.show_filtered(&[]);
            self.hint.set(|| tr("Refreshing this period…"));
        }
        if !self.calendars_complete.get() {
            self.load_calendars();
        }
        let range = match self.range() {
            Ok(r) => r,
            Err(e) => return self.show_status(move || e.message.clone()),
        };
        self.period.set_text(&self.period_text());
        let Some(cals) = self.calendars.borrow().clone() else {
            let error = self.setup_error.borrow().clone();
            return self.show_status(move || setup_text(error.as_deref()));
        };
        let generation = {
            let mut s = self.state.borrow_mut();
            s.generation += 1;
            s.generation
        };
        if self.content.child().is_none() {
            self.show_status(|| tr("Loading…"));
        }
        let ui = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let listing = gio::spawn_blocking(move || cals.events(&range)).await;
            let Some(ui) = ui.upgrade() else { return };
            if ui.state.borrow().generation != generation {
                return;
            }
            let listing = listing.unwrap_or_default();
            let warnings: Vec<String> = listing.warnings.iter().map(|w| w.message.clone()).collect();
            ui.warning.set_text(&warnings.join("\n"));
            ui.warning.set_visible(!warnings.is_empty());
            *ui.events.borrow_mut() = listing.items;
            ui.filter_events();
        });
    }

    /// The period shown, as the language writes its days and months.
    fn period_text(&self) -> String {
        let (agenda, anchor) = {
            let s = self.state.borrow();
            (s.agenda, s.anchor)
        };
        let first = if agenda { anchor } else { monday(anchor) };
        let days = if agenda { AGENDA_DAYS - 1 } else { 6 };
        let last = first + chrono::Days::new(u64::from(days));
        tr!("{} – {}", day_text(first, "%-d %b"), day_text(last, "%-d %b %Y"))
    }

    /// One line where the week would be, said again in the new language after a switch.
    fn show_status(&self, say: impl Fn() -> String + 'static) {
        let line = text(&say(), "status-line");
        line.set_selectable(true);
        self.content.set_child(Some(&line));
        self.status.replace(Some(Rc::new(say)));
    }

    /// Builds again, in the language and the theme of the moment, everything the window made of words and colours:
    /// the period, and the week, the agenda or the line shown in their place.
    fn redraw(self: &Rc<Self>) {
        self.period.set_text(&self.period_text());
        let status = self.status.borrow().clone();
        match status {
            Some(say) => self.show_status(move || say()),
            None => {
                let shown = self.shown.borrow().clone();
                self.show_filtered(&shown);
            }
        }
    }

    fn filter_events(self: &Rc<Self>) {
        let raw = self.search.text().to_string();
        let semantic = raw.starts_with('~');
        let query = raw.trim_start_matches('~').trim().to_string();
        let mut events = self.events.borrow().clone();
        events.retain(|event| !self.deleted.borrow().contains(&event.id));
        let generation = self.search_generation.get() + 1;
        self.search_generation.set(generation);
        let matching: Vec<_> = events
            .iter()
            .filter(|event| {
                query.is_empty()
                    || serde_json::to_string(event)
                        .unwrap_or_default()
                        .to_lowercase()
                        .contains(&query.to_lowercase())
            })
            .cloned()
            .collect();
        self.show_filtered(&matching);
        self.search_hint.set_visible(semantic && !query.is_empty());
        if !semantic || query.is_empty() {
            return;
        }
        self.hint.set(|| tr("Searching this period with local Recall…"));
        let records: Vec<_> = events.iter().map(|event| serde_json::json!({"id":event.id,"title":event.title,"text":serde_json::to_string(event).unwrap_or_default().chars().take(4000).collect::<String>()})).collect();
        let ui = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let ranked =
                gio::spawn_blocking(move || crate::recall_client::rank("cloud-calendar", &query, &records, 10))
                    .await
                    .ok()
                    .flatten();
            let Some(ui) = ui.upgrade() else { return };
            if ui.search_generation.get() != generation {
                return;
            }
            match ranked {
                Some(ids) => {
                    let hits: Vec<_> = ids
                        .iter()
                        .filter_map(|id| events.iter().find(|event| &event.id == id).cloned())
                        .collect();
                    ui.show_filtered(&hits);
                    ui.hint.set(|| tr("Semantic matches in this period · local Recall"));
                }
                None => ui
                    .hint
                    .set(|| tr("Recall unavailable or warming up · showing text matches")),
            }
        });
    }
    fn show_filtered(self: &Rc<Self>, events: &[Event]) {
        self.status.replace(None);
        self.shown.replace(events.to_vec());
        if self.state.borrow().agenda {
            if let Ok(range) = self.range() {
                self.show_agenda(events, &range);
            }
        } else {
            self.show_week(events);
        }
    }

    fn event_button(self: &Rc<Self>, e: &Event, show_day: bool) -> gtk::Button {
        let b = gtk::Button::new();
        b.add_css_class("event");
        let body = gtk::Box::new(gtk::Orientation::Vertical, 1);
        // The calendar's own colour, or the theme's dimmed text for one without.
        let dot = e
            .color
            .clone()
            .filter(|c| c.starts_with('#') && c.len() <= 9)
            .unwrap_or_else(|| {
                omui::Theme::get()
                    .map(|theme| theme.tokens())
                    .unwrap_or_else(omui::Tokens::fallback)
                    .dim_text
            });
        let title = text("", "");
        title.set_markup(&format!(
            "<span foreground=\"{dot}\">●</span>\u{a0}{}{}",
            glib::markup_escape_text(&e.title),
            if e.recurring { " ↻" } else { "" }
        ));
        body.append(&title);
        let mut when = if e.all_day {
            let last = e.end.local_date() - chrono::Days::new(1);
            if last > e.start.local_date() {
                tr!("all day, until {}", day_text(last, "%a %-d"))
            } else {
                tr("all day")
            }
        } else {
            format!("{}–{}", clock(&e.start), clock(&e.end))
        };
        if show_day {
            when = format!("{when} · {} ({})", e.calendar, e.account);
        }
        body.append(&text(&when, "meta"));
        if let Some(l) = &e.location {
            body.append(&text(l, "meta"));
        }
        b.set_child(Some(&body));
        b.set_tooltip_text(Some(&format!("{} · {} ({})", e.title, e.calendar, e.account)));
        let event = e.clone();
        b.connect_clicked(glib::clone!(
            #[weak(rename_to = ui)]
            self,
            move |_| editor::open(&ui, Some(event.clone()), event.start.local_date())
        ));
        b
    }

    fn show_week(self: &Rc<Self>, events: &[Event]) {
        let today = Local::now().date_naive();
        let start = monday(self.state.borrow().anchor);
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        row.set_homogeneous(true);
        for i in 0..7u64 {
            let day = start + chrono::Days::new(i);
            let col = gtk::Box::new(gtk::Orientation::Vertical, 0);
            col.add_css_class("day");
            if day == today {
                col.add_css_class("today");
            }
            let head = gtk::Button::with_label(&day_text(day, "%a %-d"));
            head.add_css_class("flat");
            head.add_css_class("day-head");
            head.set_tooltip_text(Some(&tr("New event on this day")));
            head.connect_clicked(glib::clone!(
                #[weak(rename_to = ui)]
                self,
                move |_| ui.new_event(Some(day))
            ));
            col.append(&head);
            let range = Range::day(day);
            for e in events.iter().filter(|e| range.overlaps(&e.start, &e.end)) {
                col.append(&self.event_button(e, false));
            }
            row.append(&col);
        }
        self.content.set_child(Some(&row));
    }

    fn show_agenda(self: &Rc<Self>, events: &[Event], range: &Range) {
        let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
        list.add_css_class("agenda");
        let first = range.start.with_timezone(&Local).date_naive();
        let mut any = false;
        for i in 0..u64::from(AGENDA_DAYS) {
            let Some(day) = first.checked_add_days(chrono::Days::new(i)) else {
                break;
            };
            let day_range = Range::day(day);
            let todays: Vec<&Event> = events.iter().filter(|e| day_range.overlaps(&e.start, &e.end)).collect();
            if todays.is_empty() {
                continue;
            }
            any = true;
            list.append(&text(&day_text(day, "%A %-d %B"), "agenda-day"));
            for e in todays {
                list.append(&self.event_button(e, true));
            }
        }
        if !any {
            list.append(&text(&tr("No events in these two weeks."), "status-line"));
        }
        self.content.set_child(Some(&list));
    }
}

/// What the window says before any account is linked: the config's problem, if it has one (in English, as the
/// library says it), and then what to do.
fn setup_text(error: Option<&str>) -> String {
    let todo = tr("No calendar accounts are linked yet. Add iCloud, Google or HEY under Accounts.");
    match error {
        Some(problem) => format!("{problem}\n\n{todo}"),
        None => todo,
    }
}
