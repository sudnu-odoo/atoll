//! Palette for cairo drawing. Must match the `@define-color`s in style.css.

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
pub const LAVENDER: Rgb = hex(0xc9b8ff);
pub const PINK: Rgb = hex(0xff8fb1);
pub const MUTED: Rgb = hex(0x6272a4);
