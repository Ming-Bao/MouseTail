//! Linux / Wayland backend.
//!
//! Injection uses unprivileged Wayland protocols (`zwlr_virtual_pointer_v1`,
//! `zwp_virtual_keyboard_v1`), so no root or udev rules are needed. Display geometry comes
//! from `xdg-output`; shortcuts and the screensaver from Hyprland / Omarchy when present.

pub mod audio;
pub mod capture;
mod outputs;
mod uinput;

use std::collections::HashSet;
use std::os::fd::{AsFd, OwnedFd};
use std::process::Command;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Context;
use mousetail_core::keys::{CommandRemap, ev};
use mousetail_core::layout::Rect;
use mousetail_core::proto::{DisplayInfo, Scroll};
use serde::Deserialize;
use wayland_client::protocol::wl_pointer::{Axis, AxisSource, ButtonState};
use wayland_client::protocol::{wl_keyboard, wl_registry, wl_seat};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, WEnum, delegate_noop};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
    zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1,
    zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1,
};
use wayland_protocols_wlr::virtual_pointer::v1::client::{
    zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1,
    zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1,
};

// ---------------------------------------------------------------------------------------------
// Displays, shortcuts, screensaver, notifications

#[derive(Deserialize)]
struct HyprMonitor {
    name: String,
    #[serde(default)]
    description: String,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    scale: f64,
    #[serde(default)]
    transform: u32,
    #[serde(default)]
    disabled: bool,
}

pub fn displays() -> Vec<DisplayInfo> {
    // Any Wayland compositor; Hyprland's own tool as a fallback.
    if let Some(d) = outputs::list() {
        return d;
    }
    let Some(monitors) = hyprctl_json::<Vec<HyprMonitor>>(&["monitors"]) else {
        return vec![];
    };
    let mut out: Vec<DisplayInfo> = monitors
        .into_iter()
        .filter(|m| !m.disabled)
        .map(|m| {
            let scale = if m.scale > 0.0 { m.scale } else { 1.0 };
            let (mut w, mut h) = (m.width / scale, m.height / scale);
            if m.transform % 2 == 1 {
                std::mem::swap(&mut w, &mut h);
            }
            DisplayInfo {
                id: m.name.clone(),
                name: if m.description.is_empty() {
                    m.name
                } else {
                    m.description
                },
                rect: Rect::new(m.x, m.y, w.round(), h.round()),
                scale,
                primary: false,
            }
        })
        .collect();
    // Hyprland has no primary display; call the one at the origin (or the first) primary.
    let primary = out
        .iter()
        .position(|d| d.rect.x == 0.0 && d.rect.y == 0.0)
        .unwrap_or(0);
    if let Some(d) = out.get_mut(primary) {
        d.primary = true;
    }
    out
}

#[derive(Deserialize)]
struct HyprBind {
    modmask: u32,
    #[serde(default)]
    key: String,
}

/// Command → Super for desktop shortcuts, plus copy/paste when the desktop binds Super+C/V/X
/// to universal clipboard actions (Omarchy does).
pub fn command_super_keys() -> HashSet<u16> {
    const SUPER: u32 = 64;
    let mut keys = CommandRemap::default_super_keys();
    if let Some(binds) = hyprctl_json::<Vec<HyprBind>>(&["binds"]) {
        for b in binds.iter().filter(|b| b.modmask == SUPER) {
            match b.key.to_ascii_uppercase().as_str() {
                "C" => keys.insert(ev::C),
                "V" => keys.insert(ev::V),
                "X" => keys.insert(ev::X),
                _ => false,
            };
        }
    }
    keys
}

#[derive(Deserialize)]
struct HyprClient {
    class: String,
    pid: i64,
}

/// A controller's cursor just arrived: wake the screen. Omarchy's screensaver only closes on
/// a key press, so end it the way Omarchy's own lock does (SIGTERM, which restores the
/// cursor).
pub fn on_enter() {
    thread::spawn(|| {
        // Turn the backlight back on if the lock screen blanked it.
        let _ = Command::new("omarchy-system-wake").status();
        let Some(clients) = hyprctl_json::<Vec<HyprClient>>(&["clients"]) else {
            return;
        };
        for c in clients
            .iter()
            .filter(|c| c.class == "org.omarchy.screensaver")
        {
            if c.pid > 0 {
                let _ = Command::new("kill").arg(c.pid.to_string()).status();
            }
        }
    });
}

const TEXT_MIME: &str = "text/plain;charset=utf-8";

/// Plain-text clipboard contents, if any (via data-control; no window needed).
pub fn clipboard_get() -> Option<(String, Vec<u8>)> {
    use std::io::Read;
    use wl_clipboard_rs::paste::{ClipboardType, MimeType, Seat, get_contents};
    let (mut pipe, _) =
        get_contents(ClipboardType::Regular, Seat::Unspecified, MimeType::Text).ok()?;
    let mut data = vec![];
    pipe.read_to_end(&mut data).ok()?;
    (!data.is_empty()).then(|| (TEXT_MIME.to_string(), data))
}

pub fn clipboard_set(mime: &str, data: &[u8]) {
    use wl_clipboard_rs::copy::{MimeType, Options, Source};
    if !mime.starts_with("text/") {
        return;
    }
    // Served from a background thread in this process until something else is copied.
    let result = Options::new().copy(
        Source::Bytes(data.to_vec().into_boxed_slice()),
        MimeType::Text,
    );
    if let Err(e) = result {
        tracing::warn!("couldn't set the clipboard: {e}");
    }
}

/// Hardware addresses of physical network interfaces, for Wake-on-LAN.
pub fn wake_macs() -> Vec<String> {
    let Ok(entries) = std::fs::read_dir("/sys/class/net") else {
        return vec![];
    };
    let mut macs: Vec<String> = entries
        .flatten()
        .filter(|e| e.path().join("device").exists())
        .filter_map(|e| std::fs::read_to_string(e.path().join("address")).ok())
        .map(|m| m.trim().to_string())
        .filter(|m| !m.is_empty() && m != "00:00:00:00:00:00")
        .collect();
    macs.sort();
    macs
}

pub fn notify(title: &str, body: &str) {
    let _ = Command::new("notify-send")
        .args(["--app-name=MouseTail", title, body])
        .spawn();
}

fn hyprctl_json<T: for<'de> Deserialize<'de>>(args: &[&str]) -> Option<T> {
    let out = Command::new("hyprctl").arg("-j").args(args).output().ok()?;
    serde_json::from_slice(&out.stdout).ok()
}

// ---------------------------------------------------------------------------------------------
// Injection

enum Cmd {
    Bounds(Rect),
    Motion(f64, f64),
    Button(u16, bool),
    Key(u16, bool),
    Scroll(Scroll),
    ReleaseAll,
}

/// Handle to the injection thread, which owns the Wayland connection.
pub struct Emulator {
    tx: mpsc::Sender<Cmd>,
}

impl Emulator {
    pub fn supported() -> bool {
        true
    }

    pub fn start() -> anyhow::Result<Self> {
        let (tx, rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel();
        thread::Builder::new()
            .name("emulator".into())
            .spawn(move || match Injector::connect() {
                Ok(mut inj) => {
                    let _ = ready_tx.send(Ok(()));
                    if let Err(e) = inj.run(rx) {
                        tracing::error!("input injection stopped: {e:#}");
                        // Without injection this machine is useless as a target; let the
                        // service manager restart us with a fresh Wayland connection.
                        std::process::exit(1);
                    }
                }
                // GNOME, KDE, X11…: a kernel virtual device instead.
                Err(wayland) => match uinput::Uinput::new() {
                    Ok(dev) => {
                        tracing::info!("injecting input through /dev/uinput ({wayland:#})");
                        let _ = ready_tx.send(Ok(()));
                        dev.run(rx);
                    }
                    Err(e) => {
                        let _ = ready_tx.send(Err(anyhow::anyhow!("{wayland:#}; {e:#}")));
                    }
                },
            })?;
        ready_rx.recv().context("emulator thread died")??;
        let emulator = Self { tx };
        if let Some(bounds) = Rect::bounding(displays().iter().map(|d| d.rect)) {
            emulator.set_bounds(bounds);
        }
        Ok(emulator)
    }

    fn send(&self, cmd: Cmd) {
        let _ = self.tx.send(cmd);
    }

    pub fn set_bounds(&self, bounds: Rect) {
        self.send(Cmd::Bounds(bounds));
    }

    pub fn motion(&self, x: f64, y: f64) {
        self.send(Cmd::Motion(x, y));
    }

    pub fn button(&self, code: u16, down: bool) {
        self.send(Cmd::Button(code, down));
    }

    pub fn key(&self, code: u16, down: bool) {
        self.send(Cmd::Key(code, down));
    }

    pub fn scroll(&self, scroll: Scroll) {
        self.send(Cmd::Scroll(scroll));
    }

    pub fn release_all(&self) {
        self.send(Cmd::ReleaseAll);
    }
}

/// Sub-pixel resolution for absolute motion.
const SUBPIXEL: f64 = 8.0;

// Standard xkb modifier masks for evdev keymaps.
const MOD_SHIFT: u32 = 1;
const MOD_LOCK: u32 = 2;
const MOD_CTRL: u32 = 4;
const MOD_ALT: u32 = 8;
const MOD_SUPER: u32 = 64;

struct Injector {
    conn: Connection,
    queue: EventQueue<State>,
    state: State,
    pointer: ZwlrVirtualPointerV1,
    keyboard: ZwpVirtualKeyboardV1,
    bounds: Rect,
    start: Instant,
    keys: HashSet<u16>,
    buttons: HashSet<u16>,
    caps_locked: bool,
    mods: u32,
}

impl Injector {
    fn connect() -> anyhow::Result<Self> {
        let conn = Connection::connect_to_env().context("connecting to the Wayland compositor")?;
        let mut queue = conn.new_event_queue();
        let qh = queue.handle();
        conn.display().get_registry(&qh, ());
        let mut state = State::default();
        queue.roundtrip(&mut state)?;

        let seat = state.seat.clone().context("compositor has no seat")?;
        let vpm = state
            .vpm
            .clone()
            .context("compositor lacks zwlr_virtual_pointer_manager_v1")?;
        let vkm = state
            .vkm
            .clone()
            .context("compositor lacks zwp_virtual_keyboard_manager_v1")?;

        // Borrow the seat's keymap so the user's own layout applies.
        let kb = seat.get_keyboard(&qh, ());
        queue.roundtrip(&mut state)?;
        let (format, fd, size) = state.keymap.take().context("seat sent no keymap")?;
        kb.release();

        let pointer = vpm.create_virtual_pointer(Some(&seat), &qh, ());
        let keyboard = vkm.create_virtual_keyboard(&seat, &qh, ());
        keyboard.keymap(format, fd.as_fd(), size);
        queue.roundtrip(&mut state)?;

        Ok(Self {
            conn,
            queue,
            state,
            pointer,
            keyboard,
            bounds: Rect::new(0.0, 0.0, 1920.0, 1080.0),
            start: Instant::now(),
            keys: HashSet::new(),
            buttons: HashSet::new(),
            caps_locked: false,
            mods: 0,
        })
    }

    fn now(&self) -> u32 {
        self.start.elapsed().as_millis() as u32
    }

    fn run(&mut self, rx: mpsc::Receiver<Cmd>) -> anyhow::Result<()> {
        loop {
            match rx.recv_timeout(Duration::from_millis(500)) {
                Ok(cmd) => self.apply(cmd),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
            }
            // Drain anything the compositor sent so its buffer never fills.
            if let Some(guard) = self.conn.prepare_read() {
                let _ = guard.read();
            }
            self.queue.dispatch_pending(&mut self.state)?;
            self.flush()?;
        }
    }

    fn apply(&mut self, cmd: Cmd) {
        let t = self.now();
        match cmd {
            Cmd::Bounds(b) => self.bounds = b,
            Cmd::Motion(x, y) => {
                let b = self.bounds;
                let x = ((x - b.x).clamp(0.0, b.w - 1.0) * SUBPIXEL) as u32;
                let y = ((y - b.y).clamp(0.0, b.h - 1.0) * SUBPIXEL) as u32;
                self.pointer.motion_absolute(
                    t,
                    x,
                    y,
                    (b.w * SUBPIXEL) as u32,
                    (b.h * SUBPIXEL) as u32,
                );
                self.pointer.frame();
            }
            Cmd::Button(code, down) => {
                if down {
                    self.buttons.insert(code);
                } else if !self.buttons.remove(&code) {
                    return;
                }
                let state = if down {
                    ButtonState::Pressed
                } else {
                    ButtonState::Released
                };
                self.pointer.button(t, code as u32, state);
                self.pointer.frame();
            }
            Cmd::Key(code, down) => self.key(t, code, down),
            Cmd::Scroll(s) => {
                match s.notches {
                    Some((nx, ny)) => {
                        self.pointer.axis_source(AxisSource::Wheel);
                        if ny != 0 {
                            self.pointer.axis_discrete(
                                t,
                                Axis::VerticalScroll,
                                ny as f64 * 15.0,
                                ny,
                            );
                        }
                        if nx != 0 {
                            self.pointer.axis_discrete(
                                t,
                                Axis::HorizontalScroll,
                                nx as f64 * 15.0,
                                nx,
                            );
                        }
                    }
                    None => {
                        // Continuous: the Mac already applies momentum, so don't use
                        // `Finger` (clients would add their own kinetic scrolling).
                        self.pointer.axis_source(AxisSource::Continuous);
                        if s.dy != 0.0 {
                            self.pointer.axis(t, Axis::VerticalScroll, s.dy);
                        }
                        if s.dx != 0.0 {
                            self.pointer.axis(t, Axis::HorizontalScroll, s.dx);
                        }
                    }
                }
                self.pointer.frame();
            }
            Cmd::ReleaseAll => {
                for code in std::mem::take(&mut self.keys) {
                    self.keyboard.key(t, code as u32, 0);
                }
                for code in std::mem::take(&mut self.buttons) {
                    self.pointer.button(t, code as u32, ButtonState::Released);
                }
                self.pointer.frame();
                self.mods = 0;
                self.send_modifiers();
            }
        }
    }

    fn key(&mut self, t: u32, code: u16, down: bool) {
        if down {
            // Our own key repeat is the compositor's job; ignore duplicate downs.
            if !self.keys.insert(code) {
                return;
            }
        } else if !self.keys.remove(&code) {
            return;
        }
        self.keyboard.key(t, code as u32, down as u32);
        if code == ev::CAPSLOCK && down {
            self.caps_locked = !self.caps_locked;
        }
        let mods = self.keys.iter().fold(0, |m, k| {
            m | match *k {
                ev::LEFTSHIFT | ev::RIGHTSHIFT => MOD_SHIFT,
                ev::LEFTCTRL | ev::RIGHTCTRL => MOD_CTRL,
                ev::LEFTALT | ev::RIGHTALT => MOD_ALT,
                ev::LEFTMETA | ev::RIGHTMETA => MOD_SUPER,
                _ => 0,
            }
        });
        if mods != self.mods || code == ev::CAPSLOCK {
            self.mods = mods;
            self.send_modifiers();
        }
    }

    fn send_modifiers(&self) {
        let locked = if self.caps_locked { MOD_LOCK } else { 0 };
        self.keyboard.modifiers(self.mods, 0, locked, 0);
    }

    /// Write out pending requests, waiting briefly if the socket is momentarily full.
    fn flush(&self) -> anyhow::Result<()> {
        for _ in 0..500 {
            match self.conn.flush() {
                Ok(()) => return Ok(()),
                Err(wayland_client::backend::WaylandError::Io(e))
                    if e.kind() == std::io::ErrorKind::WouldBlock =>
                {
                    thread::sleep(Duration::from_millis(1));
                }
                Err(e) => return Err(e.into()),
            }
        }
        anyhow::bail!("compositor stopped reading input")
    }
}

#[derive(Default)]
struct State {
    seat: Option<wl_seat::WlSeat>,
    vpm: Option<ZwlrVirtualPointerManagerV1>,
    vkm: Option<ZwpVirtualKeyboardManagerV1>,
    keymap: Option<(u32, OwnedFd, u32)>,
}

impl Dispatch<wl_registry::WlRegistry, ()> for State {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            match interface.as_str() {
                "wl_seat" if state.seat.is_none() => {
                    state.seat = Some(registry.bind(name, version.min(7), qh, ()))
                }
                "zwlr_virtual_pointer_manager_v1" => {
                    state.vpm = Some(registry.bind(name, version.min(2), qh, ()))
                }
                "zwp_virtual_keyboard_manager_v1" => {
                    state.vkm = Some(registry.bind(name, 1, qh, ()))
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<wl_keyboard::WlKeyboard, ()> for State {
    fn event(
        state: &mut Self,
        _: &wl_keyboard::WlKeyboard,
        event: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_keyboard::Event::Keymap { format, fd, size } = event {
            let format = match format {
                WEnum::Value(v) => u32::from(v),
                WEnum::Unknown(u) => u,
            };
            state.keymap = Some((format, fd, size));
        }
    }
}

delegate_noop!(State: ignore wl_seat::WlSeat);
delegate_noop!(State: ZwlrVirtualPointerManagerV1);
delegate_noop!(State: ZwlrVirtualPointerV1);
delegate_noop!(State: ZwpVirtualKeyboardManagerV1);
delegate_noop!(State: ZwpVirtualKeyboardV1);
