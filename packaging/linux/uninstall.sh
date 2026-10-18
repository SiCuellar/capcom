#!/usr/bin/env bash
# Remove Capcom's Linux integration. Config (~/.config/capcom) and history are kept.
set -uo pipefail
bin_dir="${HOME}/.local/bin"
unit_dir="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"

if command -v omarchy-remove-dictation-capcom &>/dev/null; then
  omarchy-remove-dictation-capcom
else
  systemctl --user disable --now capcom.service 2>/dev/null
fi
rm -f "$unit_dir/capcom.service"
systemctl --user daemon-reload
rm -f "$bin_dir/capcom" "$bin_dir/omarchy-dictation-capcom" \
  "$bin_dir/omarchy-install-dictation-capcom" "$bin_dir/omarchy-remove-dictation-capcom"
rm -rf "${XDG_RUNTIME_DIR:-/nonexistent}/capcom"
echo "Removed. Config and history left in ~/.config/capcom and ~/.local/share/capcom."
