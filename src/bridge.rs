//! The optional Atoll Bridge extension, seen from Settings: its state, and
//! the switch the user flips to turn it on or off. Goes through GNOME
//! Shell's own org.gnome.Shell.Extensions service, the same one the
//! Extensions app uses; nothing happens unless the user flips the switch.

use gtk::prelude::*;
use gtk::{gio, glib};

pub const UUID: &str = "atoll-bridge@atoll.dev";

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Status {
    /// Not on disk (neither the package nor install.sh put it there).
    NotInstalled,
    /// On disk, but GNOME Shell hasn't seen it yet: it only looks at login.
    NeedsRelogin,
    Off,
    On,
    /// GNOME Shell couldn't load it (see the journal).
    Error,
}

impl Status {
    pub fn describe(self) -> &'static str {
        match self {
            Status::NotInstalled => "Not installed",
            Status::NeedsRelogin => "Installed: log out and back in, then switch it on",
            Status::Off => "Off: clicks bring the app forward",
            Status::On => "On: clicks open the exact chat or item",
            Status::Error => "GNOME Shell couldn't load it",
        }
    }
}

fn on_disk() -> bool {
    let user = glib::user_data_dir()
        .join("gnome-shell/extensions")
        .join(UUID);
    let system = std::path::Path::new("/usr/share/gnome-shell/extensions").join(UUID);
    user.is_dir() || system.is_dir()
}

async fn call(method: &str) -> Result<glib::Variant, glib::Error> {
    let conn = gio::bus_get_future(gio::BusType::Session).await?;
    conn.call_future(
        Some("org.gnome.Shell.Extensions"),
        "/org/gnome/Shell/Extensions",
        "org.gnome.Shell.Extensions",
        method,
        Some(&(UUID,).to_variant()),
        None,
        gio::DBusCallFlags::NONE,
        3000,
    )
    .await
}

pub async fn status() -> Status {
    if !on_disk() {
        return Status::NotInstalled;
    }
    let info = match call("GetExtensionInfo").await {
        Ok(v) => v.child_value(0),
        Err(e) => {
            tracing::debug!("extension info unavailable: {e}");
            return Status::NeedsRelogin;
        }
    };
    let state = glib::VariantDict::new(Some(&info))
        .lookup::<f64>("state")
        .ok()
        .flatten();
    // GNOME Shell's ExtensionState values.
    match state {
        None => Status::NeedsRelogin,
        Some(s) if s == 1.0 || s == 8.0 => Status::On,
        Some(s) if s == 3.0 || s == 4.0 => Status::Error,
        Some(_) => Status::Off,
    }
}

/// Turns the extension on or off for this user, then reports the new state.
pub async fn set(on: bool) -> Status {
    let method = if on {
        "EnableExtension"
    } else {
        "DisableExtension"
    };
    if let Err(e) = call(method).await {
        tracing::warn!("cannot {method} {UUID}: {e}");
    }
    status().await
}
