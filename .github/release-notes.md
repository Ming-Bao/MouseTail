## What's new

- **Kiore is now MouseTail**, with a new icon. Existing installs keep their pairings and
  arrangement: install MouseTail on both computers and they reconnect. On Linux the installer
  removes the old Kiore service; on a Mac, delete Kiore from Applications and allow MouseTail's
  permissions when asked.
- **Any computer can control any other**: Mac → Linux, Linux → Mac, Mac → Mac and Linux → Linux.
  Linux can now be the main computer.
- Works beyond Omarchy: Sway and other wlroots desktops, and KDE and GNOME as the computer
  being controlled (some still experimental).

## Install

**Mac** (macOS 14 or later, Apple Silicon or Intel): download `MouseTail-macos.dmg`, drag MouseTail to
Applications and open it. This build isn't notarised yet, so the first time macOS will refuse
to open it: go to **System Settings → Privacy & Security** and click **Open Anyway**.

**Linux** (Wayland):

```sh
curl -fsSL https://mousetail.galen.green/install.sh | sh
```

or download `mousetail-linux-<arch>.tar.gz`, extract it and run `./install.sh`.

Then pair: click **Pair…** next to the other computer in MouseTail's menu on a Mac, or run
`mousetail pair` on Linux, and type the code the other computer shows.
