//! The event editor: a new event, or changes to one. All-day events show their last day (the
//! stored end is the day after). A repeating event's changes reach its whole series, so its times
//! can't change here, and what its provider can't do to a series at all is switched off. Closing it with changes
//! that were not saved asks first, whichever way it is closed. What it says follows a switch of language while it is
//! open; what an account said (an error, a provider's caveat) is English, as the account's library says it.

use chrono::{Local, NaiveDate};
use cloud_calendar_api::{self as api, Event, EventChange, NewEvent, Time};
use gtk::{glib, prelude::*};
use omui::i18n;
use omui::tr;
use std::cell::Cell;
use std::rc::Rc;

use crate::ui::{Live, Ui};

/// An entry holding `text`, with a placeholder and the name a screen reader says, both in the language of the moment.
fn entry(text: &str, placeholder: fn() -> String, name: fn() -> String) -> gtk::Entry {
    let e = gtk::Entry::new();
    e.set_text(text);
    i18n::bind(&e, move |e| {
        e.set_placeholder_text(Some(&placeholder()));
        e.update_property(&[gtk::accessible::Property::Label(&name())]);
    });
    e
}

/// Gives `widget` the name a screen reader says, in the language of the moment: for a field with no placeholder or
/// label of its own to be named by.
fn name(widget: &impl IsA<gtk::Widget>, name: fn() -> String) {
    i18n::bind(widget.as_ref(), move |w| {
        w.update_property(&[gtk::accessible::Property::Label(&name())])
    });
}

/// A line of text in the fields' column, as wide as that column always: a wrapping label's least height is
/// measured at its least width, and GTK warns when the window's height comes from a wider one.
fn note(text: &str, class: &str) -> gtk::Label {
    let l = omui::label(text, class);
    l.set_width_chars(50);
    l.set_max_width_chars(50);
    l
}

/// A field's row: its name, in the language of the moment, beside it.
fn field(grid: &gtk::Grid, row: i32, name: Option<fn() -> String>, widget: &impl IsA<gtk::Widget>) {
    let l = gtk::Label::new(None);
    if let Some(name) = name {
        i18n::bind(&l, move |l| l.set_label(&name()));
    }
    l.add_css_class("dim-label");
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
        let after = last
            .checked_add_days(chrono::Days::new(1))
            .ok_or_else(|| api::Error::bad_request(tr("the last day is out of range")))?;
        let (start, end) = (Time::Date(s), Time::Date(api::time::bounded(after, "the last day")?));
        api::check_span(&start, &end)?;
        return Ok((start, end));
    }
    if st.trim().is_empty() {
        return Err(api::Error::bad_request(tr("Give a start time, or tick All day.")));
    }
    let at = |d: &str, t: &str| api::time::parse_time(&format!("{} {}", date(d)?.format("%Y-%m-%d"), t.trim()), today);
    let start = at(sd, st)?;
    let end = if et.trim().is_empty() {
        api::time::add_length(&start, chrono::Duration::hours(1))?
    } else {
        at(if ed.trim().is_empty() { sd } else { ed }, et)?
    };
    api::check_span(&start, &end)?;
    Ok((start, end))
}

/// Whether the time inputs differ from what the editor showed: only then is a time sent, so a
/// change to the title alone never moves the event (shown clocks drop seconds).
fn times_touched(shown_all_day: bool, shown: [&str; 4], all_day: bool, now: [&str; 4]) -> bool {
    shown_all_day != all_day || shown.iter().zip(now).any(|(a, b)| a.trim() != b.trim())
}

/// What the editor's fields hold. The editor has unsaved work when this differs from what it opened with, however
/// many edits lie between: a title typed and deleted again is no change. The calendar chosen for a new event is
/// left out, since its list fills in by itself as the accounts answer and choosing from it is not work to lose.
#[derive(Debug, PartialEq)]
struct Draft {
    title: String,
    all_day: bool,
    /// Start date and time, end date and time, as typed.
    times: [String; 4],
    location: String,
    notes: String,
}

/// Fills the calendar list from `writable`, keeping the choice when it's still there.
fn fill_calendars(dropdown: &gtk::DropDown, writable: &[api::Calendar]) {
    let chosen = dropdown
        .selected_item()
        .and_downcast::<gtk::StringObject>()
        .map(|o| o.string().to_string());
    let names: Vec<String> = writable.iter().map(|c| format!("{} ({})", c.name, c.account)).collect();
    dropdown.set_model(Some(&gtk::StringList::new(
        &names.iter().map(String::as_str).collect::<Vec<_>>(),
    )));
    if let Some(i) = chosen.and_then(|c| names.iter().position(|n| *n == c)) {
        dropdown.set_selected(i as u32);
    }
}

pub fn open(ui: &Rc<Ui>, event: Option<Event>, day: NaiveDate) {
    let Some(cals) = ui.calendars.borrow().clone() else {
        return;
    };
    let win = gtk::Window::builder().transient_for(&ui.window).modal(true).build();
    let is_new = event.is_none();
    i18n::bind(&win, move |w| {
        w.set_title(Some(&if is_new { tr("New event") } else { tr("Event") }))
    });
    // The editor is kept out of the application, so omui names it `cloud-calendar` only when asked to.
    omui::name_window(&win);
    // While a save runs, the editor stays open: closing it would hide the outcome.
    let busy = Rc::new(Cell::new(false));
    let grid = gtk::Grid::builder().row_spacing(8).column_spacing(10).build();
    grid.add_css_class("sheet");

    let calendar = gtk::DropDown::from_strings(&[]);
    fill_calendars(&calendar, &ui.writable.borrow());
    let title = entry(
        event.as_ref().map(|e| e.title.as_str()).unwrap_or(""),
        || tr("Title"),
        || tr("Title"),
    );
    let all_day = gtk::CheckButton::new();
    i18n::bind_label(&all_day, "All day");
    let (start, end) = match &event {
        Some(e) => (shown(&e.start, false), shown(&e.end, true)),
        None => {
            let d = day.format("%Y-%m-%d").to_string();
            ((d.clone(), "09:00".into()), (d, "10:00".into()))
        }
    };
    all_day.set_active(event.as_ref().is_some_and(|e| e.all_day));
    // The two rows' entries share their placeholders (the form a date or a time is typed in); a screen reader hears
    // which is which.
    let (sd, st, ed, et) = (
        entry(&start.0, || tr("YYYY-MM-DD"), || tr("Start date")),
        entry(&start.1, || tr("HH:MM"), || tr("Start time")),
        entry(&end.0, || tr("YYYY-MM-DD"), || tr("End date")),
        entry(&end.1, || tr("HH:MM"), || tr("End time")),
    );
    // The list has no placeholder to be named by, nor have the notes: each takes the label beside it.
    name(&calendar, || tr("Calendar"));
    let location = entry(
        event.as_ref().and_then(|e| e.location.as_deref()).unwrap_or(""),
        || tr("Location"),
        || tr("Location"),
    );
    let notes = gtk::TextView::new();
    notes.add_css_class("notes");
    notes.set_wrap_mode(gtk::WrapMode::WordChar);
    notes.set_size_request(-1, 90);
    name(&notes, || tr("Notes"));
    notes
        .buffer()
        .set_text(event.as_ref().and_then(|e| e.notes.as_deref()).unwrap_or(""));

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
            Err(_) => Some((
                api::SeriesSupport {
                    edit: false,
                    delete: false,
                },
                // The account's own name, which starts the event's ID: what there is to call it by.
                e.id.split(':').next().unwrap_or_default().to_string(),
            )),
        },
        None => None,
    };
    if let Some(caveat) = event.as_ref().and_then(|e| cals.edit_caveat(&e.id)) {
        let mut caveat = caveat;
        caveat[..1].make_ascii_uppercase();
        grid.attach(&note(&format!("{caveat}."), "meta"), 1, row, 1, 1);
        row += 1;
    }
    if let Some((support, label)) = &support {
        let (support, label) = (*support, label.clone());
        let line = note("", "meta");
        // The provider's name (iCloud, Google, HEY) is a brand, the same in every language.
        i18n::bind(&line, move |line| {
            line.set_text(&match (support.edit, support.delete) {
                (true, _) => tr!(
                    "A repeating {0} event: changes reach every occurrence, and its time can only change in {0}.",
                    label
                ),
                (false, true) => tr!(
                    "A repeating {0} event can't be changed here, only deleted (the whole series); change it in {0}.",
                    label
                ),
                (false, false) => tr!(
                    "A repeating {0} event can't be changed or deleted here; use {0}.",
                    label
                ),
            })
        });
        grid.attach(&line, 1, row, 1, 1);
        row += 1;
    }
    match &event {
        None => field(&grid, row, Some(|| tr("Calendar")), &calendar),
        Some(e) => field(
            &grid,
            row,
            Some(|| tr("Calendar")),
            &gtk::Label::builder()
                .label(format!("{} ({})", e.calendar, e.account))
                .xalign(0.0)
                .build(),
        ),
    }
    row += 1;
    title.set_hexpand(true);
    field(&grid, row, Some(|| tr("Title")), &title);
    row += 1;
    field(&grid, row, None, &all_day);
    row += 1;
    field(&grid, row, Some(|| tr("Starts")), &span(&sd, &st));
    row += 1;
    field(&grid, row, Some(|| tr("Ends")), &span(&ed, &et));
    row += 1;
    field(&grid, row, Some(|| tr("Location")), &location);
    row += 1;
    // omui's text view is bare; in a frame it is a field of several lines.
    field(
        &grid,
        row,
        Some(|| tr("Notes")),
        &gtk::Frame::builder().child(&notes).build(),
    );
    row += 1;
    if recurring {
        for w in [
            sd.upcast_ref::<gtk::Widget>(),
            st.upcast_ref(),
            ed.upcast_ref(),
            et.upcast_ref(),
            all_day.upcast_ref(),
        ] {
            w.set_sensitive(false);
        }
    }
    let can_edit = support.as_ref().is_none_or(|(s, _)| s.edit);
    let can_delete = support.as_ref().is_none_or(|(s, _)| s.delete);
    if !can_edit {
        for w in [
            title.upcast_ref::<gtk::Widget>(),
            location.upcast_ref(),
            notes.upcast_ref(),
        ] {
            w.set_sensitive(false);
        }
    }
    // A new event waits for the calendars it can go on; the list follows when they arrive.
    let no_calendars = note("", "error");
    i18n::bind(&no_calendars, |l| {
        l.set_text(&tr(
            "No calendar you can add to has loaded yet (still loading, or an account failed: see the warning above the week).",
        ))
    });
    if event.is_none() {
        no_calendars.set_visible(ui.writable.borrow().is_empty());
        grid.attach(&no_calendars, 1, row, 1, 1);
        row += 1;
        if ui.writable.borrow().is_empty() {
            ui.load_calendars();
        }
        let ui_weak = Rc::downgrade(ui);
        *ui.on_writable.borrow_mut() = Some(Box::new(glib::clone!(
            #[weak]
            calendar,
            #[weak]
            no_calendars,
            move || {
                let Some(ui) = ui_weak.upgrade() else { return };
                let writable = ui.writable.borrow();
                fill_calendars(&calendar, &writable);
                no_calendars.set_visible(writable.is_empty());
            }
        )));
    }
    let error = note("", "error");
    grid.attach(&error, 1, row, 1, 1);
    let said = Live::new(error.clone());
    row += 1;

    let sync_clocks = glib::clone!(
        #[weak]
        st,
        #[weak]
        et,
        move |b: &gtk::CheckButton| {
            st.set_visible(!b.is_active());
            et.set_visible(!b.is_active());
        }
    );
    sync_clocks(&all_day);
    all_day.connect_toggled(sync_clocks);

    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    buttons.set_halign(gtk::Align::End);
    // Delete asks again on itself: its second label is the question.
    let confirming = Rc::new(Cell::new(false));
    let delete = gtk::Button::new();
    i18n::bind(
        &delete,
        glib::clone!(
            #[strong]
            confirming,
            move |b| b.set_label(&match (confirming.get(), recurring) {
                (false, false) => tr("Delete"),
                (false, true) => tr("Delete series"),
                (true, false) => tr("Really delete?"),
                (true, true) => tr("Delete every occurrence?"),
            })
        ),
    );
    delete.add_css_class("destructive-action");
    let cancel = gtk::Button::new();
    i18n::bind_label(&cancel, "Cancel");
    let save = gtk::Button::new();
    i18n::bind_label(&save, "Save");
    save.add_css_class("suggested-action");
    delete.set_sensitive(can_delete);
    save.set_sensitive(can_edit);
    // In the window even for a new event, hidden: `run` holds it weakly and does nothing once it is gone, as a button
    // with no parent is by the time Save is pressed.
    delete.set_visible(event.is_some());
    buttons.append(&delete);
    buttons.append(&cancel);
    buttons.append(&save);
    grid.attach(&buttons, 1, row, 1, 1);
    win.set_child(Some(&grid));

    // What the fields hold now, and what they held as the editor opened. Nothing is saved without the editor
    // closing, so that stays what there is to compare with.
    let draft = glib::clone!(
        #[weak]
        title,
        #[weak]
        all_day,
        #[weak]
        sd,
        #[weak]
        st,
        #[weak]
        ed,
        #[weak]
        et,
        #[weak]
        location,
        #[weak]
        notes,
        #[upgrade_or_panic]
        move || {
            let buf = notes.buffer();
            Draft {
                title: title.text().into(),
                all_day: all_day.is_active(),
                times: [&sd, &st, &ed, &et].map(|e| e.text().into()),
                location: location.text().into(),
                notes: buf.text(&buf.start_iter(), &buf.end_iter(), false).into(),
            }
        }
    );
    let loaded = draft();
    // Set once the editor may close whatever it holds: its event was saved or deleted, or the changes were given up.
    let settled = Rc::new(Cell::new(false));
    // One question at a time, however many times the window is asked to close.
    let asking = Rc::new(Cell::new(false));
    // Every way out comes through here: Escape, Cancel, Ctrl+W, Ctrl+Q and the window's own close button.
    win.connect_close_request(glib::clone!(
        #[weak]
        ui,
        #[strong]
        busy,
        #[strong]
        settled,
        #[upgrade_or]
        glib::Propagation::Proceed,
        move |win| {
            if busy.get() || asking.get() {
                return glib::Propagation::Stop;
            }
            if settled.get() || draft() == loaded {
                ui.on_writable.borrow_mut().take();
                return glib::Propagation::Proceed;
            }
            asking.set(true);
            // Its words follow a switch of language while it is open, as the editor under it does.
            let question = omui::Dialog::new(Some(""), Some(""));
            question.set_heading_translated("Discard changes?");
            question.set_body_translated("This event has changes that were not saved.");
            question.add_response("keep", "");
            question.add_response("discard", "");
            question.set_response_translated("keep", "_Keep editing");
            question.set_response_translated("discard", "_Discard");
            question.set_response_appearance("discard", omui::Appearance::Destructive);
            // Enter, Escape and closing the question all keep the work.
            question.set_default_response("keep");
            question.set_close_response("keep");
            // Asked while the app was quitting: discarding goes on to quit, keeping calls it off.
            let quitting = ui.is_quitting();
            question.connect_response(glib::clone!(
                #[weak]
                ui,
                #[weak]
                win,
                #[strong]
                settled,
                #[strong]
                asking,
                move |_, response| {
                    asking.set(false);
                    // Only Discard discards: Keep editing, Escape, closing the question and any answer not known here
                    // keep the work.
                    if response != "discard" {
                        return;
                    }
                    settled.set(true);
                    win.close();
                    if quitting {
                        ui.quit();
                    }
                }
            ));
            question.present(win);
            glib::Propagation::Stop
        }
    ));

    // Runs an account call off the main thread; closes the editor and reloads on success.
    let run = Rc::new(glib::clone!(
        #[weak]
        ui,
        #[weak]
        win,
        #[strong]
        said,
        #[weak]
        save,
        #[weak]
        cancel,
        #[weak]
        delete,
        #[strong]
        busy,
        #[strong]
        settled,
        move |job: Box<dyn FnOnce() -> api::Result<()> + Send>| {
            busy.set(true);
            for b in [&save, &cancel, &delete] {
                b.set_sensitive(false);
            }
            said.set(|| tr("Saving…"));
            let said = said.clone();
            let (busy, settled) = (busy.clone(), settled.clone());
            // Held by the application until it is done: a save under way outlives the windows.
            crate::work::write(job, move |result| {
                busy.set(false);
                match result {
                    Ok(()) => {
                        settled.set(true);
                        win.close();
                        ui.reload();
                    }
                    Err(e) => {
                        said.set_fixed(&e.message);
                        save.set_sensitive(can_edit);
                        cancel.set_sensitive(true);
                        delete.set_sensitive(can_delete);
                    }
                }
            });
        }
    ));

    let original = event.clone();
    let shown_all_day = all_day.is_active();
    let shown_times = [
        sd.text().to_string(),
        st.text().to_string(),
        ed.text().to_string(),
        et.text().to_string(),
    ];
    save.connect_clicked(glib::clone!(
        #[strong]
        run,
        #[weak]
        ui,
        #[weak]
        title,
        #[weak]
        all_day,
        #[weak]
        sd,
        #[weak]
        st,
        #[weak]
        ed,
        #[weak]
        et,
        #[weak]
        location,
        #[weak]
        notes,
        // Strong: an event's editor shows its calendar as a label, so nothing else keeps the list.
        #[strong]
        calendar,
        #[strong]
        said,
        move |_| {
            let buf = notes.buffer();
            let notes_text = buf.text(&buf.start_iter(), &buf.end_iter(), false).trim().to_string();
            let title_text = title.text().trim().to_string();
            let location_text = location.text().trim().to_string();
            // The accounts as they are now: one may have been linked or removed since this opened.
            let Some(cals) = ui.calendars.borrow().clone() else {
                return said.set(|| tr("No accounts are linked any more."));
            };
            match &original {
                None => {
                    let writable = ui.writable.borrow();
                    if writable.is_empty() {
                        return said
                            .set(|| tr("No calendar you can add to has loaded yet; try Save again in a moment."));
                    }
                    let Some(cal) = writable.get(calendar.selected() as usize) else {
                        return said.set(|| tr("Pick a calendar."));
                    };
                    let (start, end) = match typed(all_day.is_active(), &sd.text(), &st.text(), &ed.text(), &et.text())
                    {
                        Ok(s) => s,
                        Err(e) => return said.set_fixed(&e.message),
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
                    let now = [
                        sd.text().to_string(),
                        st.text().to_string(),
                        ed.text().to_string(),
                        et.text().to_string(),
                    ];
                    let touched = times_touched(
                        shown_all_day,
                        shown_times.each_ref().map(String::as_str),
                        all_day.is_active(),
                        now.each_ref().map(String::as_str),
                    );
                    if !e.recurring && touched {
                        match typed(all_day.is_active(), &sd.text(), &st.text(), &ed.text(), &et.text()) {
                            Ok((s, t)) if (s, t) != (e.start, e.end) => {
                                change.start = Some(s);
                                change.end = Some(t);
                            }
                            Ok(_) => {}
                            Err(err) => return said.set_fixed(&err.message),
                        }
                    }
                    if change.is_empty() {
                        return said.set(|| tr("Nothing changed."));
                    }
                    let id = e.id.clone();
                    run(Box::new(move || cals.update(&id, &change).map(|_| ())));
                }
            }
        }
    ));

    delete.connect_clicked(glib::clone!(
        #[weak]
        ui,
        #[weak]
        win,
        #[strong]
        settled,
        #[strong]
        event,
        move |b| {
            let Some(e) = &event else { return };
            if !confirming.replace(true) {
                // The label follows the language from the binding above; it is the question now.
                b.set_label(&if e.recurring {
                    tr("Delete every occurrence?")
                } else {
                    tr("Really delete?")
                });
                return;
            }
            // The editor closes at once and the week offers Undo; changes typed here go with the event.
            settled.set(true);
            win.close();
            ui.delete_later(e);
        }
    ));
    cancel.connect_clicked(glib::clone!(
        #[weak]
        win,
        move |_| win.close()
    ));

    // Escape closes the editor as its close button does, so changes are asked about first; it is typed into, so
    // its shortcuts are on F1 and Ctrl+?.
    crate::ui::window_keys(ui, &win, true).on_always(
        "Escape",
        glib::clone!(
            #[weak]
            win,
            move || win.close()
        ),
    );
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
        assert_eq!(
            typed(true, "2026-10-09", "", "2026-10-11", "").unwrap(),
            (Time::Date(d(9)), Time::Date(d(12)))
        );
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
        assert!(times_touched(
            false,
            shown,
            false,
            ["2026-10-09", "14:30", "2026-10-09", "15:00"]
        ));
        assert!(times_touched(false, shown, true, shown));
    }
}
