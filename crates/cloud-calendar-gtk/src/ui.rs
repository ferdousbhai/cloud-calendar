//! The main window: a week (Monday to Sunday) or a two-week agenda across every linked account.
//! Keys: h/← and l/→ move a week, t goes to today, a switches week and agenda, n adds an event,
//! r reloads.

use chrono::{Datelike, Local, NaiveDate};
use cloud_calendar_api::{self as api, Calendar, Calendars, Event, Range, Time};
use gtk::{gdk, gio, glib, prelude::*};
use std::cell::RefCell;
use std::rc::Rc;

use crate::{editor, theme};

const AGENDA_DAYS: u32 = 14;

struct State {
    /// A day in the week shown, or the agenda's first day.
    anchor: NaiveDate,
    agenda: bool,
    /// Bumped on every load, so a slow answer for an old week is dropped.
    generation: u64,
}

pub struct Ui {
    pub window: gtk::ApplicationWindow,
    period: gtk::Label,
    warning: gtk::Label,
    content: gtk::ScrolledWindow,
    agenda: gtk::ToggleButton,
    state: RefCell<State>,
    /// The linked accounts, read again after any account change.
    pub calendars: RefCell<Option<Calendars>>,
    setup_error: RefCell<Option<String>>,
    /// Calendars you can add events to, once read.
    pub writable: RefCell<Vec<Calendar>>,
}

pub fn monday(d: NaiveDate) -> NaiveDate {
    d - chrono::Days::new(u64::from(d.weekday().num_days_from_monday()))
}

fn clock(t: &Time) -> String {
    match t {
        Time::At(t) => t.with_timezone(&Local).format("%H:%M").to_string(),
        Time::Date(_) => String::new(),
    }
}

fn label(text: &str, class: &str) -> gtk::Label {
    let l = gtk::Label::new(Some(text));
    l.add_css_class(class);
    l.set_xalign(0.0);
    l.set_wrap(true);
    l.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    l
}

impl Ui {
    pub fn new(app: &gtk::Application) -> Rc<Self> {
        let palette = theme::load();
        theme::apply(&palette);
        theme::watch(|p| theme::apply(&p));

        let window = gtk::ApplicationWindow::builder().application(app).title("Cloud Calendar").default_width(1200).default_height(760).build();
        window.add_css_class("cloud-calendar");

        let toolbar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        toolbar.add_css_class("toolbar");
        let brand = gtk::Label::new(Some("Cloud Calendar"));
        brand.add_css_class("brand");
        let prev = gtk::Button::with_label("◀");
        let today = gtk::Button::with_label("Today");
        let next = gtk::Button::with_label("▶");
        let period = gtk::Label::new(None);
        period.add_css_class("period");
        let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        spacer.set_hexpand(true);
        let agenda = gtk::ToggleButton::with_label("Agenda");
        let reload = gtk::Button::with_label("Reload");
        let accounts = gtk::Button::with_label("Accounts");
        let add = gtk::Button::with_label("+ New event");
        add.add_css_class("suggested");
        for w in [brand.upcast_ref::<gtk::Widget>(), prev.upcast_ref(), today.upcast_ref(), next.upcast_ref(), period.upcast_ref(), spacer.upcast_ref(), agenda.upcast_ref(), reload.upcast_ref(), accounts.upcast_ref(), add.upcast_ref()] {
            toolbar.append(w);
        }
        prev.set_tooltip_text(Some("Previous (h)"));
        next.set_tooltip_text(Some("Next (l)"));
        today.set_tooltip_text(Some("Today (t)"));
        agenda.set_tooltip_text(Some("Week or agenda (a)"));
        reload.set_tooltip_text(Some("Reload (r)"));
        add.set_tooltip_text(Some("New event (n)"));

        let warning = label("", "warning");
        warning.set_visible(false);
        let content = gtk::ScrolledWindow::new();
        content.set_vexpand(true);
        content.set_hscrollbar_policy(gtk::PolicyType::Never);

        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.append(&toolbar);
        root.append(&warning);
        root.append(&content);
        window.set_child(Some(&root));

        let ui = Rc::new(Self {
            window,
            period,
            warning,
            content,
            agenda,
            state: RefCell::new(State { anchor: Local::now().date_naive(), agenda: false, generation: 0 }),
            calendars: RefCell::new(None),
            setup_error: RefCell::new(None),
            writable: RefCell::new(Vec::new()),
        });

        prev.connect_clicked(glib::clone!(#[weak] ui, move |_| ui.shift(-1)));
        next.connect_clicked(glib::clone!(#[weak] ui, move |_| ui.shift(1)));
        today.connect_clicked(glib::clone!(#[weak] ui, move |_| ui.go_today()));
        reload.connect_clicked(glib::clone!(#[weak] ui, move |_| ui.reload()));
        add.connect_clicked(glib::clone!(#[weak] ui, move |_| ui.new_event(None)));
        ui.agenda.connect_toggled(glib::clone!(#[weak] ui, move |b| {
            ui.state.borrow_mut().agenda = b.is_active();
            ui.reload();
        }));

        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed(glib::clone!(#[weak] ui, #[upgrade_or] glib::Propagation::Proceed, move |_, key, _, mods| {
            if mods.intersects(gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::ALT_MASK) {
                return glib::Propagation::Proceed;
            }
            match key {
                gdk::Key::h | gdk::Key::Left => ui.shift(-1),
                gdk::Key::l | gdk::Key::Right => ui.shift(1),
                gdk::Key::t => ui.go_today(),
                gdk::Key::a => ui.agenda.set_active(!ui.agenda.is_active()),
                gdk::Key::n => ui.new_event(None),
                gdk::Key::r => ui.reload(),
                _ => return glib::Propagation::Proceed,
            }
            glib::Propagation::Stop
        }));
        ui.window.add_controller(keys);

        accounts.connect_clicked(glib::clone!(#[weak] ui, move |_| crate::accounts::open(&ui)));
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
        self.load_calendars();
        self.reload();
    }

    pub fn present(&self) {
        self.window.present();
    }

    fn shift(self: &Rc<Self>, by: i64) {
        {
            let mut s = self.state.borrow_mut();
            let days = if s.agenda { i64::from(AGENDA_DAYS) } else { 7 };
            s.anchor += chrono::Duration::days(by * days);
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
                if s.agenda || monday(s.anchor) == monday(today) { today.max(s.anchor) } else { monday(s.anchor) }
            });
            editor::open(self, None, day);
        }
    }

    fn range(&self) -> Range {
        let s = self.state.borrow();
        if s.agenda { Range::days(s.anchor, AGENDA_DAYS) } else { Range::days(monday(s.anchor), 7) }
    }

    fn load_calendars(self: &Rc<Self>) {
        let Some(cals) = self.calendars.borrow().clone() else { return };
        let ui = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let Ok(listing) = gio::spawn_blocking(move || cals.calendars()).await else { return };
            if let Some(ui) = ui.upgrade() {
                *ui.writable.borrow_mut() = listing.items.into_iter().filter(|c| c.writable).collect();
            }
        });
    }

    /// Reads the shown week (or agenda) again from every account.
    pub fn reload(self: &Rc<Self>) {
        let range = self.range();
        let (agenda, anchor) = {
            let s = self.state.borrow();
            (s.agenda, s.anchor)
        };
        self.period.set_text(&if agenda {
            let last = anchor + chrono::Days::new(u64::from(AGENDA_DAYS - 1));
            format!("{} – {}", anchor.format("%-d %b"), last.format("%-d %b %Y"))
        } else {
            let m = monday(anchor);
            format!("{} – {}", m.format("%-d %b"), (m + chrono::Days::new(6)).format("%-d %b %Y"))
        });
        let Some(cals) = self.calendars.borrow().clone() else {
            let text = label(self.setup_error.borrow().as_deref().unwrap_or(""), "setup");
            text.set_selectable(true);
            self.content.set_child(Some(&text));
            return;
        };
        let generation = {
            let mut s = self.state.borrow_mut();
            s.generation += 1;
            s.generation
        };
        if self.content.child().is_none() {
            self.content.set_child(Some(&label("Loading…", "empty")));
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
            if agenda { ui.show_agenda(&listing.items, &range) } else { ui.show_week(&listing.items) }
        });
    }

    fn event_button(self: &Rc<Self>, e: &Event, show_day: bool) -> gtk::Button {
        let b = gtk::Button::new();
        b.add_css_class("event");
        let body = gtk::Box::new(gtk::Orientation::Vertical, 1);
        let dot = e.color.as_deref().filter(|c| c.starts_with('#') && c.len() <= 9).unwrap_or("#888888");
        let title = gtk::Label::new(None);
        title.set_markup(&format!("<span foreground=\"{dot}\">●</span> {}{}", glib::markup_escape_text(&e.title), if e.recurring { " ↻" } else { "" }));
        title.add_css_class("title");
        title.set_xalign(0.0);
        title.set_wrap(true);
        title.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        body.append(&title);
        let mut when = if e.all_day {
            let last = e.end.local_date() - chrono::Days::new(1);
            if last > e.start.local_date() { format!("all day, until {}", last.format("%a %-d")) } else { "all day".into() }
        } else {
            format!("{}–{}", clock(&e.start), clock(&e.end))
        };
        if show_day {
            when = format!("{when} · {} ({})", e.calendar, e.account);
        }
        body.append(&label(&when, "when"));
        if let Some(l) = &e.location {
            body.append(&label(l, "where"));
        }
        b.set_child(Some(&body));
        b.set_tooltip_text(Some(&format!("{} · {} ({})", e.title, e.calendar, e.account)));
        let event = e.clone();
        b.connect_clicked(glib::clone!(#[weak(rename_to = ui)] self, move |_| editor::open(&ui, Some(event.clone()), event.start.local_date())));
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
            let head = gtk::Button::with_label(&format!("{} {}", day.format("%a"), day.day()));
            head.add_css_class("flat");
            head.add_css_class("day-head");
            head.set_tooltip_text(Some("New event on this day"));
            head.connect_clicked(glib::clone!(#[weak(rename_to = ui)] self, move |_| ui.new_event(Some(day))));
            col.append(&head);
            let range = Range::days(day, 1);
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
            let day = first + chrono::Days::new(i);
            let day_range = Range::days(day, 1);
            let todays: Vec<&Event> = events.iter().filter(|e| day_range.overlaps(&e.start, &e.end)).collect();
            if todays.is_empty() {
                continue;
            }
            any = true;
            list.append(&label(&day.format("%A %-d %B").to_string(), "agenda-day"));
            for e in todays {
                list.append(&self.event_button(e, true));
            }
        }
        if !any {
            list.append(&label("No events in these two weeks.", "empty"));
        }
        self.content.set_child(Some(&list));
    }
}

fn setup_text(error: Option<&str>) -> String {
    let problem = error.map(|e| format!("{e}\n\n")).unwrap_or_default();
    format!("{problem}No calendar accounts are linked yet. Add iCloud, Google or HEY under Accounts.")
}
