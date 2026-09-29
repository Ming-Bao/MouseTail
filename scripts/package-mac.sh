#!/bin/bash
# Build the Mac release: dist/Kiore-macos.dmg (universal: Apple Silicon and Intel).
set -euo pipefail
cd "$(dirname "$0")/.."
scripts/build-mac-app.sh universal
stage=dist/dmg
rm -rf "$stage" dist/Kiore-macos.dmg
mkdir -p "$stage"
cp -R dist/Kiore.app "$stage/"
ln -s /Applications "$stage/Applications"
hdiutil create -volname Kiore -srcfolder "$stage" -ov -format UDZO dist/Kiore-macos.dmg >/dev/null
rm -rf "$stage"
echo "built dist/Kiore-macos.dmg"
