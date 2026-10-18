#!/usr/bin/env bash
set -euo pipefail
LABEL="dev.capcom.client"
BIN="/Applications/Capcom.app/Contents/MacOS/capcom"
launchctl bootout "gui/$(id -u)/$LABEL" 2>/dev/null || true
rm -f "$HOME/Library/LaunchAgents/$LABEL.plist"
if pkill -TERM -f "^$BIN" 2>/dev/null; then sleep 1; fi
rm -rf /Applications/Capcom.app
echo "Uninstalled. Config (~/.config/capcom) and history were left in place."
echo "If you had enabled 'Launch at login', remove Capcom from System Settings > General > Login Items."
echo "Remove stale permission entries in System Settings > Privacy & Security if desired."
