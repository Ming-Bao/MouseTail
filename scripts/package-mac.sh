#!/bin/bash
# Build the Mac release (universal: Apple Silicon and Intel): dist/MouseTail-macos.dmg to
# download, and dist/MouseTail-macos.zip for the app to update itself from.
set -euo pipefail
cd "$(dirname "$0")/.."
scripts/build-mac-app.sh universal
stage=dist/dmg
rm -rf "$stage" dist/MouseTail-macos.dmg dist/MouseTail-macos.zip
mkdir -p "$stage"
cp -R dist/MouseTail.app "$stage/"
ln -s /Applications "$stage/Applications"
hdiutil create -volname MouseTail -srcfolder "$stage" -ov -format UDZO dist/MouseTail-macos.dmg >/dev/null
rm -rf "$stage"
ditto -c -k --keepParent dist/MouseTail.app dist/MouseTail-macos.zip
echo "built dist/MouseTail-macos.dmg and .zip"
