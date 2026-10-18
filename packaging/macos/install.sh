#!/usr/bin/env bash
# Install Capcom.app to /Applications and start it (menu-bar icon). Run make-app.sh first.
#
#   ./install.sh                 copy + launch; enable "Launch at login" from the menu-bar icon
#   ./install.sh --launch-agent  also install the LaunchAgent (autostart that restarts Capcom
#                                after a crash; not needed with "Launch at login")
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
APP="$HERE/build/Capcom.app"
LABEL="dev.capcom.client"
AGENT="$HOME/Library/LaunchAgents/$LABEL.plist"
BIN="/Applications/Capcom.app/Contents/MacOS/capcom"

[ -d "$APP" ] || { echo "build first: $HERE/make-app.sh" >&2; exit 1; }

# Stop a running installed copy cleanly (SIGTERM -> the daemon removes its socket).
launchctl bootout "gui/$(id -u)/$LABEL" 2>/dev/null || true
if pkill -TERM -f "^$BIN" 2>/dev/null; then sleep 1; fi

rm -rf /Applications/Capcom.app
cp -R "$APP" /Applications/Capcom.app

if [ "${1:-}" = "--launch-agent" ]; then
  mkdir -p "$HOME/Library/LaunchAgents" "$HOME/Library/Logs"
  sed "s|__HOME__|$HOME|g" "$HERE/$LABEL.plist" > "$AGENT"
  launchctl bootstrap "gui/$(id -u)" "$AGENT"
  launchctl kickstart -k "gui/$(id -u)/$LABEL"
else
  open /Applications/Capcom.app
fi

cat <<EOF
Installed /Applications/Capcom.app: look for the mic icon in the menu bar. Log: ~/Library/Logs/Capcom.log
If another 'capcom daemon' already runs (e.g. from a terminal), the app exits quietly; stop that one first.
Grant /Applications/Capcom.app in System Settings > Privacy & Security:
  Microphone, Accessibility, Input Monitoring  (then Quit Capcom from its menu and open the app again)
Autostart: menu-bar icon > "Launch at login".
(Only if you set push_to_talk = "fn": System Settings > Keyboard > 'Press Globe key to' = Do Nothing.)
Verify: $BIN doctor
Optional CLI: ln -sf $BIN /usr/local/bin/capcom
EOF
