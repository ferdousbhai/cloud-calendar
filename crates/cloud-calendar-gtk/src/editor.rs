//! The event editor: a new event, or changes to one. All-day events show their last day (the
//! stored end is the day after). A repeating event's changes reach its whole series, so its times
//! can't change here.

use chrono::{Local, NaiveDate};
use cloud_calendar_api::{self as api, Event, EventChange, NewEvent, Time};
use gtk::{gdk, gio, glib, prelude::*};
use std::rc::Rc;

use crate::ui::Ui;

fn entry(text: &str, placeholder: &str) -> gtk::Entry {
    let e = gtk::Entry::new();
    e.set_text(text);
    e.set_placeholder_text(Some(placeholder));
    e
}

fn field(grid: &gtk::Grid, row: i32, name: &str, widget: &impl IsA<gtk::Widget>) {
    let l = gtk::Label::new(Some(name));
    l.add_css_class("field-label");
    l.set_xalign(1.0);
    grid.attach(&l, 0, row, 1, 1);
    grid.attach(widget, 1, row, 1, 1);
}

/// `(date, clock)` as shown for an end or start; an all-day end shows its last day.
fn shown(t: &Time, is_end: bool) -> (String, String) {
    match t {
        Time::At(t) => {
            let l = t.with_timezone(&Local);
            (l.format("%Y-%m-%d").to_string(), l.format("%H:%M").to_string())
        }
        Time::Date(d) => {
            let d = if is_end { *d - chrono::Days::new(1) } else { *d };
            (d.format("%Y-%m-%d").to_string(), String::new())
        }
    }
}

/// The span typed in: dates alone for all-day (the last day inclusive), else dates and clocks.
fn typed(all_day: bool, sd: &str, st: &str, ed: &str, et: &str) -> api::Result<(Time, Time)> {
    let today = Local::now().date_naive();
    let date = |s: &str| api::time::parse_date(s, today);
    if all_day {
        let s = date(sd)?;
        let last = if ed.trim().is_empty() { s } else { date(ed)? };
        let (start, end) = (Time::Date(s), Time::Date(last + chrono::Days::new(1)));
        api::check_span(&start, &end)?;
        return Ok((start, end));
    }
    let at = |d: &str, t: &str| api::time::parse_time(&format!("{} {}", date(d)?.format("%Y-%m-%d"), t.trim()), today);
    let start = at(sd, st)?;
    let end = if et.trim().is_empty() { api::time::add_length(&start, chrono::Duration::hours(1)) } else { at(if ed.trim().is_empty() { sd } else { ed }, et)? };
    api::check_span(&start, &end)?;
    Ok((start, end))
}

pub fn open(ui: &Rc<Ui>, event: Option<Event>, day: NaiveDate) {
    let Some(cals) = ui.calendars.borrow().clone() else { return };
    let win = gtk::Window::builder().title(if event.is_some() { "Event" } else { "New event" }).transient_for(&ui.window).modal(true).default_width(520).build();
    win.add_css_class("editor");
    let grid = gtk::Grid::builder().row_spacing(8).column_spacing(10).build();
    grid.add_css_class("editor");

    let writable = ui.writable.borrow().clone();
    let names: Vec<String> = writable.iter().map(|c| format!("{} ({})", c.name, c.account)).collect();
    let calendar = gtk::DropDown::from_strings(&names.iter().map(String::as_str).collect::<Vec<_>>());
    let title = entry(event.as_ref().map(|e| e.title.as_str()).unwrap_or(""), "Title");
    let all_day = gtk::CheckButton::with_label("All day");
    let (start, end) = match &event {
        Some(e) => (shown(&e.start, false), shown(&e.end, true)),
        None => {
            let d = day.format("%Y-%m-%d").to_string();
            ((d.clone(), "09:00".into()), (d, "10:00".into()))
        }
    };
    all_day.set_active(event.as_ref().is_some_and(|e| e.all_day));
    let (sd, st, ed, et) = (entry(&start.0, "YYYY-MM-DD"), entry(&start.1, "HH:MM"), entry(&end.0, "YYYY-MM-DD"), entry(&end.1, "HH:MM"));
    let location = entry(event.as_ref().and_then(|e| e.location.as_deref()).unwrap_or(""), "Location");
    let notes = gtk::TextView::new();
    notes.set_wrap_mode(gtk::WrapMode::WordChar);
    notes.set_size_request(-1, 90);
    notes.buffer().set_text(event.as_ref().and_then(|e| e.notes.as_deref()).unwrap_or(""));

    let span = |a: &gtk::Entry, b: &gtk::Entry| {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        a.set_hexpand(true);
        row.append(a);
        row.append(b);
        row
    };
    let mut row = 0;
    match &event {
        None => field(&grid, row, "Calendar", &calendar),
        Some(e) => field(&grid, row, "Calendar", &gtk::Label::builder().label(format!("{} ({})", e.calendar, e.account)).xalign(0.0).build()),
    }
    row += 1;
    title.set_hexpand(true);
    field(&grid, row, "Title", &title);
    row += 1;
    field(&grid, row, "", &all_day);
    row += 1;
    field(&grid, row, "Starts", &span(&sd, &st));
    row += 1;
    field(&grid, row, "Ends", &span(&ed, &et));
    row += 1;
    field(&grid, row, "Location", &location);
    row += 1;
    field(&grid, row, "Notes", &notes);
    row += 1;
    let recurring = event.as_ref().is_some_and(|e| e.recurring);
    if recurring {
        let note = gtk::Label::new(Some("A repeating event: changes reach every occurrence, and its time can only change in the calendar it came from."));
        note.add_css_class("note");
        note.set_wrap(true);
        note.set_xalign(0.0);
        grid.attach(&note, 1, row, 1, 1);
        row += 1;
        for w in [sd.upcast_ref::<gtk::Widget>(), st.upcast_ref(), ed.upcast_ref(), et.upcast_ref(), all_day.upcast_ref()] {
            w.set_sensitive(false);
        }
    }
    if event.is_none() && writable.is_empty() {
        let note = gtk::Label::new(Some("No calendar you can add to was found (still loading, or every account failed)."));
        note.add_css_class("error");
        grid.attach(&note, 1, row, 1, 1);
        row += 1;
    }
    let error = gtk::Label::new(None);
    error.add_css_class("error");
    error.set_wrap(true);
    error.set_xalign(0.0);
    grid.attach(&error, 1, row, 1, 1);
    row += 1;

    let sync_clocks = glib::clone!(#[weak] st, #[weak] et, move |b: &gtk::CheckButton| {
        st.set_visible(!b.is_active());
        et.set_visible(!b.is_active());
    });
    sync_clocks(&all_day);
    all_day.connect_toggled(sync_clocks);

    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    buttons.set_halign(gtk::Align::End);
    let delete = gtk::Button::with_label(if recurring { "Delete series" } else { "Delete" });
    delete.add_css_class("destructive");
    let cancel = gtk::Button::with_label("Cancel");
    let save = gtk::Button::with_label("Save");
    save.add_css_class("suggested");
    if event.is_some() {
        buttons.append(&delete);
    }
    buttons.append(&cancel);
    buttons.append(&save);
    grid.attach(&buttons, 1, row, 1, 1);
    win.set_child(Some(&grid));

    // Runs an account call off the main thread; closes the editor and reloads on success.
    let run = Rc::new(glib::clone!(#[weak] ui, #[weak] win, #[weak] error, #[weak] save, move |job: Box<dyn FnOnce() -> api::Result<()> + Send>| {
        save.set_sensitive(false);
        error.set_text("Saving…");
        glib::MainContext::default().spawn_local(async move {
            let result = gio::spawn_blocking(job).await.unwrap_or_else(|_| Err(api::Error::new(api::ErrorKind::AccountUnavailable, "failed unexpectedly")));
            match result {
                Ok(()) => {
                    win.close();
                    ui.reload();
                }
                Err(e) => {
                    error.set_text(&e.message);
                    save.set_sensitive(true);
                }
            }
        });
    }));

    let original = event.clone();
    save.connect_clicked(glib::clone!(#[strong] run, #[strong] cals, #[weak] title, #[weak] all_day, #[weak] sd, #[weak] st, #[weak] ed, #[weak] et, #[weak] location, #[weak] notes, #[weak] calendar, #[weak] error, move |_| {
        let buf = notes.buffer();
        let notes_text = buf.text(&buf.start_iter(), &buf.end_iter(), false).trim().to_string();
        let title_text = title.text().trim().to_string();
        let location_text = location.text().trim().to_string();
        let cals = cals.clone();
        match &original {
            None => {
                let Some(cal) = writable.get(calendar.selected() as usize) else {
                    error.set_text("Pick a calendar.");
                    return;
                };
                let (start, end) = match typed(all_day.is_active(), &sd.text(), &st.text(), &ed.text(), &et.text()) {
                    Ok(s) => s,
                    Err(e) => return error.set_text(&e.message),
                };
                let new = NewEvent {
                    calendar_id: cal.id.clone(),
                    title: title_text,
                    start,
                    end,
                    location: Some(location_text).filter(|l| !l.is_empty()),
                    notes: Some(notes_text).filter(|n| !n.is_empty()),
                };
                run(Box::new(move || cals.create(&new).map(|_| ())));
            }
            Some(e) => {
                let mut change = EventChange::default();
                if title_text != e.title {
                    change.title = Some(title_text);
                }
                if location_text != e.location.clone().unwrap_or_default() {
                    change.location = Some(location_text);
                }
                if notes_text != e.notes.clone().unwrap_or_default() {
                    change.notes = Some(notes_text);
                }
                if !e.recurring {
                    match typed(all_day.is_active(), &sd.text(), &st.text(), &ed.text(), &et.text()) {
                        Ok((s, t)) if (s, t) != (e.start, e.end) => {
                            change.start = Some(s);
                            change.end = Some(t);
                        }
                        Ok(_) => {}
                        Err(err) => return error.set_text(&err.message),
                    }
                }
                if change.is_empty() {
                    return error.set_text("Nothing changed.");
                }
                let id = e.id.clone();
                run(Box::new(move || cals.update(&id, &change)));
            }
        }
    }));

    let confirming = Rc::new(std::cell::Cell::new(false));
    delete.connect_clicked(glib::clone!(#[strong] run, #[strong] cals, #[strong] event, move |b| {
        let Some(e) = &event else { return };
        if !confirming.replace(true) {
            b.set_label(if e.recurring { "Delete every occurrence?" } else { "Really delete?" });
            return;
        }
        let (cals, id) = (cals.clone(), e.id.clone());
        run(Box::new(move || cals.delete(&id)));
    }));
    cancel.connect_clicked(glib::clone!(#[weak] win, move |_| win.close()));

    let keys = gtk::EventControllerKey::new();
    keys.connect_key_pressed(glib::clone!(#[weak] win, #[upgrade_or] glib::Propagation::Proceed, move |_, key, _, _| {
        if key == gdk::Key::Escape {
            win.close();
            return glib::Propagation::Stop;
        }
        glib::Propagation::Proceed
    }));
    win.add_controller(keys);
    win.present();
    title.grab_focus();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_day_shows_and_reads_its_last_day() {
        let d = |n| NaiveDate::from_ymd_opt(2026, 10, n).unwrap();
        assert_eq!(shown(&Time::Date(d(12)), true).0, "2026-10-11");
        assert_eq!(typed(true, "2026-10-09", "", "2026-10-11", "").unwrap(), (Time::Date(d(9)), Time::Date(d(12))));
        assert_eq!(typed(true, "2026-10-09", "", "", "").unwrap().1, Time::Date(d(10)));
        assert!(typed(true, "2026-10-09", "", "2026-10-08", "").is_err());
    }
}
