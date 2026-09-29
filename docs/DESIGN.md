# Kiore — Design

Share one keyboard and mouse across machines on the same LAN. Move the cursor off the
edge of a screen and it appears on the neighbouring machine; move it back and you're home.
Clipboard follows you across.

Initial target: **macOS (controller)** → **Omarchy / Hyprland Linux (controlled)**.
The core is symmetric so the reverse direction can be added later.

## Goals

- **No fuss.** One install per machine, auto-discovery on the LAN, a one-time 4-digit pairing
  code, then it just reconnects forever.
- **Never trapped.** The cursor can always come home, whatever state the remote is in
  (lock screen, frozen compositor, network drop, sleep).
- **Multi-monitor aware** on both sides, with a drag-to-arrange layout editor like macOS
  *Displays → Arrange*.
- **Clipboard sync** (text first, then images/files).
- **Secure by default.** Everything is encrypted; only paired devices are accepted.
- **Small.** Native binaries, no runtimes, low idle CPU.

## Non-goals (v1)

- More than one remote machine at a time (the model supports it; the UI won't at first).
- Controlling the Mac from Linux (designed for, not built).
- File drag-and-drop between machines.
- Internet / cross-subnet use.

## Architecture

```
┌──────────────────────── macOS ───────────────────────┐        ┌─────────────── Omarchy ──────────────┐
│ Kiore.app (SwiftUI menu bar + arrangement window) │        │ kiore (systemd --user service)   │
│        │ local socket (JSON lines)                     │        │   ↑ local socket ← Omarchy bar plugin│
│ kiore-core (Rust)                                 │  QUIC  │ kiore-core (Rust)                │
│  • capture: CGEventTap                                │◄──────►│  • emulate: Wayland virtual pointer  │
│  • layout + virtual cursor (authoritative)            │ TLS1.3 │    + virtual keyboard (no root)      │
│  • clipboard: NSPasteboard                            │ pinned │  • clipboard: data-control           │
│  • discovery: mDNS                                    │        │  • discovery: mDNS                   │
└───────────────────────────────────────────────────────┘        │  • Omarchy bar plugin (status)       │
                                                                  └──────────────────────────────────────┘
```

- **`kiore-core`** (Rust library): protocol, pairing, crypto, discovery, layout maths,
  virtual cursor, key mapping, clipboard sync logic. Platform backends behind traits:
  `InputCapture`, `InputEmulation`, `Clipboard`, `DisplayInfo`.
- **macOS app** (`apps/macos`): SwiftUI menu bar app and arrangement window. It bundles the
  Rust daemon (`Contents/MacOS/kiored`), runs it while open, and talks to it over the
  daemon's local control socket (one JSON request/response per line). No FFI: the same socket
  serves the CLI and the Omarchy bar plugin, so every front end sees the same thing.
- **Linux daemon** `kiore run`: headless, runs as a systemd user service started with the
  Hyprland session. No window. Status via an Omarchy shell bar plugin (Omarchy 4 replaced
  Waybar with its Quickshell shell); pairing and events via desktop notifications.
  Feasibility of every piece below is recorded in [`research/RESULTS.md`](research/RESULTS.md).

### Roles

Each node can be a **controller** (owns the physical keyboard/mouse, captures input) or a
**target** (injects input). v1: Mac = controller, Linux = target. The protocol does not assume
which is which.

## The key idea: the controller is authoritative

The lock-screen trap in Lan Mouse happens because the *remote* decides when the cursor has hit
the edge to come back; when the lock screen owns input, it never finds out.

In Kiore the **controller decides everything**:

1. While the cursor is on the remote, the Mac keeps its event tap active, hides and pins its own
   cursor, and tracks a **virtual cursor** in the unified layout coordinate space.
2. It applies its own pointer acceleration to raw deltas, clamps to the remote's displays, and
   sends **absolute** positions to the target.
3. When the virtual cursor crosses a shared edge back to a Mac display, the Mac releases capture
   and warps its real cursor to the matching point. **No message from the remote is required.**
4. Safety valves:
   - Hotkey (default `Ctrl+Opt+Esc`) always returns control.
   - Heartbeat: if the target stops acking for ~1s, return control automatically.
   - Disconnect → immediate return; all held keys are released on the target.

Absolute positioning means the two sides can never drift out of agreement about where the edge
is — even over the lock screen.

## Layout model

All displays from all machines live in one **unified layout** in logical points (not pixels),
exactly like macOS arranges its own displays.

- Each machine reports its displays: id, name, logical size, scale, position within its own
  local space (macOS `CGDisplayBounds`; Hyprland `hyprctl monitors -j`).
- A machine's displays keep their local arrangement as a rigid group. The user places the
  **group** relative to the other machine's group.
- A crossing exists wherever an edge of one machine's display touches an edge of another
  machine's display. Partial overlaps are fine — only the touching segment is a crossing.
  So attaching the iMac to the left of the *Dell* (not the MacBook screen) means only the Dell's
  left edge leads to the iMac.
- Mapping across an edge is one-to-one in layout points along the shared segment (as macOS
  does between its own displays); the arrangement decides where screens line up.
- Display hot-plug on either side re-reports displays; the layout is kept by display identity
  where possible and snapped back to a valid touching position if not.

The layout is stored on the controller and synced to the peer, so a future reverse-direction mode
uses the same arrangement.

### Arrangement window (macOS)

Modelled on *System Settings → Displays → Arrange*:

- Every display drawn to scale; each machine's displays tinted as a group and labelled
  ("This Mac", "omarchy-imac").
- Drag the remote group; it snaps to edges of local displays. Shared edges are highlighted to
  show exactly where the cursor can cross.
- Click a display to flash an identifier on the real screen (both machines).
- Minimal settings alongside: clipboard sync on/off, return hotkey, modifier mapping.

## Input

### Capture (macOS)

- `CGEventTap` at the session level for mouse moves, buttons, scroll (including continuous /
  trackpad scroll and momentum phases) and keys (including `flagsChanged` for modifiers).
- While remote: events are swallowed; cursor hidden and dissociated
  (`CGAssociateMouseAndMouseCursorPosition(false)`), deltas read from the events.
- Requires Accessibility + Input Monitoring permissions (first-run guide in the app).

### Emulation (Linux)

Unprivileged Wayland protocols — no root, no udev rule (verified on Hyprland 0.56, including
at the Omarchy lock screen):

- **Pointer:** `zwlr_virtual_pointer_v1.motion_absolute` with the extent set to the whole
  Hyprland layout, so multi-monitor maps naturally. Buttons and `axis` scroll (with
  `axis_source`/`axis_discrete` for smooth vs notched) on the same object.
- **Keyboard:** `zwp_virtual_keyboard_v1`, loaded with the seat's own keymap so the iMac's
  layout applies; keys are evdev codes.
- Writes must wait for socket writability on `EWOULDBLOCK` rather than drop or panic.
- Fallback backend for non-wlroots compositors: uinput (then a udev rule is needed).

### Omarchy integration

- Injected input resets Omarchy's idle timers, so the iMac won't blank or lock while you're
  using it from the Mac.
- On entering the iMac, dismiss the screensaver the way Omarchy does (SIGTERM its script),
  since pointer motion alone doesn't close it.
- Hyprland 0.56 `hyprctl dispatch` takes Lua (`hl.dsp.…`); use the new syntax or the IPC
  socket directly.

### Key mapping

- Send **physical key positions** (mapped macOS virtual keycode → Linux evdev code). The target
  applies its own keyboard layout, so both machines should use the same layout.
- Modifier mapping (default, configurable):
  - `Cmd` → `Ctrl` so Cmd+C / Cmd+V / Cmd+T etc. behave as expected on Linux.
  - Exceptions stay `Super` for Hyprland: Cmd+Tab, Cmd+Space, Cmd+Return, Cmd+number,
    Cmd+arrows, and Cmd on its own. The exception list is editable.
  - `Ctrl` → `Ctrl`; `Opt` → `Alt`.
- On leaving the remote (or disconnecting) all pressed keys and buttons are released, so nothing
  gets stuck.
- Keys held at the moment of crossing are not carried across.

## Clipboard

- On crossing, the machine being **left** sends its clipboard to the machine being entered, if it
  has changed since the last sync. (Nothing is sent continuously.)
- macOS: `NSPasteboard.changeCount` to detect changes; read/write text, then images (PNG) and
  file URLs later.
- Linux: data-control (`wl-clipboard-rs`) to read/set the clipboard without a window. Omarchy
  also ships a clipboard-history plugin; synced entries will show up there naturally.
- Size cap (default 10 MB) and an on/off switch.

## Sound

The machine with the speakers (the Mac) asks the other to send its sound (`AudioWanted`) when
they connect, if the `audio` setting is on. The Linux side then creates a PipeWire
`Audio/Sink` named after the Mac (a real output in Omarchy's audio menu) and makes it the
default, remembering the previous default. Whatever plays into it is encoded with Opus
(48 kHz stereo, 10 ms frames, 160 kbit/s, in-band FEC) and sent as QUIC datagrams; silence
isn't sent. The Mac decodes into a playout buffer (40 ms cushion, conceals short gaps, re-buffers
after pauses, skips ahead if it falls >120 ms behind, resamples to the device rate) and plays
through the current default output, opening the device only while sound is arriving. On
disconnect or when switched off, the virtual speaker is removed and the previous default is
restored if the user hadn't picked something else. Opus is built in on macOS (self-contained
app) and uses the system library on Linux.

## Discovery and pairing

- Each node advertises `_kiore._tcp` via mDNS with its device id and name.
- Each device has a long-term self-signed certificate (its identity).
- First connection: the target shows a notification with a 4-digit code; the user enters it in
  the Mac's menu. The code authenticates an exchange of certificate fingerprints (SPAKE2), after
  which both sides pin each other's certificate.
- Each shown code allows one attempt. Every attempt counts against a machine-wide limit (3
  per 10 minutes, refunded on success) and codes are shown at most every 5 s, so guessing a
  4-digit code takes weeks, with a notification on screen for every try.
- Subsequent connections: mutual TLS with the pinned certificates. Unknown peers are rejected.
- Unpair from either side.

### Who connects to whom

Connection direction is independent of role. Both sides advertise and both dial; the first
authenticated connection wins and the other is dropped. This matters because Omarchy's ufw
blocks inbound connections by default while the Mac's application firewall allows them — so in
practice the iMac dials the Mac and the Omarchy install needs no firewall change and no sudo.

## Transport

- **QUIC** (`quinn`), one connection per peer, TLS 1.3 with pinned certificates.
  - Pointer motion: unreliable **datagrams** of absolute positions with a sequence number;
    latest wins, so loss self-heals and a Wi-Fi stall can't queue up stale motion. When the
    pointer rests, its final position is re-sent twice so a lost last packet can't leave the
    remote cursor short. (Measured on
    this mesh Wi-Fi: 5 ms average, occasional multi-second stalls.)
  - Keys, buttons, scroll, clipboard, control: reliable **streams**.
- One QUIC endpoint per local IPv4 address, all on one port, rebound as networks change. A
  single wildcard socket would answer from the primary (Wi-Fi) address even when the peer
  dialled the Ethernet address, and stateful firewalls drop those replies.
- Path selection: race every (local address, peer address) pair on the same subnet, give the
  stragglers 150 ms after the first success, keep the lowest RTT. Here that's the Mac's wired
  link (~7 ms vs ~26 ms over the Wi-Fi mesh).
- Messages: `Hello`, `Displays`, `Layout`, `Enter{pos}`, `Leave`, `PointerAbs`, `Button`,
  `Scroll`, `Key`, `ReleaseAll`, `Clipboard`, `Heartbeat`, `Ack`.
- Automatic reconnect with backoff; survives sleep/wake and network changes (QUIC connection
  migration helps when the Mac swaps between Wi-Fi and Ethernet).

## Install and running

**macOS**
- `Kiore.app` into /Applications (DMG or Homebrew cask later). Launch-at-login via
  `SMAppService`. Ad-hoc signed for now; a Developer ID later stops macOS from asking for the
  permissions again after updates.

**Omarchy**
- `curl -fsSL …/install.sh | sh` (AUR package later). **No sudo:** installs the binary to
  `~/.local/bin`, enables `kiored.service` (`systemctl --user`, bound to
  `graphical-session.target`), and installs the bar plugin into `~/.config/omarchy/plugins/`.
- Starts with the Hyprland session; restarts on failure. `kiore status` CLI for debugging.

## Local control socket

`$XDG_RUNTIME_DIR/kiore.sock` (Linux) or `~/Library/Application Support/Kiore/
kiore.sock` (macOS), mode 0600. Requests: `status`, `layout`, `pair`, `pair_code`,
`unpair`, `place_at` (drop + snap), `place`, `set_setting`, `release`. `kiore watch`
streams status as JSON lines for status bars.

## Milestones

1. **Spike** ✅ (see `research/RESULTS.md`).
2. **Core** ✅: discovery, code pairing, pinned-certificate QUIC, keyboard with Command
   mapping, stuck-key protection, dead-peer detection (~1.5 s) and automatic reconnect.
3. **Multi-monitor layout** ✅: unified layout, drop-to-snap placement, per-peer placement saved.
4. **Clipboard** ✅ text. Images and files still to do.
5. **Mac app** ✅ first version: menu bar, pairing, arrangement window, permission guidance,
   Secure Input warning. Still to do: app icon, signed/notarised DMG, first-run walkthrough.
6. **Linux** ✅ first version: no-sudo installer/uninstaller, systemd user service, Omarchy bar
   plugin with pairing code, screensaver handling. Still to do: prebuilt binaries (install
   without Rust), AUR package.
7. **Sound** ✅ first version: Linux → Mac, switchable from either side.
8. **Later:** reverse direction (Linux capture via evdev grab or InputCapture portal, macOS
   injection via `CGEventPost`), multiple remotes, file transfer, image clipboard.

## Known issues / next up

- **Keyboard end-to-end test** (typing, Command → Ctrl/Super, hotkey) still needs a run with
  the Mac unlocked; macOS Secure Input blocks synthetic keys while it's locked.
- Occasional multi-second Wi-Fi stalls (likely AWDL); reconnect backoff is capped at 4 s.

## Reference setup

Developed and tested on:

- **Controller:** MacBook Pro (Apple Silicon), macOS 27. Built-in display (main) with a 27"
  external display above it; Wi-Fi plus USB Ethernet.
- **Target:** an Intel iMac running Omarchy 4 (Hyprland 0.56, PipeWire 1.6), on Wi-Fi, with
  ufw denying inbound connections, sitting to the left of the Mac.
- `dev/` holds the tools used for this: `sync.sh` copies the tree to the Linux machine
  (`KIORE_REMOTE`, an SSH host), `dev.sh` rebuilds and restarts both sides, and `e2e.sh`
  drives the Mac with synthetic input and checks the Linux side over SSH.
- mDNS needs both machines on the same LAN.
