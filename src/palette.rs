//! Palette for cairo drawing. The fixed colors match style.css; accent and
//! secondary come from the theme settings and are read at draw time, so a
//! theme change shows on the next redraw.

use std::cell::Cell;

pub type Rgb = (f64, f64, f64);

const fn hex(v: u32) -> Rgb {
    (
        ((v >> 16) & 0xff) as f64 / 255.0,
        ((v >> 8) & 0xff) as f64 / 255.0,
        (v & 0xff) as f64 / 255.0,
    )
}

thread_local! {
    // GTK drawing happens on the main thread only.
    static ACCENT: Cell<Rgb> = const { Cell::new(hex(0xff8fb1)) };
    static SECONDARY: Cell<Rgb> = const { Cell::new(hex(0xc9b8ff)) };
    static DARK: Cell<bool> = const { Cell::new(true) };
}

/// The notch itself. Dark: opaque near-black, like a hardware notch
/// (anything translucent lets the GNOME clock underneath bleed through).
/// Light: an opaque near-white.
pub fn notch() -> Rgb {
    if is_dark() {
        hex(0x0b0b12)
    } else {
        hex(0xf6f4fb)
    }
}

pub fn bg_raised() -> Rgb {
    if is_dark() {
        hex(0x44475a)
    } else {
        hex(0xe3deee)
    }
}

pub fn muted() -> Rgb {
    if is_dark() {
        hex(0x6272a4)
    } else {
        hex(0x4e5478)
    }
}

pub fn is_dark() -> bool {
    DARK.with(Cell::get)
}

pub fn set_dark(dark: bool) {
    DARK.with(|c| c.set(dark));
}

/// A color tuned for the dark notch, made readable on the light one
/// (4.5:1 or better on its background and raised surfaces).
pub fn for_light((r, g, b): Rgb) -> Rgb {
    const KEEP: f64 = 0.55;
    (r * KEEP, g * KEEP, b * KEEP)
}

/// Accent (pink by default): progress, active states, excited eyes.
pub fn pink() -> Rgb {
    ACCENT.with(Cell::get)
}

/// Secondary (lavender by default): labels, meters, sleepy eyes.
pub fn lavender() -> Rgb {
    SECONDARY.with(Cell::get)
}

pub fn set_theme(accent: Rgb, secondary: Rgb) {
    ACCENT.with(|c| c.set(accent));
    SECONDARY.with(|c| c.set(secondary));
}
