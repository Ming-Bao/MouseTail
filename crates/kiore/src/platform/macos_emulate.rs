//! Being controlled on macOS: input from another computer is posted as HID-level Quartz
//! events, so every app sees it exactly like a real mouse and keyboard.
//!
//! Every posted event carries `KIORE_EVENT` in its user-data field. Our own event tap skips
//! those, so input we inject can never be mistaken for the local user pushing at an edge.

use std::collections::HashSet;
use std::ffi::c_void;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use kiore_core::keys::{ev, evdev_to_mac};
use kiore_core::layout::Rect;
use kiore_core::proto::Scroll;

/// Marks events Kiore posted (checked by the event tap in `macos.rs`).
pub const KIORE_EVENT: i64 = 0x4B494F52; // "KIOR"

#[repr(C)]
#[derive(Clone, Copy, Default, PartialEq)]
struct CGPoint {
    x: f64,
    y: f64,
}

type CGEventRef = *mut c_void;

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrusted() -> bool;
    fn CGEventSourceCreate(state: i32) -> *mut c_void;
    fn CGEventCreateMouseEvent(source: *mut c_void, t: u32, at: CGPoint, button: u32)
    -> CGEventRef;
    fn CGEventCreateKeyboardEvent(source: *mut c_void, vk: u16, down: bool) -> CGEventRef;
    fn CGEventCreateScrollWheelEvent2(
        source: *mut c_void,
        units: u32,
        count: u32,
        wheel1: i32,
        wheel2: i32,
        wheel3: i32,
    ) -> CGEventRef;
    fn CGEventSetIntegerValueField(event: CGEventRef, field: u32, value: i64);
    fn CGEventSetDoubleValueField(event: CGEventRef, field: u32, value: f64);
    fn CGEventSetFlags(event: CGEventRef, flags: u64);
    fn CGEventPost(tap: u32, event: CGEventRef);
    fn CFRelease(cf: *mut c_void);
}

const HID_TAP: u32 = 0;
const HID_SYSTEM_STATE: i32 = 1;

// Event types.
const LEFT_DOWN: u32 = 1;
const LEFT_UP: u32 = 2;
const RIGHT_DOWN: u32 = 3;
const RIGHT_UP: u32 = 4;
const MOVED: u32 = 5;
const LEFT_DRAGGED: u32 = 6;
const RIGHT_DRAGGED: u32 = 7;
const OTHER_DOWN: u32 = 25;
const OTHER_UP: u32 = 26;
const OTHER_DRAGGED: u32 = 27;

// Event fields.
const MOUSE_CLICK_STATE: u32 = 1;
const MOUSE_BUTTON_NUMBER: u32 = 3;
const MOUSE_DELTA_X: u32 = 4;
const MOUSE_DELTA_Y: u32 = 5;
const EVENT_SOURCE_USER_DATA: u32 = 42;

// Modifier flags (device-independent | device-dependent bits for left/right).
fn modifier_flags(held: &HashSet<u16>) -> u64 {
    let mut f = 0u64;
    for code in held {
        f |= match *code {
            ev::LEFTSHIFT => 0x0002_0000 | 0x02,
            ev::RIGHTSHIFT => 0x0002_0000 | 0x04,
            ev::LEFTCTRL => 0x0004_0000 | 0x01,
            ev::RIGHTCTRL => 0x0004_0000 | 0x2000,
            ev::LEFTALT => 0x0008_0000 | 0x20,
            ev::RIGHTALT => 0x0008_0000 | 0x40,
            ev::LEFTMETA => 0x0010_0000 | 0x08,
            ev::RIGHTMETA => 0x0010_0000 | 0x10,
            _ => 0,
        };
    }
    f
}

fn is_modifier(code: u16) -> bool {
    matches!(
        code,
        ev::LEFTSHIFT
            | ev::RIGHTSHIFT
            | ev::LEFTCTRL
            | ev::RIGHTCTRL
            | ev::LEFTALT
            | ev::RIGHTALT
            | ev::LEFTMETA
            | ev::RIGHTMETA
    )
}

enum Cmd {
    Motion(f64, f64),
    Button(u16, bool),
    Key(u16, bool),
    Scroll(Scroll),
    ReleaseAll,
}

pub struct Emulator {
    tx: mpsc::Sender<Cmd>,
}

impl Emulator {
    pub fn supported() -> bool {
        true
    }

    /// Posting input needs Accessibility permission (the same one capturing uses).
    pub fn start() -> anyhow::Result<Self> {
        anyhow::ensure!(
            unsafe { AXIsProcessTrusted() },
            "Kiore needs Accessibility permission to be controlled from another computer"
        );
        let (tx, rx) = mpsc::channel();
        thread::Builder::new()
            .name("emulator".into())
            .spawn(move || Injector::new().run(rx))?;
        Ok(Self { tx })
    }

    fn send(&self, cmd: Cmd) {
        let _ = self.tx.send(cmd);
    }

    /// macOS positions are global already; nothing to map.
    pub fn set_bounds(&self, _bounds: Rect) {}

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

struct Injector {
    source: *mut c_void,
    pos: CGPoint,
    keys: HashSet<u16>,
    buttons: HashSet<u16>,
    last_click: Option<(u16, Instant, CGPoint)>,
    clicks: i64,
}

// The source is only touched from the injector thread.
unsafe impl Send for Injector {}

impl Injector {
    fn new() -> Self {
        Self {
            source: unsafe { CGEventSourceCreate(HID_SYSTEM_STATE) },
            pos: CGPoint::default(),
            keys: HashSet::new(),
            buttons: HashSet::new(),
            last_click: None,
            clicks: 1,
        }
    }

    fn run(mut self, rx: mpsc::Receiver<Cmd>) {
        while let Ok(cmd) = rx.recv() {
            match cmd {
                Cmd::Motion(x, y) => self.motion(CGPoint { x, y }),
                Cmd::Button(code, down) => self.button(code, down),
                Cmd::Key(code, down) => self.key(code, down),
                Cmd::Scroll(s) => self.scroll(s),
                Cmd::ReleaseAll => {
                    for code in self.keys.clone() {
                        self.key(code, false);
                    }
                    for code in self.buttons.clone() {
                        self.button(code, false);
                    }
                }
            }
        }
    }

    fn post(&self, event: CGEventRef) {
        if event.is_null() {
            return;
        }
        unsafe {
            CGEventSetIntegerValueField(event, EVENT_SOURCE_USER_DATA, KIORE_EVENT);
            CGEventSetFlags(event, modifier_flags(&self.keys));
            CGEventPost(HID_TAP, event);
            CFRelease(event);
        }
    }

    fn motion(&mut self, to: CGPoint) {
        let (dx, dy) = (to.x - self.pos.x, to.y - self.pos.y);
        self.pos = to;
        // While a button is held this is a drag, which apps treat differently from a move.
        let (kind, button) = if self.buttons.contains(&ev::BTN_LEFT) {
            (LEFT_DRAGGED, 0)
        } else if self.buttons.contains(&ev::BTN_RIGHT) {
            (RIGHT_DRAGGED, 1)
        } else if let Some(b) = self.buttons.iter().next() {
            (OTHER_DRAGGED, button_number(*b))
        } else {
            (MOVED, 0)
        };
        unsafe {
            let e = CGEventCreateMouseEvent(self.source, kind, to, button);
            if !e.is_null() {
                CGEventSetDoubleValueField(e, MOUSE_DELTA_X, dx);
                CGEventSetDoubleValueField(e, MOUSE_DELTA_Y, dy);
                CGEventSetIntegerValueField(e, MOUSE_DELTA_X, dx.round() as i64);
                CGEventSetIntegerValueField(e, MOUSE_DELTA_Y, dy.round() as i64);
            }
            self.post(e);
        }
    }

    fn button(&mut self, code: u16, down: bool) {
        if down {
            if !self.buttons.insert(code) {
                return;
            }
            // Double and triple clicks: same button, close in time and place.
            let now = Instant::now();
            self.clicks = match self.last_click {
                Some((b, t, p))
                    if b == code
                        && now.duration_since(t) < Duration::from_millis(450)
                        && (p.x - self.pos.x).abs() < 5.0
                        && (p.y - self.pos.y).abs() < 5.0 =>
                {
                    self.clicks + 1
                }
                _ => 1,
            };
            self.last_click = Some((code, now, self.pos));
        } else if !self.buttons.remove(&code) {
            return;
        }
        let n = button_number(code);
        let kind = match (n, down) {
            (0, true) => LEFT_DOWN,
            (0, false) => LEFT_UP,
            (1, true) => RIGHT_DOWN,
            (1, false) => RIGHT_UP,
            (_, true) => OTHER_DOWN,
            (_, false) => OTHER_UP,
        };
        unsafe {
            let e = CGEventCreateMouseEvent(self.source, kind, self.pos, n);
            if !e.is_null() {
                CGEventSetIntegerValueField(e, MOUSE_BUTTON_NUMBER, n as i64);
                CGEventSetIntegerValueField(e, MOUSE_CLICK_STATE, self.clicks);
            }
            self.post(e);
        }
    }

    fn key(&mut self, code: u16, down: bool) {
        if down {
            if !self.keys.insert(code) {
                return; // the Mac repeats held keys itself
            }
        } else if !self.keys.remove(&code) {
            return;
        }
        let Some(vk) = evdev_to_mac(code) else { return };
        // Modifiers are posted as keys too. Flags on every event come from `keys`, which
        // already reflects this change, so the modifier's own event carries the new state.
        debug_assert!(!is_modifier(code) || self.keys.contains(&code) == down);
        unsafe {
            let e = CGEventCreateKeyboardEvent(self.source, vk, down);
            self.post(e);
        }
    }

    fn scroll(&mut self, s: Scroll) {
        // Scroll deltas arrive in the Wayland convention (positive = view moves down/right);
        // Quartz is the other way round.
        unsafe {
            let e = match s.notches {
                Some((nx, ny)) => CGEventCreateScrollWheelEvent2(self.source, 1, 2, -ny, -nx, 0),
                None => CGEventCreateScrollWheelEvent2(
                    self.source,
                    0,
                    2,
                    -s.dy.round() as i32,
                    -s.dx.round() as i32,
                    0,
                ),
            };
            self.post(e);
        }
    }
}

fn button_number(code: u16) -> u32 {
    match code {
        ev::BTN_LEFT => 0,
        ev::BTN_RIGHT => 1,
        ev::BTN_MIDDLE => 2,
        ev::BTN_SIDE => 3,
        ev::BTN_EXTRA => 4,
        c => (c.saturating_sub(ev::BTN_LEFT)) as u32,
    }
}

/// Wake the display (and keep the Mac from idling) when another computer's cursor arrives.
pub fn on_enter() {
    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        fn IOPMAssertionDeclareUserActivity(name: *const c_void, kind: u32, id: *mut u32) -> i32;
    }
    use core_foundation::base::TCFType;
    use core_foundation::string::CFString;
    let name = CFString::from_static_string("Kiore: another computer is using this Mac");
    let mut id = 0u32;
    unsafe {
        IOPMAssertionDeclareUserActivity(name.as_concrete_TypeRef() as *const c_void, 0, &mut id);
    }
}
