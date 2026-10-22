//! Applies the theme: CSS for widgets, the palette for cairo drawing. Can be
//! called again with new colors; the stylesheet is swapped in place.

use std::cell::RefCell;

use gtk::{gdk, CssProvider};

use crate::config::{self, Theme};
use crate::palette;

const BASE_CSS: &str = include_str!("style.css");

thread_local! {
    static PROVIDER: RefCell<Option<CssProvider>> = const { RefCell::new(None) };
}

pub fn apply(theme: &Theme) {
    let accent = config::parse_hex(&theme.accent);
    let secondary = config::parse_hex(&theme.secondary);
    if let (Some(a), Some(s)) = (accent, secondary) {
        palette::set_theme(a, s);
    }

    // Rewrite the two themable `@define-color`s; everything else in the
    // stylesheet refers to them by name.
    let css: String = BASE_CSS
        .lines()
        .map(|line| {
            if line.starts_with("@define-color pink ") {
                format!("@define-color pink {};", theme.accent)
            } else if line.starts_with("@define-color lavender ") {
                format!("@define-color lavender {};", theme.secondary)
            } else {
                line.to_owned()
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
