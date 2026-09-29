#!/bin/bash
# Build Kiore.app: the SwiftUI menu bar app with the Rust daemon inside it.
#   scripts/build-mac-app.sh [debug|release|universal]   → dist/Kiore.app
# `universal` runs natively on Apple Silicon and Intel (what releases ship).
# Signs with the first Apple Development / Developer ID identity found (ad-hoc otherwise).
set -euo pipefail
cd "$(dirname "$0")/.."
profile=${1:-release}
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)

if [[ $profile == universal ]]; then
  # Separate target dirs per architecture: sharing one lets their host-side macro builds
  # overwrite each other.
  for t in aarch64-apple-darwin x86_64-apple-darwin; do
    rustup target add "$t" >/dev/null 2>&1 || true
    CARGO_TARGET_DIR="target/mac-$t" \
      cargo build --release --locked -p kiore --target "$t"
  done
  mkdir -p target/universal
  lipo -create -output target/universal/kiore \
    target/mac-aarch64-apple-darwin/aarch64-apple-darwin/release/kiore \
    target/mac-x86_64-apple-darwin/x86_64-apple-darwin/release/kiore
  (cd apps/macos && swift build -c release --arch arm64 --arch x86_64)
  daemon=target/universal/kiore
  ui=$(cd apps/macos && swift build -c release --arch arm64 --arch x86_64 --show-bin-path)/Kiore
elif [[ $profile == release ]]; then
  cargo build --release -p kiore
  (cd apps/macos && swift build -c release)
  daemon=target/release/kiore
  ui=apps/macos/.build/release/Kiore
else
  cargo build -p kiore
  (cd apps/macos && swift build)
  daemon=target/debug/kiore
  ui=apps/macos/.build/debug/Kiore
fi

app=dist/Kiore.app
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$ui" "$app/Contents/MacOS/Kiore"
# Lower-case "kiore" would clash with "Kiore" on a case-insensitive disk.
cp "$daemon" "$app/Contents/MacOS/kiored"
cp apps/macos/Resources/AppIcon.icns apps/macos/Resources/MenuBarIcon*.png "$app/Contents/Resources/"

cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Kiore</string>
  <key>CFBundleDisplayName</key><string>Kiore</string>
  <key>CFBundleIdentifier</key><string>nz.galengreen.Kiore</string>
  <key>CFBundleExecutable</key><string>Kiore</string>
  <key>CFBundleIconFile</key><string>AppIcon</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$version</string>
  <key>CFBundleVersion</key><string>$version</string>
  <key>LSMinimumSystemVersion</key><string>14.0</string>
  <key>LSUIElement</key><true/>
  <key>NSHumanReadableCopyright</key><string>© Galen Green</string>
  <key>NSLocalNetworkUsageDescription</key>
  <string>Kiore finds and connects to your other computers on the local network.</string>
  <key>NSBonjourServices</key>
  <array><string>_kiore._udp</string></array>
</dict>
</plist>
PLIST

ident=$(security find-identity -v -p codesigning | awk '/Developer ID Application|Apple Development/ {print $2; exit}')
ident=${ident:--}
codesign --force --sign "$ident" --identifier nz.galengreen.Kiore.daemon "$app/Contents/MacOS/kiored"
codesign --force --sign "$ident" "$app"
echo "built $app ($profile, signed with ${ident:0:8}…)"
