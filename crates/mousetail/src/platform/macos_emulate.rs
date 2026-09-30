//! Being controlled on macOS: input from another computer is posted as HID-level Quartz
//! events, so every app sees it exactly like a real mouse and keyboard.
//! Media keys (from the computer this Mac's sound is playing on) are pressed the same way, so
//! whatever is playing here responds as if to its own keyboard.
//!
//! Every posted event carries `MOUSETAIL_EVENT` in its user-data field. Our own event tap skips
//! those, so input we inject can never be mistaken for the local user pushing at an edge.

use std::collections::HashSet;
use std::ffi::c_void;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use mousetail_core::keys::{ev, evdev_to_mac};
use mousetail_core::layout::Rect;
use mousetail_core::proto::{MediaKey, Scroll};
use objc2::encode::{Encoding, RefEncode};
use objc2::msg_send;
use objc2::rc::Retained;
use objc2_app_kit::{NSEvent, NSEventModifierFlags, NSEventType};
use objc2_foundation::NSPoint;

/// Marks events MouseTail posted (checked by the event tap in `macos.rs`).
pub const MOUSETAIL_EVENT: i64 = 0x4D544149; // "MTAI"

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
    Media(MediaKey),
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
            "MouseTail needs Accessibility permission to be controlled from another computer"
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

    /// Press a media key here, as if on this Mac's keyboard, so whatever is playing on it
    /// (and now sounding on another computer) responds.
    pub fn media(&self, key: MediaKey) {
        self.send(Cmd::Media(key));
    }
}

struct Injector {
    source: *mut c_void,
    pos: CGPoint,
    keys: HashSet<u16>,
    buttons: HashSet<u16>,
    last_click: Option<(u16, Instant, CGPoint)>,
    clicks: i64,
    /// Fractions of a pixel of smooth scrolling not posted yet (Quartz takes whole pixels),
    /// so slow scrolling adds up instead of rounding away to nothing.
    scroll_rest: (f64, f64),
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
            scroll_rest: (0.0, 0.0),
        }
    }

    fn run(mut self, rx: mpsc::Receiver<Cmd>) {
        while let Ok(cmd) = rx.recv() {
            match cmd {
                Cmd::Motion(x, y) => self.motion(CGPoint { x, y }),
                Cmd::Button(code, down) => self.button(code, down),
                Cmd::Key(code, down) => self.key(code, down),
                Cmd::Scroll(s) => self.scroll(s),
                Cmd::Media(key) => {
                    media_key(key, true);
                    media_key(key, false);
                }
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
            CGEventSetIntegerValueField(event, EVENT_SOURCE_USER_DATA, MOUSETAIL_EVENT);
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
        let e = match s.notches {
            Some((nx, ny)) => unsafe {
                CGEventCreateScrollWheelEvent2(self.source, 1, 2, -ny, -nx, 0)
            },
            None => {
                let x = self.scroll_rest.0 - s.dx;
                let y = self.scroll_rest.1 - s.dy;
                let (px, py) = (x.round(), y.round());
                self.scroll_rest = (x - px, y - py);
                if px == 0.0 && py == 0.0 {
                    return;
                }
                unsafe {
                    CGEventCreateScrollWheelEvent2(self.source, 0, 2, py as i32, px as i32, 0)
                }
            }
        };
        self.post(e);
    }
}

/// Press or release a media key.
fn media_key(key: MediaKey, down: bool) {
    objc2::rc::autoreleasepool(|_| {
        let Some(event) = media_event(key, down) else {
            return;
        };
        let cg = cg_event(&event);
        if cg.is_null() {
            return;
        }
        // The CGEvent belongs to the NSEvent, so it isn't released here.
        unsafe {
            CGEventSetIntegerValueField(cg, EVENT_SOURCE_USER_DATA, MOUSETAIL_EVENT);
            CGEventPost(HID_TAP, cg);
        }
    });
}

/// Media keys aren't key events but "system defined" ones (subtype 8, the auxiliary control
/// buttons), which only AppKit builds. The key goes in the top half of data1 and the
/// down/up state in the bottom, with the same state in the flags.
fn media_event(key: MediaKey, down: bool) -> Option<Retained<NSEvent>> {
    // NX_KEYTYPE_PLAY, NX_KEYTYPE_FAST and NX_KEYTYPE_REWIND: what F8, F9 and F7 send.
    let code: isize = match key {
        MediaKey::PlayPause => 16,
        MediaKey::Next => 19,
        MediaKey::Previous => 20,
    };
    let state: isize = if down { 0xa00 } else { 0xb00 };
    NSEvent::otherEventWithType_location_modifierFlags_timestamp_windowNumber_context_subtype_data1_data2(
        NSEventType::SystemDefined,
        NSPoint::new(0.0, 0.0),
        NSEventModifierFlags(state as usize),
        0.0,
        0,
        None,
        8,
        (code << 16) | state,
        -1,
    )
}

fn cg_event(event: &NSEvent) -> CGEventRef {
    let cg: *mut OpaqueCGEvent = unsafe { msg_send![event, CGEvent] };
    cg.cast()
}

/// `CGEventRef`'s target, so `msg_send!` can check the method's return type.
#[repr(C)]
struct OpaqueCGEvent {
    _private: [u8; 0],
}

unsafe impl RefEncode for OpaqueCGEvent {
    const ENCODING_REF: Encoding = Encoding::Pointer(&Encoding::Struct("__CGEvent", &[]));
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
    let name = CFString::from_static_string("MouseTail: another computer is using this Mac");
    let mut id = 0u32;
    unsafe {
        IOPMAssertionDeclareUserActivity(name.as_concrete_TypeRef() as *const c_void, 0, &mut id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Builds the events without posting them (posting would press the key on this Mac).
    #[test]
    fn media_events() {
        #[link(name = "ApplicationServices", kind = "framework")]
        unsafe extern "C" {
            fn CGEventGetType(event: CGEventRef) -> u32;
        }
        objc2::rc::autoreleasepool(|_| {
            for (key, code) in [
                (MediaKey::PlayPause, 16),
                (MediaKey::Next, 19),
                (MediaKey::Previous, 20),
            ] {
                for (down, state) in [(true, 0xa00), (false, 0xb00)] {
                    let event = media_event(key, down).unwrap();
                    assert_eq!(event.subtype().0, 8);
                    assert_eq!(event.data1(), (code << 16) | state);
                    let cg = cg_event(&event);
                    assert!(!cg.is_null());
                    assert_eq!(unsafe { CGEventGetType(cg) }, 14); // system defined
                }
            }
        });
    }
}
