#!/bin/bash
# Build the Linux release tarball: dist/kiore-linux-<arch>.tar.gz
# It holds the binary, install/uninstall scripts and the Omarchy bar plugin; no Rust needed
# to install it.
set -euo pipefail
cd "$(dirname "$0")/.."
arch=$(uname -m)
name=kiore-linux-$arch
out=dist/$name

cargo build --release --locked -p kiore
rm -rf "$out" && mkdir -p "$out/omarchy-plugin"
cp target/release/kiore "$out/"
strip "$out/kiore" 2>/dev/null || true
cp scripts/install-linux.sh "$out/install.sh"
cp scripts/uninstall-linux.sh "$out/uninstall.sh"
cp scripts/enable-wake-linux.sh "$out/enable-wake.sh"
cp scripts/enable-input-linux.sh "$out/enable-input.sh"
cp -r integrations/omarchy/nz.galengreen.kiore "$out/omarchy-plugin/"
cp LICENSE "$out/"
cat > "$out/README.txt" <<'TXT'
Kiore for Linux (Wayland: Hyprland / Omarchy)

  ./install.sh        install for your user (no sudo) and start it with your desktop
  ./uninstall.sh      remove it again
  ./enable-wake.sh    optional: let other computers wake this one from sleep (asks for sudo)
  ./enable-input.sh   only if Kiore says so (GNOME, KDE): allow it to control this computer

Then open Kiore on your Mac and click Pair. https://github.com/galengreen/kiore
TXT
tar -C dist -czf "dist/$name.tar.gz" "$name"
echo "built dist/$name.tar.gz"
