//! User settings, from ~/.config/atoll/config.json. Missing keys take their
//! defaults, so old files keep working as settings are added. The file is
//! created with all defaults on first run, so there's something to edit.

use serde::{Deserialize, Serialize};

use crate::store;

const FILE: &str = "config.json";

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Atoll switched off in Settings: no notch anywhere, GNOME's banners
    /// back. Stored as "paused" so files without the key mean on.
    pub paused: bool,
    /// Start Atoll at login (~/.config/autostart/atoll.desktop).
    pub autostart: bool,
    pub features: Features,
    pub theme: Theme,
    pub water: Water,
    pub commands: Commands,
    pub calendar: Calendar,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Calendar {
    /// Peek this many minutes before a meeting starts.
    pub remind_before_min: u32,
}

impl Default for Calendar {
    fn default() -> Self {
        Self {
            remind_before_min: 5,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Commands {
    /// Only commands that ran at least this long get a peek.
    pub min_seconds: u32,
}

impl Default for Commands {
    fn default() -> Self {
        Self { min_seconds: 15 }
    }
}

/// Each feature can be switched off; its card or peek then never appears.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Features {
    pub music: bool,
    pub lyrics: bool,
    /// English lines under foreign lyrics (sends lyric text to Google).
    pub translate_lyrics: bool,
    pub eyes: bool,
    pub water: bool,
    /// Claude Code prompts and peeks (needs the hooks installed).
    pub claude: bool,
    pub todo: bool,
    /// Peek when a long terminal command finishes (needs the shell line).
    pub command_peek: bool,
    /// Next meetings from GNOME Online Accounts (Google Calendar).
    pub calendar: bool,
    /// File shelf: drop files on the notch, drag them out later.
    pub shelf: bool,
    /// Quick settings: Wi-Fi, Bluetooth, volume, brightness...
    pub controls: bool,
    /// Show notifications in the notch, and hide GNOME's own banners.
    pub notifications: bool,
    /// Order of the expanded cards. "music" is the clock card when nothing
    /// plays. Unknown names are dropped and missing ones appended.
    pub order: Vec<String>,
}

/// Every card that can appear in `order`, in the default order.
pub const CARDS: &[&str] = &[
    "music",
    "controls",
    "notifications",
    "calendar",
    "water",
    "todo",
    "shelf",
];

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Theme {
    /// Accent: progress, active buttons, excited eyes. "#RRGGBB".
    pub accent: String,
    /// Secondary: labels, sleepy eyes, meters. "#RRGGBB".
    pub secondary: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Water {
    pub goal_ml: u32,
    /// What one "Drink" click adds.
    pub glass_ml: u32,
    pub reminders: bool,
    /// Remind this long after the last drink.
    pub remind_every_min: u32,
    /// Reminders only between these hours (local time, 0–24).
    pub active_from_hour: u32,
    pub active_to_hour: u32,
}

impl Config {
    /// Notifications take over GNOME's banners only while Atoll is on.
    pub fn hides_banners(&self) -> bool {
        !self.paused && self.features.notifications
    }
}

impl Default for Features {
    fn default() -> Self {
        Self {
            music: true,
            lyrics: true,
            translate_lyrics: false,
            eyes: true,
            water: true,
            claude: true,
            todo: true,
            command_peek: true,
            calendar: true,
            shelf: true,
            controls: true,
            notifications: true,
            order: CARDS.iter().map(|c| (*c).to_owned()).collect(),
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            accent: "#FF8FB1".into(),
            secondary: "#C9B8FF".into(),
        }
    }
}

impl Default for Water {
    fn default() -> Self {
        Self {
            goal_ml: 2000,
            glass_ml: 250,
            reminders: true,
            remind_every_min: 60,
            active_from_hour: 9,
            active_to_hour: 21,
        }
    }
}

impl Config {
    pub fn load() -> Self {
        let Some(path) = store::config_file(FILE) else {
            tracing::warn!("no HOME; using default settings");
            return Self::default();
        };
        match store::read::<Self>(&path) {
            Ok(Some(config)) => config.sanitized(),
            Ok(None) => {
                let config = Self::default();
                // Seed the file so the settings are discoverable.
                if let Err(e) = store::write(&path, &config) {
                    tracing::warn!("cannot create settings file: {e}");
                }
                config
            }
            Err(e) => {
                // Never overwrite a file the user is editing; just run with
                // defaults until it parses again.
                tracing::warn!("settings unreadable, using defaults: {e}");
                Self::default()
            }
        }
    }

    /// Like `load`, but never creates the file or logs: for the hook helper,
    /// which runs after every shell command and must stay silent.
    pub fn load_quiet() -> Self {
        store::config_file(FILE)
            .and_then(|p| store::read::<Self>(&p).ok().flatten())
            .map(Self::sanitized)
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let Some(path) = store::config_file(FILE) else {
            return;
        };
        if let Err(e) = store::write(&path, self) {
            tracing::warn!("cannot save settings: {e}");
        }
    }

    /// Clamps values that would break the UI (zero goal, bad colors).
    pub fn sanitized(mut self) -> Self {
        let defaults = Self::default();
        let mut order: Vec<String> = Vec::new();
        for name in &self.features.order {
            if CARDS.contains(&name.as_str()) && !order.contains(name) {
                order.push(name.clone());
            }
        }
        for name in CARDS {
            if !order.iter().any(|o| o == name) {
                order.push((*name).to_owned());
            }
        }
        self.features.order = order;
        if parse_hex(&self.theme.accent).is_none() {
            self.theme.accent = defaults.theme.accent;
        }
        if parse_hex(&self.theme.secondary).is_none() {
            self.theme.secondary = defaults.theme.secondary;
        }
        let w = &mut self.water;
        w.goal_ml = w.goal_ml.clamp(250, 10_000);
        w.glass_ml = w.glass_ml.clamp(50, 2_000);
        w.remind_every_min = w.remind_every_min.clamp(10, 600);
        w.active_from_hour = w.active_from_hour.min(23);
        w.active_to_hour = w.active_to_hour.clamp(w.active_from_hour + 1, 24);
        self.commands.min_seconds = self.commands.min_seconds.clamp(3, 3600);
        self.calendar.remind_before_min = self.calendar.remind_before_min.clamp(1, 60);
        self
    }
}

/// "#RRGGBB" -> (r, g, b) in 0..1.
pub fn parse_hex(s: &str) -> Option<(f64, f64, f64)> {
    let hex = s.strip_prefix('#')?;
    if hex.len() != 6 {
        return None;
    }
    let v = u32::from_str_radix(hex, 16).ok()?;
    let c = |shift: u32| f64::from((v >> shift) & 0xff) / 255.0;
    Some((c(16), c(8), c(0)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_colors() {
        assert_eq!(parse_hex("#FF0000"), Some((1.0, 0.0, 0.0)));
        assert_eq!(
            parse_hex("#ff8fb1").map(|c| (c.0 * 255.0).round()),
            Some(255.0)
        );
        for bad in ["FF0000", "#FFF", "#GG0000", ""] {
            assert_eq!(parse_hex(bad), None, "{bad}");
        }
    }

    #[test]
    fn sanitize_clamps_and_fixes_colors() {
        let mut c = Config::default();
        c.theme.accent = "pink".into();
        c.water.goal_ml = 0;
        c.water.remind_every_min = 1;
        c.water.active_from_hour = 30;
        c.water.active_to_hour = 5;
        c.commands.min_seconds = 0;
        let c = c.sanitized();
        assert_eq!(c.theme.accent, Theme::default().accent);
        assert_eq!(c.water.goal_ml, 250);
        assert_eq!(c.water.remind_every_min, 10);
        assert_eq!(c.water.active_from_hour, 23);
        assert_eq!(c.water.active_to_hour, 24, "end stays after start");
        assert_eq!(c.commands.min_seconds, 3);
    }

    #[test]
    fn card_order_drops_unknown_and_appends_missing() {
        let mut c = Config::default();
        c.features.order = vec![
            "shelf".into(),
            "bogus".into(),
            "music".into(),
            "shelf".into(),
        ];
        let order = c.sanitized().features.order;
        assert_eq!(order[..2], ["shelf".to_owned(), "music".to_owned()]);
        assert_eq!(order.len(), CARDS.len(), "every card exactly once");
        for card in CARDS {
            assert!(order.iter().any(|o| o == card), "{card} missing");
        }
    }

    #[test]
    fn old_files_load_with_defaults() {
        // A file from before most settings existed, with a since-removed key.
        let json = r#"{"features": {"music": false, "bluetooth_peek": true}}"#;
        let c: Config = serde_json::from_str(json).unwrap();
        assert!(!c.features.music);
        assert!(c.features.water, "missing keys take defaults");
        assert_eq!(c.water.goal_ml, 2000);
    }
}
