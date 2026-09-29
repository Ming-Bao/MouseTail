#!/bin/bash
# Build the Linux release tarball: dist/mousetail-linux-<arch>.tar.gz
# It holds the binary, install/uninstall scripts and the Omarchy bar plugin; no Rust needed
# to install it.
set -euo pipefail
cd "$(dirname "$0")/.."
arch=$(uname -m)
name=mousetail-linux-$arch
out=dist/$name

cargo build --release --locked -p mousetail
rm -rf "$out" && mkdir -p "$out/omarchy-plugin"
cp target/release/mousetail "$out/"
strip "$out/mousetail" 2>/dev/null || true
cp scripts/install-linux.sh "$out/install.sh"
cp scripts/uninstall-linux.sh "$out/uninstall.sh"
cp scripts/enable-wake-linux.sh "$out/enable-wake.sh"
cp scripts/enable-input-linux.sh "$out/enable-input.sh"
cp -r integrations/omarchy/nz.galengreen.mousetail "$out/omarchy-plugin/"
cp LICENSE "$out/"
cat > "$out/README.txt" <<'TXT'
MouseTail for Linux (Wayland: Hyprland / Omarchy)

  ./install.sh        install for your user (no sudo) and start it with your desktop
  ./uninstall.sh      remove it again
  ./enable-wake.sh    optional: let other computers wake this one from sleep (asks for sudo)
  ./enable-input.sh   only if MouseTail says so (GNOME, KDE): allow it to control this computer

Then open MouseTail on your Mac and click Pair. https://github.com/galengreen/mousetail
TXT
tar -C dist -czf "dist/$name.tar.gz" "$name"
echo "built dist/$name.tar.gz"
