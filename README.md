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

To start Atoll at login, open Settings and turn on **General → Start at
login**. This writes `~/.config/autostart/atoll.desktop`, and turning the
switch off removes it.

To uninstall, run `./install.sh --uninstall`. Your settings and data stay in
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
