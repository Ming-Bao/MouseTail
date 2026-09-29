#!/bin/bash
# Install Kiore for the current user on Linux (Wayland: Hyprland / Omarchy). No sudo.
#
#   ./install.sh                  from a release download (uses the included binary)
#   scripts/install-linux.sh      from a source checkout (builds it; needs Rust)
#
# Installs ~/.local/bin/kiore, runs it as a systemd user service that starts with your
# desktop, and on Omarchy adds a status icon to the bar.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
bin_dir=$HOME/.local/bin
config_home=${XDG_CONFIG_HOME:-$HOME/.config}
unit_dir=$config_home/systemd/user
plugin_id=nz.galengreen.kiore
omarchy=$config_home/omarchy

say() { printf '\033[1m==> %s\033[0m\n' "$*"; }

if [[ -x $here/kiore ]]; then
  # Release download: everything is alongside this script.
  binary=$here/kiore
  plugin_src=$here/omarchy-plugin/$plugin_id
else
  repo=$(cd "$here/.." && pwd)
  [[ -f $repo/Cargo.toml ]] || { echo "Can't find Kiore to install."; exit 1; }
  cargo=$(command -v cargo || true)
  [[ -z $cargo && -x $HOME/.cargo/bin/cargo ]] && cargo=$HOME/.cargo/bin/cargo
  if [[ -z $cargo ]]; then
    echo "Building Kiore needs Rust (https://rustup.rs), or download a release instead."
    exit 1
  fi
  say "Building Kiore"
  (cd "$repo" && "$cargo" build --release --quiet -p kiore)
  binary=$repo/target/release/kiore
  plugin_src=$repo/integrations/omarchy/$plugin_id
fi

install -Dm755 "$binary" "$bin_dir/kiore"
missing=$(ldd "$bin_dir/kiore" 2>/dev/null | awk '/not found/ {print $1}' | paste -sd' ' || true)
if [[ -n $missing ]]; then
  echo "Kiore needs these libraries, which this system is missing: $missing"
  echo "(They come with PipeWire and Opus; install those with your package manager.)"
  exit 1
fi

say "Starting it with your desktop"
mkdir -p "$unit_dir"
cat > "$unit_dir/kiore.service" <<UNIT
[Unit]
Description=Kiore keyboard, mouse and clipboard sharing
After=graphical-session.target
PartOf=graphical-session.target

[Service]
ExecStart=%h/.local/bin/kiore run
Restart=on-failure
RestartSec=2

[Install]
WantedBy=graphical-session.target
UNIT
systemctl --user daemon-reload
systemctl --user enable --quiet kiore.service
systemctl --user restart kiore.service

if [[ -d $omarchy ]]; then
  say "Adding Kiore to the Omarchy bar"
  rm -rf "$omarchy/plugins/$plugin_id"
  mkdir -p "$omarchy/plugins"
  cp -r "$plugin_src" "$omarchy/plugins/$plugin_id"
  shell=$omarchy/shell.json
  if [[ -f $shell ]] && command -v jq >/dev/null; then
    if jq -e --arg id "$plugin_id" '[.bar.layout[]?[]?.id] | index($id)' "$shell" >/dev/null; then
      :
    elif jq -e '.bar.layout.right | type == "array"' "$shell" >/dev/null; then
      cp "$shell" "$shell.before-kiore"
      jq --arg id "$plugin_id" '.bar.layout.right = [{"id": $id}] + .bar.layout.right' \
        "$shell" > "$shell.tmp" && mv "$shell.tmp" "$shell"
    else
      echo "    Your bar uses Omarchy's default layout; add \"Kiore\" from the bar settings."
    fi
  fi
fi

# On desktops without Wayland's virtual-input protocols, being controlled needs uinput access.
sleep 3
if ! "$bin_dir/kiore" status 2>/dev/null | grep -q "can be controlled"; then
  echo
  echo "To let other computers control this one on this desktop, run once (asks for your password):"
  if [[ -x $here/enable-input.sh ]]; then echo "    $here/enable-input.sh"; else echo "    $here/enable-input-linux.sh"; fi
fi

if systemctl --user is-active --quiet lan-mouse.service 2>/dev/null; then
  echo
  echo "Note: Lan Mouse is also running. Use one or the other:"
  echo "    systemctl --user disable --now lan-mouse.service"
fi

echo
say "Done. On your Mac, open Kiore and click Pair next to $(hostname)."
