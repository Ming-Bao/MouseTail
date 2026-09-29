#!/bin/bash
# Build the Mac release: dist/MouseTail-macos.dmg (universal: Apple Silicon and Intel).
set -euo pipefail
cd "$(dirname "$0")/.."
scripts/build-mac-app.sh universal
stage=dist/dmg
rm -rf "$stage" dist/MouseTail-macos.dmg
mkdir -p "$stage"
cp -R dist/MouseTail.app "$stage/"
ln -s /Applications "$stage/Applications"
hdiutil create -volname MouseTail -srcfolder "$stage" -ov -format UDZO dist/MouseTail-macos.dmg >/dev/null
rm -rf "$stage"
echo "built dist/MouseTail-macos.dmg"
