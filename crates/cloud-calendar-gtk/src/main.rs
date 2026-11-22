mod accounts;
mod editor;
mod recall_client;
mod ui;
mod work;

use gtk::{glib, prelude::*};
use std::cell::RefCell;
use std::rc::Rc;

/// The one name Omarchy knows the app by: the command, the package, the `.desktop` file, the icon and the Wayland app
/// id (the window's class in Hyprland).
pub const NAME: &str = "cloud-calendar";

/// The application's D-Bus name, which makes a second launch present the open window. A GApplication id needs a dot,
/// so it cannot be `NAME`; it stays the name it always had, so a running copy of an older version is still found.
pub const APP_ID: &str = "com.ferdousbhai.CloudCalendar";

fn main() -> glib::ExitCode {
    omui::i18n::register_all(&[("en", include_str!("i18n/en.json"))]);
    // The QA scripts switch to a language of their own to see the window follow; release builds have no such catalog.
    #[cfg(feature = "qa")]
    omui::i18n::register("da", include_str!("../../../dev/qa/da.json"));
    // Kept here for as long as the application runs: the window's handlers hold it weakly.
    let ui: Rc<RefCell<Option<Rc<ui::Ui>>>> = Rc::default();
    omui::application(
        APP_ID,
        NAME,
        "Cloud Calendar",
        include_str!("style.css"),
        // omui calls this only when the application has no window: at the start, and for a launch that arrives
        // while it is finishing a save after its window was closed. A closed window is gone, so each gets a new one.
        move |app| {
            let new = ui::Ui::new(app);
            new.present();
            ui.replace(Some(new));
        },
    )
    .run()
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_catalogs_match_the_sources() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        omui::i18n::CatalogCheck::new(root.join("src"), root.join("src/i18n")).assert_ok();
    }

    /// The notifications' strings are the library's, which has no GTK to run omui's check: it is run from here.
    #[test]
    fn the_notification_catalogs_match_their_sources() {
        let api = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../cloud-calendar-api/src");
        omui::i18n::CatalogCheck::new(api.clone(), api.join("i18n")).assert_ok();
    }

    /// Every file that carries the name: the desktop entry is named by it, opens the window through it, and tells
    /// Hyprland the class the window has.
    #[test]
    fn the_desktop_entry_is_the_one_name() {
        let entry = include_str!("../data/cloud-calendar.desktop");
        for line in [
            "Exec=cloud-calendar open",
            "Icon=cloud-calendar",
            "StartupWMClass=cloud-calendar",
        ] {
            assert!(entry.lines().any(|l| l == line), "{line} is missing");
        }
        assert_eq!(super::NAME, "cloud-calendar");
        assert!(gtk::gio::Application::id_is_valid(super::APP_ID));
    }
}
