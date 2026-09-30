## What's new

- Caps Lock no longer gets stuck on the other computer after you switch it off on your Mac.
- The app icon no longer shows faint white lines at small sizes, and the menu is tidier.
- MouseTail keeps itself up to date. If you're on 0.2.0 or earlier, install this release by
  hand once on each computer; 0.2.1 and later update by themselves.

## Install

**Mac** (macOS 14.2 or later, Apple Silicon or Intel): download `MouseTail-macos.dmg`, drag MouseTail to
Applications and open it. This build isn't notarised yet, so the first time macOS will refuse
to open it: go to **System Settings → Privacy & Security** and click **Open Anyway**.

**Linux** (Wayland):

```sh
curl -fsSL https://mousetail.galen.green/install.sh | sh
```

or download `mousetail-linux-<arch>.tar.gz`, extract it and run `./install.sh`.

Then pair: click **Pair…** next to the other computer in MouseTail's menu on a Mac, or run
`mousetail pair` on Linux, and type the code the other computer shows.
