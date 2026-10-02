use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    sync::{
        OnceLock,
        atomic::{AtomicU8, Ordering},
    },
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[repr(u8)]
pub enum Locale {
    En,
    Es,
    Ru,
}

impl Default for Locale {
    fn default() -> Self {
        Self::system()
    }
}

impl Locale {
    pub fn code(self) -> &'static str {
        match self {
            Self::En => "en",
            Self::Es => "es",
            Self::Ru => "ru",
        }
    }
    pub fn parse(code: &str) -> Option<Self> {
        match code {
            "en" => Some(Self::En),
            "es" => Some(Self::Es),
            "ru" => Some(Self::Ru),
            _ => None,
        }
    }
    pub fn system() -> Self {
        for key in [
            "STABILIZER_LANG",
            "LANGUAGE",
            "LC_ALL",
            "LC_MESSAGES",
            "LANG",
        ] {
            if let Ok(value) = std::env::var(key) {
                let code = value.split(['_', '-', '.', ':']).next().unwrap_or("");
                if let Some(locale) = Self::parse(code) {
                    return locale;
                }
            }
        }
        Self::En
    }
    fn catalog_index(self) -> usize {
        match self {
            Self::Ru => 0,
            Self::En => 1,
            Self::Es => 2,
        }
    }
}

static CURRENT: AtomicU8 = AtomicU8::new(255);
pub fn set_locale(locale: Locale) {
    CURRENT.store(locale as u8, Ordering::Relaxed);
}
pub fn locale() -> Locale {
    match CURRENT.load(Ordering::Relaxed) {
        0 => Locale::En,
        1 => Locale::Es,
        2 => Locale::Ru,
        _ => Locale::system(),
    }
}
pub fn t(text: &str) -> String {
    translate(locale(), text)
}

struct Pattern {
    literals: Vec<String>,
    slots: Vec<String>,
}
impl Pattern {
    fn new(text: &str) -> Self {
        let mut literals = Vec::new();
        let mut slots = Vec::new();
        let mut rest = text;
        while let Some(start) = rest.find('{') {
            let Some(end) = rest[start..].find('}') else {
                break;
            };
            literals.push(rest[..start].to_string());
            slots.push(rest[start + 1..start + end].to_string());
            rest = &rest[start + end + 1..];
        }
        literals.push(rest.to_string());
        Self { literals, slots }
    }
    fn capture(&self, text: &str) -> Option<Vec<String>> {
        let mut rest = text.strip_prefix(&self.literals[0])?;
        let mut values = Vec::new();
        for (i, _) in self.slots.iter().enumerate() {
            let next = &self.literals[i + 1];
            let end = if next.is_empty() && i + 1 == self.slots.len() {
                rest.len()
            } else {
                rest.find(next)?
            };
            values.push(rest[..end].to_string());
            rest = &rest[end..];
            rest = rest.strip_prefix(next)?;
        }
        if rest.is_empty() { Some(values) } else { None }
    }
    fn render(&self, values: &[String], locale: Locale, depth: usize) -> String {
        let mut output = self.literals[0].clone();
        for (i, slot) in self.slots.iter().enumerate() {
            let mut value = values.get(i).cloned().unwrap_or_default();
            if slot.starts_with(':') && value.replace(',', ".").parse::<f64>().is_ok() {
                value = if locale == Locale::En {
                    value.replace(',', ".")
                } else {
                    value.replace('.', ",")
                };
            } else if depth < 4 {
                value = translate_inner(locale, &value, depth + 1);
            }
            output.push_str(&value);
            output.push_str(&self.literals[i + 1]);
        }
        output
    }
}

struct Entry {
    patterns: [Pattern; 3],
    specificity: usize,
}
fn catalog() -> &'static Vec<Entry> {
    static CATALOG: OnceLock<Vec<Entry>> = OnceLock::new();
    CATALOG.get_or_init(|| {
        let documents: [BTreeMap<String, String>; 3] = [
            serde_json::from_str(include_str!("../locales/ru.json")).expect("Russian catalog"),
            serde_json::from_str(include_str!("../locales/en.json")).expect("English catalog"),
            serde_json::from_str(include_str!("../locales/es.json")).expect("Spanish catalog"),
        ];
        let mut entries = Vec::new();
        for key in documents[0].keys() {
            let patterns = std::array::from_fn(|i| Pattern::new(&documents[i][key]));
            let specificity = patterns[0].literals.iter().map(String::len).sum();
            entries.push(Entry {
                patterns,
                specificity,
            });
        }
        entries.sort_by_key(|e| std::cmp::Reverse(e.specificity));
        entries
    })
}

fn translate_inner(locale: Locale, text: &str, depth: usize) -> String {
    for entry in catalog() {
        for pattern in &entry.patterns {
            if let Some(values) = pattern.capture(text) {
                return entry.patterns[locale.catalog_index()].render(&values, locale, depth);
            }
        }
    }
    if depth < 4 {
        for separator in [": ", " · ", " / "] {
            if let Some((left, right)) = text.split_once(separator) {
                let translated_left = translate_inner(locale, left, depth + 1);
                let translated_right = translate_inner(locale, right, depth + 1);
                if translated_left != left || translated_right != right {
                    return format!("{translated_left}{separator}{translated_right}");
                }
            }
        }
    }
    text.to_string()
}

pub fn translate(locale: Locale, text: &str) -> String {
    translate_inner(locale, text, 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn catalog_keys_and_placeholders_match_for_all_languages() {
        let ru: BTreeMap<String, String> =
            serde_json::from_str(include_str!("../locales/ru.json")).unwrap();
        for data in [
            include_str!("../locales/en.json"),
            include_str!("../locales/es.json"),
        ] {
            let translated: BTreeMap<String, String> = serde_json::from_str(data).unwrap();
            assert_eq!(
                ru.keys().collect::<Vec<_>>(),
                translated.keys().collect::<Vec<_>>()
            );
            for (key, value) in &translated {
                assert!(!value.is_empty(), "{key}");
                assert_eq!(
                    Pattern::new(&ru[key]).slots,
                    Pattern::new(value).slots,
                    "{key}"
                );
            }
        }
    }
    #[test]
    fn dynamic_messages_and_saved_events_switch_languages() {
        assert_eq!(
            translate(Locale::Es, "Доступно RAM: 1.5 ГиБ"),
            "RAM disponible: 1,5 GiB"
        );
        assert_eq!(
            translate(Locale::En, "RAM disponible: 1,5 GiB"),
            "Available RAM: 1.5 GiB"
        );
        assert_eq!(
            translate(Locale::Es, "Применено · Высокий приоритет"),
            "Aplicada · Prioridad alta"
        );
        assert_eq!(
            translate(Locale::Ru, "Applied · High priority"),
            "Применено · Высокий приоритет"
        );
        assert_eq!(
            translate(Locale::En, "Unrecognized /proc/path"),
            "Unrecognized /proc/path"
        );
    }
}
