//! The event editor: a new event, or changes to one. All-day events show their last day (the
//! stored end is the day after). A repeating event's changes reach its whole series, so its times
//! can't change here, and what its provider can't do to a series at all is switched off.

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
        let after = last.checked_add_days(chrono::Days::new(1)).ok_or_else(|| api::Error::bad_request("the last day is out of range"))?;
        let (start, end) = (Time::Date(s), Time::Date(api::time::bounded(after, "the last day")?));
        api::check_span(&start, &end)?;
        return Ok((start, end));
    }
    if st.trim().is_empty() {
        return Err(api::Error::bad_request("Give a start time, or tick All day."));
    }
    let at = |d: &str, t: &str| api::time::parse_time(&format!("{} {}", date(d)?.format("%Y-%m-%d"), t.trim()), today);
    let start = at(sd, st)?;
    let end = if et.trim().is_empty() { api::time::add_length(&start, chrono::Duration::hours(1))? } else { at(if ed.trim().is_empty() { sd } else { ed }, et)? };
    api::check_span(&start, &end)?;
    Ok((start, end))
}

/// Whether the time inputs differ from what the editor showed: only then is a time sent, so a
/// change to the title alone never moves the event (shown clocks drop seconds).
fn times_touched(shown_all_day: bool, shown: [&str; 4], all_day: bool, now: [&str; 4]) -> bool {
    shown_all_day != all_day || shown.iter().zip(now).any(|(a, b)| a.trim() != b.trim())
}

/// Fills the calendar list from `writable`, keeping the choice when it's still there.
fn fill_calendars(dropdown: &gtk::DropDown, writable: &[api::Calendar]) {
    let chosen = dropdown.selected_item().and_downcast::<gtk::StringObject>().map(|o| o.string().to_string());
    let names: Vec<String> = writable.iter().map(|c| format!("{} ({})", c.name, c.account)).collect();
    dropdown.set_model(Some(&gtk::StringList::new(&names.iter().map(String::as_str).collect::<Vec<_>>())));
    if let Some(i) = chosen.and_then(|c| names.iter().position(|n| *n == c)) {
        dropdown.set_selected(i as u32);
    }
}

pub fn open(ui: &Rc<Ui>, event: Option<Event>, day: NaiveDate) {
    let Some(cals) = ui.calendars.borrow().clone() else { return };
    let win = gtk::Window::builder().title(if event.is_some() { "Event" } else { "New event" }).transient_for(&ui.window).modal(true).default_width(520).build();
    win.add_css_class("editor");
    // While a save or delete runs, the editor stays open: closing it would hide the outcome.
    let busy = Rc::new(std::cell::Cell::new(false));
    win.connect_close_request(glib::clone!(#[strong] busy, move |_| if busy.get() { glib::Propagation::Stop } else { glib::Propagation::Proceed }));
    let grid = gtk::Grid::builder().row_spacing(8).column_spacing(10).build();
    grid.add_css_class("editor");

    let calendar = gtk::DropDown::from_strings(&[]);
    fill_calendars(&calendar, &ui.writable.borrow());
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
    let recurring = event.as_ref().is_some_and(|e| e.recurring);
    // What this event's provider can do to its series, said up front.
    let support = match event.as_ref().filter(|e| e.recurring) {
        Some(e) => match cals.series_support(&e.id) {
            Ok((support, label)) => Some((support, label)),
            Err(_) => Some((api::SeriesSupport { edit: false, delete: false }, "this".to_string())),
        },
        None => None,
    };
    if let Some(caveat) = event.as_ref().and_then(|e| cals.edit_caveat(&e.id)) {
        let mut caveat = caveat;
        caveat[..1].make_ascii_uppercase();
        let note = gtk::Label::new(Some(&format!("{caveat}.")));
        note.add_css_class("note");
        note.set_wrap(true);
        note.set_xalign(0.0);
        grid.attach(&note, 1, row, 1, 1);
        row += 1;
    }
    if let Some((support, label)) = &support {
        let text = match (support.edit, support.delete) {
            (true, _) => format!("A repeating {label} event: changes reach every occurrence, and its time can only change in {label}."),
            (false, true) => format!("A repeating {label} event can't be changed here, only deleted (the whole series); change it in {label}."),
            (false, false) => format!("A repeating {label} event can't be changed or deleted here; use {label}."),
        };
        let note = gtk::Label::new(Some(&text));
        note.add_css_class("note");
        note.set_wrap(true);
        note.set_xalign(0.0);
        grid.attach(&note, 1, row, 1, 1);
        row += 1;
    }
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
    if recurring {
        for w in [sd.upcast_ref::<gtk::Widget>(), st.upcast_ref(), ed.upcast_ref(), et.upcast_ref(), all_day.upcast_ref()] {
            w.set_sensitive(false);
        }
    }
    let can_edit = support.as_ref().is_none_or(|(s, _)| s.edit);
    let can_delete = support.as_ref().is_none_or(|(s, _)| s.delete);
    if !can_edit {
        for w in [title.upcast_ref::<gtk::Widget>(), location.upcast_ref(), notes.upcast_ref()] {
            w.set_sensitive(false);
        }
    }
    // A new event waits for the calendars it can go on; the list follows when they arrive.
    let no_calendars = gtk::Label::new(Some("No calendar you can add to has loaded yet (still loading, or an account failed: see the warning above the week)."));
    no_calendars.add_css_class("error");
    no_calendars.set_wrap(true);
    no_calendars.set_xalign(0.0);
    if event.is_none() {
        no_calendars.set_visible(ui.writable.borrow().is_empty());
        grid.attach(&no_calendars, 1, row, 1, 1);
        row += 1;
        if ui.writable.borrow().is_empty() {
            ui.load_calendars();
        }
        let ui_weak = Rc::downgrade(ui);
        *ui.on_writable.borrow_mut() = Some(Box::new(glib::clone!(#[weak] calendar, #[weak] no_calendars, move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            let writable = ui.writable.borrow();
            fill_calendars(&calendar, &writable);
            no_calendars.set_visible(writable.is_empty());
        })));
        win.connect_close_request(glib::clone!(#[weak] ui, #[upgrade_or] glib::Propagation::Proceed, move |_| {
            ui.on_writable.borrow_mut().take();
            glib::Propagation::Proceed
        }));
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
        delete.set_sensitive(can_delete);
        save.set_sensitive(can_edit);
        buttons.append(&delete);
    }
    buttons.append(&cancel);
    buttons.append(&save);
    grid.attach(&buttons, 1, row, 1, 1);
    win.set_child(Some(&grid));

    // Runs an account call off the main thread; closes the editor and reloads on success.
    let run = Rc::new(glib::clone!(#[weak] ui, #[weak] win, #[weak] error, #[weak] save, #[weak] cancel, #[weak] delete, #[strong] busy, move |job: Box<dyn FnOnce() -> api::Result<()> + Send>| {
        busy.set(true);
        for b in [&save, &cancel, &delete] {
            b.set_sensitive(false);
        }
        error.set_text("Saving…");
        let busy = busy.clone();
        glib::MainContext::default().spawn_local(async move {
            let result = gio::spawn_blocking(job).await.unwrap_or_else(|_| Err(api::Error::new(api::ErrorKind::AccountUnavailable, "failed unexpectedly")));
            busy.set(false);
            match result {
                Ok(()) => {
                    win.close();
                    ui.reload();
                }
                Err(e) => {
                    error.set_text(&e.message);
                    save.set_sensitive(can_edit);
                    cancel.set_sensitive(true);
                    delete.set_sensitive(can_delete);
                }
            }
        });
    }));

    let original = event.clone();
    let shown_all_day = all_day.is_active();
    let shown_times = [sd.text().to_string(), st.text().to_string(), ed.text().to_string(), et.text().to_string()];
    save.connect_clicked(glib::clone!(#[strong] run, #[weak] ui, #[weak] title, #[weak] all_day, #[weak] sd, #[weak] st, #[weak] ed, #[weak] et, #[weak] location, #[weak] notes, #[weak] calendar, #[weak] error, move |_| {
        let buf = notes.buffer();
        let notes_text = buf.text(&buf.start_iter(), &buf.end_iter(), false).trim().to_string();
        let title_text = title.text().trim().to_string();
        let location_text = location.text().trim().to_string();
        // The accounts as they are now: one may have been linked or removed since this opened.
        let Some(cals) = ui.calendars.borrow().clone() else {
            return error.set_text("No accounts are linked any more; your entries are kept.");
        };
        match &original {
            None => {
                let writable = ui.writable.borrow();
                if writable.is_empty() {
                    return error.set_text("No calendar you can add to has loaded yet; your entries are kept, so try Save again in a moment.");
                }
                let Some(cal) = writable.get(calendar.selected() as usize) else {
                    return error.set_text("Pick a calendar.");
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
                let now = [sd.text().to_string(), st.text().to_string(), ed.text().to_string(), et.text().to_string()];
                let touched = times_touched(shown_all_day, shown_times.each_ref().map(String::as_str), all_day.is_active(), now.each_ref().map(String::as_str));
                if !e.recurring && touched {
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
                run(Box::new(move || cals.update(&id, &change).map(|_| ())));
            }
        }
    }));

    let confirming = Rc::new(std::cell::Cell::new(false));
    delete.connect_clicked(glib::clone!(#[strong] run, #[weak] ui, #[weak] error, #[strong] event, move |b| {
        let Some(e) = &event else { return };
        if !confirming.replace(true) {
            b.set_label(if e.recurring { "Delete every occurrence?" } else { "Really delete?" });
            return;
        }
        let Some(cals) = ui.calendars.borrow().clone() else {
            return error.set_text("No accounts are linked any more.");
        };
        let id = e.id.clone();
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

    #[test]
    fn timed_needs_a_start_time() {
        let e = typed(false, "2026-10-09", "", "2026-10-11", "").unwrap_err();
        assert!(e.message.contains("start time"), "{}", e.message);
    }

    #[test]
    fn only_touched_times_are_sent() {
        let shown = ["2026-10-09", "14:00", "2026-10-09", "15:00"];
        assert!(!times_touched(false, shown, false, shown));
        assert!(times_touched(false, shown, false, ["2026-10-09", "14:30", "2026-10-09", "15:00"]));
        assert!(times_touched(false, shown, true, shown));
    }
}
