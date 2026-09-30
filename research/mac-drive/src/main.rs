//! Test driver: posts synthetic HID-level input on the Mac, as if from a real mouse and
//! keyboard, so MouseTail can be exercised end to end without anyone at the desk.
//!
//!   spike-mac-drive where                  print the cursor position
//!   spike-mac-drive warp <x> <y>           move the cursor to a point
//!   spike-mac-drive push <dx> <dy> <n>     post n relative moves (like sliding the mouse)
//!   spike-mac-drive click                  left click at the cursor
//!   spike-mac-drive scroll <lines>         notched wheel scroll (positive = wheel up)
//!   spike-mac-drive trackpad <points> <n>  two-finger scroll: n moves, lift, then momentum
//!   spike-mac-drive type <text>            type lowercase ascii, digits and spaces
//!   spike-mac-drive key <vk> [cmd|ctrl|opt|shift]...   press a key with modifiers

#[cfg(not(target_os = "macos"))]
fn main() {}

#[cfg(target_os = "macos")]
fn main() {
    mac::run();
}

#[cfg(target_os = "macos")]
mod mac {
    use std::ffi::c_void;
    use std::thread::sleep;
    use std::time::Duration;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CGPoint {
        x: f64,
        y: f64,
    }

    #[link(name = "ApplicationServices", kind = "framework")]
    unsafe extern "C" {
        fn CGEventCreate(source: *mut c_void) -> *mut c_void;
        fn CGEventGetLocation(e: *mut c_void) -> CGPoint;
        fn CGEventCreateMouseEvent(s: *mut c_void, t: u32, p: CGPoint, b: u32) -> *mut c_void;
        fn CGEventCreateKeyboardEvent(s: *mut c_void, vk: u16, down: bool) -> *mut c_void;
        fn CGEventCreateScrollWheelEvent(
            s: *mut c_void,
            units: u32,
            count: u32,
            w1: i32,
        ) -> *mut c_void;
        fn CGEventSetIntegerValueField(e: *mut c_void, f: u32, v: i64);
        fn CGEventSetDoubleValueField(e: *mut c_void, f: u32, v: f64);
        fn CGEventSetFlags(e: *mut c_void, flags: u64);
        fn CGEventPost(tap: u32, e: *mut c_void);
        fn CFRelease(cf: *mut c_void);
    }

    const HID: u32 = 0;
    const MOVED: u32 = 5;

    fn post(e: *mut c_void) {
        unsafe {
            CGEventPost(HID, e);
            CFRelease(e);
        }
        sleep(Duration::from_millis(8));
    }

    fn here() -> CGPoint {
        unsafe {
            let e = CGEventCreate(std::ptr::null_mut());
            let p = CGEventGetLocation(e);
            CFRelease(e);
            p
        }
    }

    fn vk(c: char) -> Option<u16> {
        const MAP: &[(char, u16)] = &[
            ('a', 0x00),
            ('s', 0x01),
            ('d', 0x02),
            ('f', 0x03),
            ('h', 0x04),
            ('g', 0x05),
            ('z', 0x06),
            ('x', 0x07),
            ('c', 0x08),
            ('v', 0x09),
            ('b', 0x0B),
            ('q', 0x0C),
            ('w', 0x0D),
            ('e', 0x0E),
            ('r', 0x0F),
            ('y', 0x10),
            ('t', 0x11),
            ('1', 0x12),
            ('2', 0x13),
            ('3', 0x14),
            ('4', 0x15),
            ('6', 0x16),
            ('5', 0x17),
            ('9', 0x19),
            ('7', 0x1A),
            ('8', 0x1C),
            ('0', 0x1D),
            ('o', 0x1F),
            ('u', 0x20),
            ('i', 0x22),
            ('p', 0x23),
            ('l', 0x25),
            ('j', 0x26),
            ('k', 0x28),
            ('n', 0x2D),
            ('m', 0x2E),
            (' ', 0x31),
            ('\n', 0x24),
        ];
        MAP.iter().find(|(ch, _)| *ch == c).map(|(_, v)| *v)
    }

    fn key(code: u16, flags: u64) {
        for down in [true, false] {
            let e = unsafe { CGEventCreateKeyboardEvent(std::ptr::null_mut(), code, down) };
            if flags != 0 {
                unsafe { CGEventSetFlags(e, flags) };
            }
            post(e);
        }
    }

    pub fn run() {
        let args: Vec<String> = std::env::args().skip(1).collect();
        let num = |i: usize| args[i].parse::<f64>().unwrap();
        match args.first().map(String::as_str) {
            Some("where") => {
                let p = here();
                println!("{}, {}", p.x, p.y);
            }
            Some("warp") => unsafe {
                post(CGEventCreateMouseEvent(
                    std::ptr::null_mut(),
                    MOVED,
                    CGPoint {
                        x: num(1),
                        y: num(2),
                    },
                    0,
                ));
            },
            Some("push") => {
                let (dx, dy, n) = (num(1), num(2), num(3) as usize);
                for _ in 0..n {
                    let p = here();
                    let target = CGPoint {
                        x: p.x + dx,
                        y: p.y + dy,
                    };
                    unsafe {
                        let e = CGEventCreateMouseEvent(std::ptr::null_mut(), MOVED, target, 0);
                        CGEventSetIntegerValueField(e, 4, dx as i64);
                        CGEventSetIntegerValueField(e, 5, dy as i64);
                        CGEventSetDoubleValueField(e, 4, dx);
                        CGEventSetDoubleValueField(e, 5, dy);
                        post(e);
                    }
                }
            }
            Some("click") => {
                let p = here();
                unsafe {
                    post(CGEventCreateMouseEvent(std::ptr::null_mut(), 1, p, 0));
                    post(CGEventCreateMouseEvent(std::ptr::null_mut(), 2, p, 0));
                }
            }
            Some("scroll") => unsafe {
                // kCGScrollEventUnitLine = 1
                post(CGEventCreateScrollWheelEvent(
                    std::ptr::null_mut(),
                    1,
                    1,
                    num(1) as i32,
                ));
            },
            Some("trackpad") => {
                let (points, n) = (num(1), num(2) as usize);
                // Fields: continuous, scroll phase, momentum phase, point delta (axis 1).
                let scroll = |dy: f64, phase: i64, momentum: i64| unsafe {
                    // kCGScrollEventUnitPixel = 0
                    let e = CGEventCreateScrollWheelEvent(std::ptr::null_mut(), 0, 1, dy as i32);
                    CGEventSetIntegerValueField(e, 88, 1);
                    CGEventSetIntegerValueField(e, 99, phase);
                    CGEventSetIntegerValueField(e, 123, momentum);
                    CGEventSetDoubleValueField(e, 96, dy);
                    post(e);
                };
                scroll(0.0, 1, 0);
                for _ in 0..n {
                    scroll(points, 2, 0);
                }
                scroll(0.0, 4, 0);
                scroll(points, 0, 1);
                for i in 1..5 {
                    scroll(points / (i + 1) as f64, 0, 2);
                }
                scroll(0.0, 0, 3);
            }
            Some("type") => {
                for c in args[1].chars() {
                    if let Some(code) = vk(c) {
                        key(code, 0);
                    }
                }
            }
            Some("key") => {
                let code = u16::from_str_radix(args[1].trim_start_matches("0x"), 16).unwrap();
                let mut flags = 0u64;
                let mut mods: Vec<u16> = vec![];
                for m in &args[2..] {
                    let (flag, vk) = match m.as_str() {
                        "cmd" => (0x0010_0008, 0x37),
                        "ctrl" => (0x0004_0001, 0x3B),
                        "opt" => (0x0008_0020, 0x3A),
                        "shift" => (0x0002_0002, 0x38),
                        _ => panic!("unknown modifier {m}"),
                    };
                    flags |= flag;
                    mods.push(vk);
                }
                // Modifiers arrive as flagsChanged events, then the key carries the flags.
                let mut held = 0u64;
                for (m, vk) in args[2..].iter().zip(&mods) {
                    held |= match m.as_str() {
                        "cmd" => 0x0010_0008,
                        "ctrl" => 0x0004_0001,
                        "opt" => 0x0008_0020,
                        _ => 0x0002_0002,
                    };
                    unsafe {
                        let e = CGEventCreateKeyboardEvent(std::ptr::null_mut(), *vk, true);
                        CGEventSetFlags(e, held);
                        post(e);
                    }
                }
                key(code, flags);
                for vk in mods.iter().rev() {
                    unsafe {
                        let e = CGEventCreateKeyboardEvent(std::ptr::null_mut(), *vk, false);
                        CGEventSetFlags(e, 0);
                        post(e);
                    }
                }
            }
            _ => eprintln!("see source for usage"),
        }
    }
}
