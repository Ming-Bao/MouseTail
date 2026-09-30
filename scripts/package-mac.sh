#!/bin/bash
# Build the Mac release (universal: Apple Silicon and Intel): dist/MouseTail-macos.dmg to
# download, and dist/MouseTail-macos.zip for the app to update itself from.
set -euo pipefail
cd "$(dirname "$0")/.."
scripts/build-mac-app.sh universal
stage=dist/dmg
rw=dist/MouseTail-rw.dmg
rm -rf "$stage" "$rw" dist/MouseTail-macos.dmg dist/MouseTail-macos.zip
mkdir -p "$stage/.background"
cp -R dist/MouseTail.app "$stage/"
ln -s /Applications "$stage/Applications"

# The window's background, at 1x and 2x in one file so it's sharp on Retina screens.
swift scripts/make-dmg-background.swift "$stage/.background/1x.png" 1
swift scripts/make-dmg-background.swift "$stage/.background/2x.png" 2
tiffutil -cathidpicheck "$stage/.background/1x.png" "$stage/.background/2x.png" \
  -out "$stage/.background/background.tiff" 2>/dev/null
rm "$stage"/.background/{1x,2x}.png

# A writable image to lay the window out in, then compressed for download.
hdiutil create -volname MouseTail -srcfolder "$stage" -ov -format UDRW -fs HFS+ "$rw" >/dev/null
rm -rf "$stage"
# Detach any copy left mounted by an earlier run, so Finder finds this one by name.
[[ -d /Volumes/MouseTail ]] && hdiutil detach /Volumes/MouseTail -force >/dev/null
device=$(hdiutil attach -readwrite -noverify -noautoopen "$rw" | awk '/Apple_HFS/ {print $1}')
volume=/Volumes/MouseTail
trap 'hdiutil detach "$device" -force >/dev/null 2>&1 || true' EXIT
# Give Finder a moment to notice the disk.
for _ in {1..10}; do
  osascript -e 'tell application "Finder" to exists disk "MouseTail"' | grep -q true && break
  sleep 1
done

# Matches the layout in scripts/make-dmg-background.swift: 640 × 400 points inside the window
# (plus its title bar). The hidden items are moved out of sight too, for anyone who has Finder
# showing hidden files.
osascript <<'APPLESCRIPT'
tell application "Finder"
  tell disk "MouseTail"
    open
    set current view of container window to icon view
    set toolbar visible of container window to false
    set statusbar visible of container window to false
    set pathbar visible of container window to false
    set bounds of container window to {200, 120, 840, 552}
    set options to the icon view options of container window
    set arrangement of options to not arranged
    set icon size of options to 128
    set text size of options to 13
    set background picture of options to file ".background:background.tiff"
    set position of item "MouseTail.app" of container window to {160, 170}
    set position of item "Applications" of container window to {480, 170}
    repeat with dotItem in {".background", ".fseventsd"}
      if exists item dotItem then set position of item dotItem of container window to {900, 900}
    end repeat
    close
    open
    update without registering applications
    delay 2
    close
  end tell
end tell
APPLESCRIPT

# The disk's own icon, while it's mounted. (Added after Finder's visit, which removes it.)
cp dist/MouseTail.app/Contents/Resources/AppIcon.icns "$volume/.VolumeIcon.icns"
SetFile -a C "$volume"
chmod -Rf go-w "$volume" || true
rm -rf "$volume/.fseventsd"
sync
hdiutil detach "$device" >/dev/null
trap - EXIT
hdiutil convert "$rw" -format UDZO -imagekey zlib-level=9 -o dist/MouseTail-macos.dmg >/dev/null
rm "$rw"
ditto -c -k --keepParent dist/MouseTail.app dist/MouseTail-macos.zip
echo "built dist/MouseTail-macos.dmg and .zip"
