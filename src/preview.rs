//! Developer preview: `ATOLL_SHOT=/tmp/card.png ATOLL_SHOW=<card> atoll`
//! renders one card to a PNG and exits. It runs beside the user's own Atoll
//! without touching it: its own (non-unique) app, a window placed far off
//! screen, and none of the side effects (Claude socket, GNOME banners, login
//! item, shell snippet).

use std::path::PathBuf;

/// Where to save the picture, when running as a preview.
pub fn shot_path() -> Option<PathBuf> {
    std::env::var_os("ATOLL_SHOT").map(PathBuf::from)
}

pub fn active() -> bool {
    shot_path().is_some()
}

/// Off any monitor, so nothing appears on the user's screen.
pub const OFFSCREEN_X: i32 = -10_000;
