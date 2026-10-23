#!/usr/bin/env bash
# Installs Atoll for the current user only (no sudo, no system files).
#
#   ./install.sh             build and install
#   ./install.sh --uninstall remove what install put in place
#
# Installs:
#   ~/.local/bin/atoll
#   ~/.local/share/applications/atoll.desktop    (menu entry, opens Settings)
#   ~/.local/share/icons/hicolor/scalable/apps/atoll.svg
# Updates, when present (a .bak copy is kept first):
#   ~/.claude/settings.json hooks and ~/.config/atoll/shell.bash, which
#   pointed at the development build, now point at ~/.local/bin/atoll.
# Start at login is a switch in Atoll's Settings.
set -euo pipefail

cd "$(dirname "$0")"
BIN="$HOME/.local/bin/atoll"
APPS="$HOME/.local/share/applications"
ICONS="$HOME/.local/share/icons/hicolor/scalable/apps"
DEV_BIN="$PWD/target/debug/atoll"
CLAUDE_SETTINGS="$HOME/.claude/settings.json"
SHELL_SNIPPET="${XDG_CONFIG_HOME:-$HOME/.config}/atoll/shell.bash"

if [[ "${1:-}" == "--uninstall" ]]; then
    pkill -x atoll 2>/dev/null || true
    rm -f "$BIN" "$APPS/atoll.desktop" "$ICONS/atoll.svg" \
        "${XDG_CONFIG_HOME:-$HOME/.config}/autostart/atoll.desktop"
    echo "Atoll removed. Settings and data are kept in ~/.config/atoll and ~/.local/share/atoll."
    echo "Remove the Atoll hooks from $CLAUDE_SETTINGS and the shell line from ~/.bashrc by hand if you added them."
    exit 0
fi

cargo build --release

install -Dm755 target/release/atoll "$BIN"
install -Dm644 resources/atoll.svg "$ICONS/atoll.svg"
mkdir -p "$APPS"
sed "s|@BIN@|$BIN|" resources/atoll.desktop > "$APPS/atoll.desktop"
update-desktop-database -q "$APPS" 2>/dev/null || true
gtk-update-icon-cache -q "$HOME/.local/share/icons/hicolor" 2>/dev/null || true

# Repoint hooks and shell snippet from the dev build to the installed one.
if [[ -f "$CLAUDE_SETTINGS" ]] && grep -qF "$DEV_BIN" "$CLAUDE_SETTINGS"; then
    cp "$CLAUDE_SETTINGS" "$CLAUDE_SETTINGS.bak"
    sed -i "s|$DEV_BIN|$BIN|g" "$CLAUDE_SETTINGS"
    echo "Claude Code hooks now use $BIN (backup: $CLAUDE_SETTINGS.bak)"
fi
if [[ -f "$SHELL_SNIPPET" ]]; then
    "$BIN" shell-init > "$SHELL_SNIPPET"
    echo "Shell snippet regenerated: $SHELL_SNIPPET"
fi

echo
echo "Installed $BIN"
echo "Start it now with:  atoll &   (or from the app menu: Atoll)"
echo "Turn on Settings → General → Start at login to launch it with GNOME."
if [[ ":$PATH:" != *":$HOME/.local/bin:"* ]]; then
    echo "Note: ~/.local/bin is not on your PATH; log out and back in, or use $BIN."
fi
