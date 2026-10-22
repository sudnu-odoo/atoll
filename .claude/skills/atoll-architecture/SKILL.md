---
name: atoll-architecture
description: Architecture and coding rules for the Atoll Rust/GTK4 island. Use before adding or changing any card, service, window/X11 code, animation, or persistence in this repo, and when reviewing Atoll code.
---

# Atoll architecture

Read CLAUDE.md first: safety rules, target machine, palette and roadmap live there. This skill covers how the code is organized so it stays easy to maintain.

## Module map and ownership

```
main.rs        app setup, logging, signal handling. Nothing else.
window.rs      X11 only: override-redirect, placement, stacking, input shape, RandR.
island.rs      the notch: shape drawing, size/state machine, animation driver.
cards/<x>.rs   one card per file: builds its widgets, renders a view model.
services/<x>.rs one external source per file: MPRIS, UPower, NM, BlueZ, LRCLIB.
store.rs       JSON persistence in ~/.config/atoll and ~/.local/share/atoll.
style.css      all CSS, embedded with include_str!.
```

Dependency direction is one-way:

```
main -> window, island
island -> cards
cards -> (view-model types only)
services -> nothing in the UI
```

- A **service never touches GTK.** It produces plain data (a `struct` or `enum`) and sends it over a channel.
- A **card never talks to D-Bus or HTTP.** It receives a view model and renders it, and it sends user intents (e.g. `MusicCmd::Next`) back as messages.
- `island.rs` decides *which* card is shown and *how big* the notch is; cards don't resize the window themselves.
- `window.rs` is the only file that imports `x11rb`.

## Services pattern

Every service follows the same shape so they're interchangeable:

```rust
pub enum Update { /* what changed, as plain data */ }
pub enum Command { /* what the UI may ask for */ }

/// Starts the service. Returns None if the backend is unavailable,
/// in which case the related card hides itself.
pub fn spawn(updates: async_channel::Sender<Update>)
    -> Option<async_channel::Sender<Command>>;
```

- Use **D-Bus signals** (`PropertiesChanged`, `NameOwnerChanged`), not polling. Idle CPU must stay ~0%.
- Absence is normal: no player, no battery, no BlueZ. Log at `debug`, return `None` or send an "unavailable" update. Never panic or log at `error` for this.
- Reconnect when a bus name reappears (e.g. Spotify restarted), driven by `NameOwnerChanged`.
- Network calls (LRCLIB) send only title, artist and duration, with a timeout, and the results are cached on disk via `store.rs`.

## Threading and async

- GTK objects live on the main thread only. Never move a widget, `Rc`, or `Cell` across threads.
- UI-side async uses `glib::MainContext::default().spawn_local(...)` and receives on an `async_channel::Receiver`.
- zbus/reqwest work runs on its own executor (a dedicated thread, or zbus's internal executor) and talks to the UI only through channels.
- Blocking calls (`std::fs`, a blocking X round-trip) are not allowed on the UI thread in hot paths. Startup-only blocking is acceptable if it's short.
- Low-level X event loops (like `window::keep_placed`) run on their own thread and block in the kernel. They never poll.

## State and animation

- Keep the island's state as an explicit enum, e.g. `Collapsed | Peek(PeekKind) | Expanded(CardKind)`, with transitions in one `fn transition(&mut self, event)`. No booleans scattered across closures.
- Animate with `gtk::TickCallback` / `adw`-style timed easing *only while animating*. Remove the tick callback when the animation ends, so idle means zero frames.
- Motion: ~250 ms, critically damped (no overshoot). Put the easing function in one place.
- The input shape must follow the current notch bounds whenever the size changes.

## Drawing and styling

- The notch outline (concave top "ears" plus rounded bottom) is drawn with cairo in `island.rs`, because CSS can't do concave corners. Everything *inside* it is normal GTK widgets styled in `style.css`.
- Colors come from the CLAUDE.md palette. Define them once (CSS `@define-color`, plus one Rust const set for cairo) and never hard-code a hex value elsewhere.
- Fonts: Ubuntu for text, JetBrainsMono Nerd Font for numbers and timers, Symbols Nerd Font for icons.
- Don't copy Nusa's artwork, icons or eyes character. Concept only.

## Error handling

- No `unwrap()`/`expect()` in runtime paths. Use `let ... else`, `?` inside helpers, and `tracing::warn!`/`debug!` at the boundary.
- Prefer degrading a feature to failing it: a bad album-art URL means a placeholder, not an error card.
- `tracing` levels: `error` = Atoll itself is broken, `warn` = something unexpected that we recovered from, `debug` = normal absence and state changes.

## Code style

- Comments explain *why* (X11 quirks, GNOME behavior, ordering constraints), not *what*.
- Keep functions short. A card file is typically `new()`, `update(&ViewModel)`, plus small private helpers.
- Plain structs and enums are better than trait hierarchies. Add a trait only when there are two or more real implementations.
- Constants (sizes, radii, durations) go at the top of the module that owns them.

## Adding a feature: checklist

1. Service in `services/` producing plain `Update`s, returning `None` when unavailable.
2. View model and card in `cards/`, with no I/O.
3. Wire the channels in `island.rs` (or a small `app.rs` if wiring grows), and add the state variant.
4. CSS in `style.css` using palette tokens.
5. `cargo fmt && cargo clippy -- -D warnings` must be clean.
6. Passive checks only (xwininfo, xprop, `import` screenshots). **Never** send synthetic clicks or keys to the user's desktop; ask the user to test interactions and send screenshots.
7. Check idle CPU with `top -p $(pgrep atoll)` after the change. It should sit at 0.0%.
