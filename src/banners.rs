//! GNOME's notification banners, turned off while Atoll shows notifications
//! itself (the user's choice in Settings, and the one gsettings key Atoll may
//! write; see CLAUDE.md). A marker file records that Atoll did it, so the
//! banners are always given back: on switch-off, on quit, and on the next
//! start after a crash.

use gtk::gio;
use gtk::prelude::*;

use crate::store;

const SCHEMA: &str = "org.gnome.desktop.notifications";
const KEY: &str = "show-banners";
const MARKER: &str = "banners-hidden-by-atoll";

fn settings() -> Option<gio::Settings> {
    // Avoid an abort if the schema is somehow missing.
    gio::SettingsSchemaSource::default()?.lookup(SCHEMA, true)?;
    Some(gio::Settings::new(SCHEMA))
}

fn marker() -> Option<std::path::PathBuf> {
    store::data_file(MARKER)
}

/// Hides GNOME's banners, if they're shown, remembering that we did.
pub fn hide() {
    let (Some(s), Some(m)) = (settings(), marker()) else {
        return;
    };
    if !s.boolean(KEY) {
        return; // already off (by the user or by us): nothing to undo later
    }
    if let Some(dir) = m.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if std::fs::write(&m, b"").is_err() {
        return; // without the marker we couldn't promise to restore
    }
    if let Err(e) = s.set_boolean(KEY, false) {
        tracing::warn!("cannot hide GNOME banners: {e}");
        let _ = std::fs::remove_file(&m);
    }
    gio::Settings::sync();
}

/// Gives the banners back, but only if Atoll was the one that hid them.
pub fn restore() {
    let (Some(s), Some(m)) = (settings(), marker()) else {
        return;
    };
    if !m.exists() {
        return;
    }
    if let Err(e) = s.set_boolean(KEY, true) {
        tracing::warn!("cannot restore GNOME banners: {e}");
        return;
    }
    gio::Settings::sync();
    let _ = std::fs::remove_file(&m);
}
