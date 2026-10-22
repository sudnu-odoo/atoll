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

/// Opaque near-black, like a hardware notch: anything translucent lets the
/// GNOME clock underneath bleed through and blends into the dark bar.
pub const NOTCH: Rgb = hex(0x0b0b12);
pub const BG_RAISED: Rgb = hex(0x44475a);
pub const MUTED: Rgb = hex(0x6272a4);

thread_local! {
    // GTK drawing happens on the main thread only.
    static ACCENT: Cell<Rgb> = const { Cell::new(hex(0xff8fb1)) };
    static SECONDARY: Cell<Rgb> = const { Cell::new(hex(0xc9b8ff)) };
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
