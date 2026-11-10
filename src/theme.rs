//! Applies the theme: CSS for widgets, the palette for cairo drawing. Can be
//! called again with new colors; the stylesheet is swapped in place.
//!
//! Light or dark follows GNOME's style setting (read only, never written)
//! unless the user fixed it in Settings, and switches live.

use std::cell::RefCell;

use gtk::prelude::*;
use gtk::{gdk, gio, CssProvider};

use crate::config::{self, Appearance, Theme};
use crate::palette::{self, Rgb};

const BASE_CSS: &str = include_str!("style.css");
const INTERFACE: &str = "org.gnome.desktop.interface";

/// The light notch's own colors (the dark ones are style.css's defaults).
const LIGHT: &[(&str, &str)] = &[
    ("bg", "#F6F4FB"),
    ("bg_raised", "#E3DEEE"),
    ("text", "#1F1B2E"),
    ("muted", "#4E5478"),
];

thread_local! {
    static PROVIDER: RefCell<Option<CssProvider>> = const { RefCell::new(None) };
    /// The system style setting, kept alive for its change signal.
    static SYSTEM: RefCell<Option<gio::Settings>> = const { RefCell::new(None) };
}

fn system_settings() -> Option<gio::Settings> {
    // Avoid an abort if the schema is somehow missing.
    gio::SettingsSchemaSource::default()?.lookup(INTERFACE, true)?;
    Some(gio::Settings::new(INTERFACE))
}

/// Whether GNOME is set to its dark style.
fn system_is_dark() -> bool {
    let Some(s) = system_settings() else {
        return true;
    };
    if s.string("color-scheme") == "prefer-dark" {
        return true;
    }
    // Older themes signal dark only through their name ("Yaru-dark").
    s.string("gtk-theme").to_lowercase().contains("dark")
}

pub fn is_dark(appearance: Appearance) -> bool {
    // Previews can pick one without touching the user's setting.
    if crate::preview::active() {
        if let Ok(v) = std::env::var("ATOLL_APPEARANCE") {
            return v != "light";
        }
    }
    match appearance {
        Appearance::System => system_is_dark(),
        Appearance::Dark => true,
        Appearance::Light => false,
    }
}

/// Calls `changed` when GNOME's style switches between light and dark.
pub fn watch_system(changed: impl Fn() + 'static) {
    let Some(s) = system_settings() else { return };
    let changed = std::rc::Rc::new(changed);
    for key in ["color-scheme", "gtk-theme"] {
        let f = changed.clone();
        s.connect_changed(Some(key), move |_, _| f());
    }
    SYSTEM.with(|cell| cell.replace(Some(s)));
}

fn to_hex((r, g, b): Rgb) -> String {
    let c = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("#{:02X}{:02X}{:02X}", c(r), c(g), c(b))
}

pub fn apply(theme: &Theme) {
    let dark = is_dark(theme.appearance);
    palette::set_dark(dark);
    let tune = |hex: &str| {
        let rgb = config::parse_hex(hex)?;
        Some(if dark { rgb } else { palette::for_light(rgb) })
    };
    let (accent, secondary) = (tune(&theme.accent), tune(&theme.secondary));
    if let (Some(a), Some(s)) = (accent, secondary) {
        palette::set_theme(a, s);
    }
    let purple = tune("#BD93F9").map(to_hex);

    // Rewrite the `@define-color`s; everything else in the stylesheet refers
    // to them by name.
    let css: String = BASE_CSS
        .lines()
        .map(|line| {
            let Some(name) = line
                .strip_prefix("@define-color ")
                .and_then(|rest| rest.split_whitespace().next())
            else {
                return line.to_owned();
            };
            let value = match name {
                "pink" => accent.map(to_hex),
                "lavender" => secondary.map(to_hex),
                "purple" => purple.clone(),
                _ if dark => None,
                _ => LIGHT
                    .iter()
                    .find(|(n, _)| *n == name)
                    .map(|(_, v)| (*v).to_owned()),
            };
            match value {
                Some(v) => format!("@define-color {name} {v};"),
                None => line.to_owned(),
            }
        })
        .collect::<Vec<_>>()
        .join("\n");

    let Some(display) = gdk::Display::default() else {
        tracing::error!("no display; CSS not loaded");
        return;
    };
    PROVIDER.with(|cell| {
        let mut slot = cell.borrow_mut();
        let provider = slot.get_or_insert_with(|| {
            let p = CssProvider::new();
            gtk::style_context_add_provider_for_display(
                &display,
                &p,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
            p
        });
        provider.load_from_string(&css);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trip() {
        assert_eq!(to_hex((1.0, 0.5, 0.0)), "#FF8000");
        let dark_pink = config::parse_hex("#FF8FB1").unwrap();
        assert_eq!(to_hex(palette::for_light(dark_pink)), "#8C4F61");
    }
}
