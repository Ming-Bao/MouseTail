## What's new

- **MouseTail now keeps itself up to date**, on Mac and Linux. New releases are signed, checked,
  and installed while you're not using another computer; turn it off in the Mac menu or with
  `mousetail set updates off`. Install this release by hand once on each computer and the
  rest arrive by themselves.
- A new menu bar icon, also used in the Omarchy bar.

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
