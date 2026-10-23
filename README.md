# Atoll

A Dynamic Island for the GNOME top bar (Xorg), written in Rust and GTK 4.
It runs as its own process, not as a Shell extension: if it stops, only the
notch disappears.

## Install

Needs Rust and the GTK 4 development files (`libgtk-4-dev`).

```bash
./install.sh
```

Everything goes into your home folder, without sudo:

- `~/.local/bin/atoll`
- an "Atoll" entry in the app menu, which opens Atoll's Settings
- the app icon

If the Claude Code hooks in `~/.claude/settings.json` or the shell snippet in
`~/.config/atoll/shell.bash` point at the development build, the script
switches them to the installed binary. It keeps a `.bak` copy of the Claude
settings first.

### Optional: Atoll Bridge (GNOME extension)

Only GNOME Shell can open a notification exactly the way its banner does,
for example straight into the right Discord chat. `install.sh` copies a
small extension for this, but doesn't enable it. Without it, clicking a
notification in Atoll brings the app forward instead.

The extension adds one D-Bus method and nothing else: no UI, no timers. Every
path is guarded so it never throws into the Shell. To turn it on, log out and
back in, then run:

```bash
gnome-extensions enable atoll-bridge@atoll.dev
```

To turn it off, run `gnome-extensions disable atoll-bridge@atoll.dev`.

### Start at login

To start Atoll at login, open Settings and turn on **General → Start at
login**. This writes `~/.config/autostart/atoll.desktop`, and turning the
switch off removes it.

To switch Atoll off without quitting it, use **General → Active**. The
notch goes away and GNOME's banners come back until you switch it on again
(from the app menu entry, or `atoll --settings`).

To uninstall, run `./install.sh --uninstall`. This also removes the
extension. Your settings and data stay in
`~/.config/atoll` and `~/.local/share/atoll`.

## Use

- `atoll`: start the notch
- `atoll --settings`: open Settings. This also starts Atoll if it isn't
  running.
- `atoll shell-init`: print the bash snippet for the "command finished" peek

## Develop

```bash
cargo run
RUST_LOG=atoll=debug cargo run
cargo test
cargo clippy --all-targets -- -D warnings
```
