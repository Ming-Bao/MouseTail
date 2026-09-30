## What's new

- Nothing changes in how MouseTail works in this release. It comes with a new website, whose
  demo shows the crossing ripple the way the app draws it.

If you're coming from 0.2.2 or earlier, 0.2.3 brought:

- A ripple spreads from the edge wherever the cursor crosses to or from another computer, so
  you can see where it went. Turn it off with **Ripple when crossing** in the menu (Mac) or
  the Omarchy panel, or `mousetail set ripple off`.
- Pause a paired computer without forgetting it: the pause button beside it in the menu (or
  the Omarchy panel, or `mousetail pause <computer>`). Nothing crosses until you resume it,
  from either computer.
- Scrolling with a Mac's trackpad on a Linux computer feels like its own trackpad: its scroll
  speed and its apps' momentum apply, rather than the Mac's.
- Only one cursor on show: the computer you've just left hides its own until you use its
  mouse again, rather than leaving it sitting at the edge.
- Sound goes both ways: push from Linux onto a Mac and the Mac's sound comes to Linux too
  (it needs the System Audio Recording permission). Headphone buttons, media keys and
  the desktop's play controls play, pause and skip whatever the other computer is playing.
- The Omarchy panel can now pair, arrange (drag computers to where they sit, like Arrange
  Displays on a Mac), change settings and check for updates.
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
