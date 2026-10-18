#!/usr/bin/env bash
# Capcom installer for Linux (Omarchy / Arch + Hyprland). Run from anywhere inside the repo:
#   packaging/linux/install.sh [--no-build] [--no-omarchy]
# From a release tarball: ./packaging/linux/install.sh --no-build
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
bin_dir="${HOME}/.local/bin"
unit_dir="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
build=1
omarchy=auto
for a in "$@"; do
  case "$a" in
    --no-build) build=0 ;;
    --no-omarchy) omarchy=0 ;;
    *) echo "unknown option: $a" >&2; exit 2 ;;
  esac
done

say() { printf '\033[1m==> %s\033[0m\n' "$*"; }

# ---- dependencies -------------------------------------------------------------------------
declare -A pkg_for=(
  [wl-copy]=wl-clipboard [wl-paste]=wl-clipboard [wtype]=wtype
  [pw-play]=pipewire [notify-send]=libnotify [jq]=jq [cargo]=rust
)
missing_pkgs=()
for cmd in wl-copy wl-paste wtype pw-play notify-send jq cargo; do
  if ! command -v "$cmd" &>/dev/null; then
    [[ $cmd == cargo && $build == 0 ]] && continue
    missing_pkgs+=("${pkg_for[$cmd]}")
  fi
done
if ((${#missing_pkgs[@]})); then
  mapfile -t missing_pkgs < <(printf '%s\n' "${missing_pkgs[@]}" | sort -u)
  echo "Missing dependencies: ${missing_pkgs[*]}"
  if command -v omarchy-pkg-add &>/dev/null; then
    omarchy-pkg-add "${missing_pkgs[@]}"
  elif command -v pacman &>/dev/null; then
    echo "Installing with: sudo pacman -S --needed ${missing_pkgs[*]}"
    sudo pacman -S --needed "${missing_pkgs[@]}"
  else
    echo "Install them with your package manager, then re-run." >&2
    exit 1
  fi
fi
# Build deps for cpal (ALSA headers; PipeWire provides pipewire-alsa at runtime).
if ((build)) && command -v pacman &>/dev/null && ! pacman -Q alsa-lib &>/dev/null; then
  sudo pacman -S --needed alsa-lib pkgconf base-devel
fi

# ---- build + install binary ---------------------------------------------------------------
if ((build)); then
  say "cargo build --release"
  (cd "$repo" && cargo build --release)
  src="$repo/target/release/capcom"
else
  # Release tarballs ship the binary at the top level; a checkout has it under target/.
  src="$repo/capcom"
  [[ -x $src && ! -d $src ]] || src="$repo/target/release/capcom"
  [[ -x $src ]] || { echo "no release binary at $src (drop --no-build)" >&2; exit 1; }
fi
mkdir -p "$bin_dir"
install -m 0755 "$src" "$bin_dir/capcom"
case ":$PATH:" in *":$bin_dir:"*) ;; *) echo "warning: $bin_dir is not in PATH" >&2 ;; esac

# ---- config -------------------------------------------------------------------------------
say "config"
if "$bin_dir/capcom" config show 2>/dev/null | grep -q "not set up"; then
  if [[ -t 0 ]]; then
    "$bin_dir/capcom" setup || true
  else
    echo "Next: run  capcom setup  to point Capcom at your speech-to-text + LLM servers."
  fi
fi

# ---- systemd user unit --------------------------------------------------------------------
say "systemd user service"
mkdir -p "$unit_dir"
install -m 0644 "$here/capcom.service" "$unit_dir/capcom.service"
systemctl --user daemon-reload
systemctl --user enable capcom.service
if systemctl --user is-active --quiet graphical-session.target; then
  systemctl --user restart capcom.service
else
  echo "graphical-session.target not active; the service will start with your next login."
fi

# ---- Omarchy dictation backend ------------------------------------------------------------
if [[ $omarchy == auto ]]; then
  command -v omarchy-dictation &>/dev/null && omarchy=1 || omarchy=0
fi
if [[ $omarchy == 1 ]]; then
  say "Omarchy dictation backend"
  for f in omarchy-dictation-capcom omarchy-install-dictation-capcom omarchy-remove-dictation-capcom; do
    install -m 0755 "$here/omarchy/$f" "$bin_dir/$f"
  done
  echo "Installed backend scripts into $bin_dir."
  echo "Select it with:  omarchy-default-dictation capcom   (or Setup > Defaults > Dictation)"
  echo "This runs omarchy-install-dictation-capcom, which selects the backend and reloads Hyprland."
else
  echo "Omarchy not detected. For plain Hyprland, see $here/hyprland-binds.lua (or .conf)."
fi

say "Waybar: see $here/waybar-capcom.jsonc"
say "Verify:  capcom doctor"
