//! Colors from Omarchy's current theme (`~/.local/state/omarchy/current/theme/colors.toml`), as
//! cloud-mail's app reads them, re-applied whenever the theme changes.

use gtk::{gdk, gio, glib, prelude::*};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

#[derive(Debug, Clone, PartialEq)]
pub struct Palette {
    pub dark: bool,
    pub accent: String,
    pub selection: String,
    pub muted: String,
    pub background: String,
    pub dark_background: String,
    pub lighter_background: String,
    pub foreground: String,
    pub dark_foreground: String,
    pub bright_foreground: String,
    pub red: String,
}

impl Default for Palette {
    fn default() -> Self {
        // tokyo-night
        Self {
            dark: true,
            accent: "#7aa2f7".into(),
            selection: "#292e42".into(),
            muted: "#414868".into(),
            background: "#1a1b26".into(),
            dark_background: "#13141c".into(),
            lighter_background: "#24283b".into(),
            foreground: "#a9b1d6".into(),
            dark_foreground: "#565f89".into(),
            bright_foreground: "#c0caf5".into(),
            red: "#f7768e".into(),
        }
    }
}

fn state_dir() -> PathBuf {
    dirs::state_dir()
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".local/state"))
        .join("omarchy/current")
}

fn is_color(v: &str) -> bool {
    v.starts_with('#') && gdk::RGBA::parse(v).is_ok()
}

pub fn load() -> Palette {
    let mut p = Palette::default();
    let Ok(text) = std::fs::read_to_string(state_dir().join("theme/colors.toml")) else {
        return p;
    };
    let Ok(map) = toml::from_str::<HashMap<String, toml::Value>>(&text) else {
        return p;
    };
    let get = |k: &str| map.get(k).and_then(|v| v.as_str()).map(str::to_string);
    if let Some(mode) = get("mode") {
        p.dark = mode != "light";
    }
    let slots: [(&str, &mut String); 10] = [
        ("accent", &mut p.accent),
        ("selection", &mut p.selection),
        ("muted", &mut p.muted),
        ("background", &mut p.background),
        ("dark_background", &mut p.dark_background),
        ("lighter_background", &mut p.lighter_background),
        ("foreground", &mut p.foreground),
        ("dark_foreground", &mut p.dark_foreground),
        ("bright_foreground", &mut p.bright_foreground),
        ("red", &mut p.red),
    ];
    for (key, slot) in slots {
        if let Some(v) = get(key).filter(|v| is_color(v)) {
            *slot = v;
        }
    }
    p
}

pub fn css(p: &Palette) -> String {
    format!(
        r#"
window.cloud-calendar, window.editor {{
  background: {bg};
  color: {fg};
  font-family: monospace;
  font-size: 10pt;
}}
.toolbar {{ padding: 10px 16px; border-bottom: 1px solid alpha({fg}, 0.10); background: {bg}; }}
.toolbar .period {{ color: {bfg}; font-weight: bold; padding: 0 10px; }}
.toolbar .brand {{ color: {fg}; font-weight: 600; padding-right: 12px; }}
.warning {{ color: {red}; font-size: 9pt; padding: 4px 14px; }}
.day {{ border-right: 1px solid alpha({fg}, 0.10); padding: 6px; }}
.day.today {{ background: alpha({accent}, 0.035); }}
.day-head {{ color: alpha({fg}, 0.65); font-size: 9pt; font-weight: bold; padding: 2px 4px 8px 4px; }}
.today .day-head {{ color: {accent}; }}
.agenda-day {{ color: {accent}; font-weight: bold; padding: 14px 16px 4px 16px; }}
button.event {{ background: alpha({fg}, 0.025); border: 1px solid alpha({fg}, 0.10); border-left: 2px solid alpha({accent}, 0.65); border-radius: 6px; padding: 7px 8px; margin: 2px 0; box-shadow: none; }}
button.event:hover {{ background: alpha({accent}, 0.10); border-color: alpha({accent}, 0.5); }}
button.event .title {{ color: {bfg}; }}
button.event .when, button.event .where, .dim {{ color: alpha({fg}, 0.65); font-size: 9pt; }}
.agenda button.event {{ margin: 2px 16px; padding: 9px 12px; }}
.empty {{ color: alpha({fg}, 0.65); padding: 40px; }}
.error {{ color: {red}; }}
.setup {{ color: {fg}; padding: 30px; }}
.setup-title {{ color: {accent}; font-weight: bold; font-size: 14pt; }}
.editor {{ padding: 16px; }}
.editor .field-label {{ color: alpha({fg}, 0.65); }}
.editor .note {{ color: alpha({fg}, 0.65); font-size: 9pt; }}
entry, textview, textview text, dropdown > button {{ background: alpha({fg}, 0.025); color: {bfg}; caret-color: {accent}; }}
entry {{ min-height: 32px; border: 1px solid alpha({fg}, 0.16); border-radius: 6px; box-shadow: none; }}
entry:focus-within {{ border-color: {accent}; outline: none; }}
textview {{ padding: 8px; border-radius: 6px; }}
button {{ background: transparent; color: {fg}; border: 1px solid alpha({fg}, 0.16); border-radius: 6px; box-shadow: none; padding: 5px 10px; min-height: 24px; font-weight: 500; }}
button:hover {{ background: alpha({fg}, 0.07); color: {bfg}; }}
button:focus-visible {{ outline: 1px solid {accent}; outline-offset: 2px; }}
button.suggested {{ background: alpha({accent}, 0.14); color: {accent}; border-color: alpha({accent}, 0.35); font-weight: 600; }}
button.destructive {{ color: {red}; border-color: {red}; background: transparent; }}
button.destructive:hover {{ background: {red}; color: {dbg}; }}
button.flat {{ border-color: transparent; background: transparent; }}
togglebutton:checked, button:checked {{ background: {sel}; color: {bfg}; border-color: {accent}; }}
checkbutton {{ color: {fg}; }}
scrollbar {{ background: transparent; }}
scrollbar slider {{ background: {muted}; border-radius: 6px; min-width: 6px; }}
popover > contents {{ background: {bg}; color: {fg}; }}
"#,
        bg = p.background,
        dbg = p.dark_background,
        fg = p.foreground,
        bfg = p.bright_foreground,
        sel = p.selection,
        muted = p.muted,
        accent = p.accent,
        red = p.red,
    )
}

thread_local! {
    static PROVIDER: RefCell<Option<gtk::CssProvider>> = const { RefCell::new(None) };
    static MONITORS: RefCell<Vec<gio::FileMonitor>> = const { RefCell::new(Vec::new()) };
    static PENDING: RefCell<Option<glib::SourceId>> = const { RefCell::new(None) };
}

pub fn apply(p: &Palette) {
    let Some(display) = gdk::Display::default() else { return };
    PROVIDER.with(|cell| {
        let mut cell = cell.borrow_mut();
        let provider = cell.get_or_insert_with(|| {
            let provider = gtk::CssProvider::new();
            gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_USER);
            provider
        });
        provider.load_from_string(&css(p));
    });
    if let Some(settings) = gtk::Settings::default() {
        settings.set_gtk_application_prefer_dark_theme(p.dark);
    }
}

/// Watches Omarchy's current-theme directory and calls `on_change` (debounced)
/// whenever the palette changes. Omarchy swaps `current/theme` by `mv`, so we
/// watch `current/` and re-arm the watch on `current/theme` after each change.
pub fn watch(on_change: impl Fn(Palette) + 'static) {
    let on_change = Rc::new(on_change);
    let last = Rc::new(RefCell::new(load()));
    arm(on_change, last);
}

fn arm(on_change: Rc<dyn Fn(Palette)>, last: Rc<RefCell<Palette>>) {
    let dir = state_dir();
    let mut monitors = Vec::new();
    for path in [dir.clone(), dir.join("theme")] {
        let Ok(monitor) =
            gio::File::for_path(&path).monitor_directory(gio::FileMonitorFlags::WATCH_MOVES, gio::Cancellable::NONE)
        else {
            continue;
        };
        let on_change = on_change.clone();
        let last = last.clone();
        monitor.connect_changed(move |_, _, _, _| {
            let on_change = on_change.clone();
            let last = last.clone();
            PENDING.with(|p| {
                if let Some(id) = p.borrow_mut().take() {
                    id.remove();
                }
                let id = glib::timeout_add_local_once(std::time::Duration::from_millis(400), move || {
                    PENDING.with(|p| p.borrow_mut().take());
                    let palette = load();
                    if *last.borrow() != palette {
                        *last.borrow_mut() = palette.clone();
                        on_change(palette);
                    }
                    arm(on_change, last);
                });
                *p.borrow_mut() = Some(id);
            });
        });
        monitors.push(monitor);
    }
    MONITORS.with(|m| *m.borrow_mut() = monitors);
}
