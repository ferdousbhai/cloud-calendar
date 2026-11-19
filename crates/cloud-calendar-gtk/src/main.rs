mod accounts;
mod editor;
mod recall_client;
mod ui;

use gtk::{glib, prelude::*};
use std::cell::OnceCell;
use std::rc::Rc;

pub const APP_ID: &str = "com.ferdousbhai.CloudCalendar";

fn main() -> glib::ExitCode {
    // Kept here for as long as the application runs: the window's handlers hold it weakly.
    let ui: Rc<OnceCell<Rc<ui::Ui>>> = Rc::default();
    omui::application(
        APP_ID,
        "cloud-calendar-gtk",
        "Cloud Calendar",
        include_str!("style.css"),
        move |app| ui.get_or_init(|| ui::Ui::new(app)).present(),
    )
    .run()
}
