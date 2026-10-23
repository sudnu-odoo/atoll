# Atoll: a Dynamic Island for GNOME (Rust)

Atoll is a small, standalone Rust app that draws a pill-shaped "island" in the center of the GNOME top bar. It shows the clock when idle and expands on hover into cards: music, lyrics, timers, and system peeks.

It is **not** a GNOME Shell extension. It runs as its own process, so if it crashes, only the island disappears and the desktop keeps working. This is a hard requirement (see "Safety rules").

## Target machine

- Ubuntu 24.04 LTS, GNOME Shell 46 (mutter 46.2), **Xorg session only** (no Wayland session available)
- Single AMD GPU (amdgpu)
- Monitors (from `xrandr --listmonitors`):
  - `DisplayPort-7` 1920x1080 at +0+0 (**primary**, shows the GNOME top bar)
  - `DisplayPort-8` 1920x1080 at +1920+0
  - `eDP` (laptop) 1920x1200 at +870+1080
  - Undocked, only `eDP` exists and becomes primary. Layout changes at runtime.
- The GNOME top bar is transparent (Transparent Top Bar extension) and uses the Dracula Shell theme.
- User: `odoo`, works as an Odoo developer. Rust tooling may need installing.

## Safety rules (from a past login black-screen incident)

1. **Never** create, install, enable, or modify GNOME Shell extensions.
2. **Never** write to dconf/gsettings, except keys this project owns (it should not need any). One exception (the user's decision): with Settings → Notifications on, Atoll sets `org.gnome.desktop.notifications show-banners` to false so notifications show only in the notch, and restores it when the setting is turned off or Atoll quits (and on the next start after a crash).
3. **Never** touch `~/.config/monitors.xml`, GDM config, or `/etc/gdm3/`.
4. Everything runs as a normal user process. No root at runtime.
5. Installing system packages with `apt` is allowed, but list them and ask before running `sudo`.
6. The app must exit cleanly on SIGTERM/SIGINT and must never block the desktop.

## Stack

- Rust stable (edition 2021)
- UI: `gtk4` (gtk4-rs, feature `v4_14`, matching Ubuntu 24.04's GTK 4.14), styled with GTK CSS
- X11 window control: `gdk4-x11` + `x11rb` (window type, stacking, input shape, positioning)
- D-Bus: `zbus` (MPRIS, UPower, NetworkManager, BlueZ)
- HTTP (lyrics): `reqwest` with `rustls`
- Config/data: `serde` + JSON in `~/.local/share/atoll/` and `~/.config/atoll/`
- Logging: `tracing` + `tracing-subscriber`, controlled by `RUST_LOG`

Async work must integrate with the GLib main loop (for example `glib::MainContext::spawn_local` or a channel back to the UI thread). Never block the UI thread.

## Window strategy (verify first)

GNOME Shell draws its top bar above normal windows. To appear *over* the bar, the island window is expected to need X11 **override-redirect**, which GNOME places in its top window group, above the panel. This must be verified on the real machine before building features (roadmap step 0).

Requirements for the island window:

- Transparent background (RGBA visual, compositor is active), only the pill is visible
- Positioned at the horizontal center of the **primary** monitor, top edge at y = monitor top
- Never steals keyboard focus; not in the taskbar, dock, Alt+Tab, or overview. One exception (the user's decision): while the user is typing in the notch after clicking a text action (e.g. adding a to-do), it takes focus and hands it back to the previous window on Enter, Esc, or leaving
- Input shape limited to the pill's current bounds, so clicks elsewhere on the top bar pass through to GNOME
- Re-positions itself when monitors change (listen for RandR events)
- Hides or steps aside when a fullscreen window is active

If override-redirect does not place it above the bar, stop and report findings before trying alternatives. Fallback: place the island directly below the top bar.

## Design

Inspiration: Apple's Dynamic Island and the "Nusa" notch app for Windows (https://rama2555.github.io/nusa/). Take the **concept** only (a pill that expands on hover into cards). Do **not** copy Nusa's name, artwork, icons, or layouts pixel-for-pixel. Atoll has its own identity that matches the user's desktop. The to-do card follows Nusa's to-do layout (the user's decision), in Atoll's palette. Idle eyes are allowed (the user's decision), drawn Atoll's own way: two sleepy, half-closed lavender eyes that blink slowly and sometimes doze off.

Palette (Dracula, tuned to the user's eclipse wallpaper):

| Token | Hex | Use |
|---|---|---|
| `bg` | `#282A36` at ~92% opacity | Pill background |
| `bg-raised` | `#44475A` | Cards, hover states |
| `text` | `#F2EEFF` | Primary text |
| `lavender` | `#C9B8FF` | Secondary text, labels |
| `pink` | `#FF8FB1` | Accents, progress, active states |
| `purple` | `#BD93F9` | Highlights |
| `muted` | `#6272A4` | Tertiary text |

Typography: `Ubuntu` (Light/Regular/Medium) for UI text, `JetBrainsMono Nerd Font` for numbers/timers, `Symbols Nerd Font` for icons. Labels use uppercase with letter-spacing, like the user's desktop widgets.

Motion: spring-like expand and collapse (~250 ms), no bouncy overshoot. Collapsed pill: about 200x32 px. Expanded: up to about 420x180 px, depending on the card.

## Performance requirements

- Idle CPU must be ~0%: redraw only while animating, use D-Bus signals instead of polling wherever possible
- Idle RSS target: under 40 MB
- The clock updates once per minute, aligned to the minute boundary

## Roadmap

- **Step 0: window prototype.** A transparent pill at the top center of the primary monitor, over the GNOME top bar. Hover changes its color. Verify layering, transparency, click-through outside the pill, and no focus stealing. Nothing else.
- **v0.1: clock + music.** Collapsed: clock. Hover: expand to a music card via MPRIS (title, artist, album art, play/pause, next, previous, progress). Collapsed shows a small "now playing" indicator while music plays.
- **v0.2: lyrics + peeks.** Synced lyrics via LRCLIB (`https://lrclib.net`, sends only title, artist, duration). Temporary peeks: charger connected/disconnected (UPower), Wi-Fi lost/restored (NetworkManager), Bluetooth headphones connected with battery (BlueZ).
- **v0.3: focus.** Pomodoro timer and a to-do list with reminders, stored locally.
- **Later:** file shelf (drag and drop), water reminder, Atoll's own idle animation, per-monitor islands, settings file.

Finish and verify each step before starting the next.

## Project layout (proposed)

```
src/
  main.rs          # app setup, signal handling
  window.rs        # X11 window setup, positioning, input shape, monitor changes
  island.rs        # pill widget, expand/collapse state machine, animation
  style.css        # all styling (embedded with include_str!)
  cards/           # one module per card: clock, music, lyrics, peeks, timer, todo
  services/        # D-Bus and HTTP clients: mpris, upower, network, bluez, lyrics
  store.rs         # local JSON persistence
```

## Commands

```bash
cargo run                  # run in development
RUST_LOG=atoll=debug cargo run
cargo clippy -- -D warnings
cargo fmt
cargo build --release      # binary at target/release/atoll
```

## Conventions

- Keep `main.rs` small; one responsibility per module
- No `unwrap()` in runtime paths; log errors and degrade gracefully (for example, no music player means no music card)
- Each D-Bus service is optional: if it is unavailable, the related feature hides itself
- Comments explain *why*, not *what*
- After each change, run `cargo clippy` and `cargo fmt`

## How the user tests

The user runs `cargo run` on the real machine and reports back with screenshots and terminal output. Claude cannot see the screen, so for anything visual, describe what the user should check and ask for a screenshot.
