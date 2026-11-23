//! The notifications' words in Omarchy's language: what `cloud-calendar notify` shows on the desktop is read in the
//! language picked under Setup > Region > Language, as the app's window is. The command line's own output stays
//! English, for the scripts and agents that read it.
//!
//! The catalogs are the app's format (omui's): a JSON object from the English text to its translation, or for a
//! counted string to its plural forms by CLDR category, with `en.json` listing every string. This crate has no GTK, so
//! it reads them itself; the app's tests run omui's catalog check over this directory. Only English ships for now.

use serde_json::Value;
use std::path::{Path, PathBuf};

/// Every catalog, by Omarchy's language code.
const CATALOGS: &[(&str, &str)] = &[("en", include_str!("i18n/en.json"))];

/// The file `omarchy language` writes: one code on its first line.
pub fn language_file() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".config"))
        .join("omarchy/language")
}

/// The language picked in Omarchy, as its code (`da`, `pt-BR`): English without the file, or with anything on its
/// first line that is not a code.
pub fn language_in(file: &Path) -> String {
    let text = std::fs::read_to_string(file).unwrap_or_default();
    let code = text.lines().next().unwrap_or_default().trim();
    let valid = !code.is_empty() && code.len() <= 16 && code.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    if valid { code.to_owned() } else { "en".to_owned() }
}

/// The text for `text` in `language` from `catalogs`, per string: `pt-BR`, then `pt`, then English, then the text
/// itself. A counted string takes the form for `count`.
fn lookup(catalogs: &[(&str, &str)], language: &str, text: &str, count: Option<i64>) -> String {
    let base = language.split('-').next().unwrap_or(language);
    for code in [language, base, "en"] {
        let Some((_, source)) = catalogs.iter().find(|(c, _)| *c == code) else {
            continue;
        };
        let Ok(Value::Object(catalog)) = serde_json::from_str::<Value>(source) else {
            continue;
        };
        let found = match (catalog.get(text), count) {
            (Some(Value::String(t)), _) if !t.is_empty() => Some(t.clone()),
            (Some(Value::Object(forms)), Some(n)) => [plural(code, n), "other"]
                .iter()
                .find_map(|category| forms.get(*category).and_then(Value::as_str))
                .filter(|form| !form.is_empty())
                .map(str::to_owned),
            _ => None,
        };
        if let Some(found) = found {
            return match count {
                Some(n) => found.replace("{n}", &n.to_string()),
                None => found,
            };
        }
    }
    text.to_owned()
}

/// The CLDR category of `count` for `language`, for the forms the catalogs give. Only one and other are told apart
/// here, which is what English, and the languages like it, have; a language with more forms takes `other` for every
/// count but one until its catalog ships, along with its rules.
fn plural(language: &str, count: i64) -> &'static str {
    let none = ["ja", "ko", "zh", "vi", "th", "id", "ms"];
    if count == 1 && !none.contains(&language.split('-').next().unwrap_or(language)) {
        "one"
    } else {
        "other"
    }
}

/// `text`, a string literal, in Omarchy's language.
pub fn tr(text: &str) -> String {
    lookup(CATALOGS, &language_in(&language_file()), text, None)
}

/// A counted string in Omarchy's language, its `{n}` the count: `trn("in {n} min", 5)`.
pub fn trn(text: &str, count: i64) -> String {
    lookup(CATALOGS, &language_in(&language_file()), text, Some(count))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_string_falls_back_toward_english_one_step_at_a_time() {
        let catalogs = [
            (
                "en",
                r#"{"starting now": "starting now", "in {n} min": {"one": "in {n} min", "other": "in {n} min"}}"#,
            ),
            (
                "pt",
                r#"{"starting now": "a começar", "in {n} min": {"one": "em {n} min", "other": "em {n} min"}}"#,
            ),
            ("pt-BR", r#"{"starting now": "começando agora"}"#),
        ];
        assert_eq!(lookup(&catalogs, "pt-BR", "starting now", None), "começando agora");
        assert_eq!(lookup(&catalogs, "pt-BR", "in {n} min", Some(5)), "em 5 min");
        assert_eq!(lookup(&catalogs, "pt", "starting now", None), "a começar");
        assert_eq!(lookup(&catalogs, "da", "in {n} min", Some(1)), "in 1 min");
        assert_eq!(
            lookup(&catalogs, "da", "not in any catalog", None),
            "not in any catalog"
        );
    }

    #[test]
    fn the_language_is_the_files_first_line() {
        let dir = std::env::temp_dir().join(format!("cloud-calendar-language-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("language");
        assert_eq!(language_in(&file), "en", "no file is English");
        std::fs::write(&file, "pt-BR\n").unwrap();
        assert_eq!(language_in(&file), "pt-BR");
        std::fs::write(&file, "../../etc\n").unwrap();
        assert_eq!(language_in(&file), "en");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
