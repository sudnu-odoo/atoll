//! User settings, from ~/.config/atoll/config.json. Missing keys take their
//! defaults, so old files keep working as settings are added. The file is
//! created with all defaults on first run, so there's something to edit.

use serde::{Deserialize, Serialize};

use crate::store;

const FILE: &str = "config.json";

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub features: Features,
    pub theme: Theme,
    pub water: Water,
}

/// Each feature can be switched off; its card or peek then never appears.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Features {
    pub music: bool,
    pub lyrics: bool,
    pub eyes: bool,
    pub water: bool,
    pub bluetooth_peek: bool,
    pub charging_peek: bool,
    /// Claude Code prompts and peeks (needs the hooks installed).
    pub claude: bool,
}

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

impl Default for Features {
    fn default() -> Self {
        Self {
            music: true,
            lyrics: true,
            eyes: true,
            water: true,
            bluetooth_peek: true,
            charging_peek: true,
            claude: true,
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
