mod editor;
mod theme;
mod ui;

use gtk::{glib, prelude::*};
use std::cell::OnceCell;
use std::rc::Rc;

pub const APP_ID: &str = "com.ferdousbhai.CloudCalendar";

fn main() -> glib::ExitCode {
    let app = gtk::Application::builder().application_id(APP_ID).build();
    let ui: Rc<OnceCell<Rc<ui::Ui>>> = Rc::default();
    app.connect_activate(move |app| ui.get_or_init(|| ui::Ui::new(app)).present());
    app.run()
}
