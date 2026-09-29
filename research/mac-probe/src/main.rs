//! Spike: what does the Mac side see? Displays in global coordinates, and whether this
//! process may tap and post input events.

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("macos only");
}

#[cfg(target_os = "macos")]
fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("keys") {
        mac::keys(args.get(2).and_then(|s| s.parse().ok()).unwrap_or(5));
    } else {
        mac::run();
    }
}

#[cfg(target_os = "macos")]
mod mac {
    use std::ffi::c_void;

    use core_graphics::display::CGDisplay;

    #[link(name = "ApplicationServices", kind = "framework")]
    unsafe extern "C" {
        fn AXIsProcessTrusted() -> bool;
        fn CGPreflightListenEventAccess() -> bool;
        fn CGPreflightPostEventAccess() -> bool;
        fn CGEventTapCreate(
            tap: u32,
            place: u32,
            options: u32,
            mask: u64,
            callback: extern "C" fn(*mut c_void, u32, *mut c_void, *mut c_void) -> *mut c_void,
            info: *mut c_void,
        ) -> *mut c_void;
        fn CFRelease(cf: *mut c_void);
    }

    extern "C" fn passthrough(
        _: *mut c_void,
        _: u32,
        event: *mut c_void,
        _: *mut c_void,
    ) -> *mut c_void {
        event
    }

    static SEEN: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

    extern "C" fn log_keys(
        _: *mut c_void,
        etype: u32,
        event: *mut c_void,
        _: *mut c_void,
    ) -> *mut c_void {
        unsafe extern "C" {
            fn CGEventGetIntegerValueField(event: *mut c_void, field: u32) -> i64;
        }
        if etype == 10 || etype == 11 {
            let vk = unsafe { CGEventGetIntegerValueField(event, 9) };
            let tag = unsafe { CGEventGetIntegerValueField(event, 42) };
            println!(
                "key {} vk=0x{vk:02x} {}",
                if etype == 10 { "down" } else { "up" },
                if tag == 0x4B494F52 {
                    "(posted by Kiore)"
                } else {
                    ""
                }
            );
            SEEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        event
    }

    /// `spike-mac-probe keys <seconds>`: print key events (listen-only) for a while.
    pub fn keys(seconds: u64) {
        unsafe extern "C" {
            fn CFMachPortCreateRunLoopSource(
                a: *const c_void,
                port: *mut c_void,
                order: isize,
            ) -> *mut c_void;
            fn CFRunLoopGetCurrent() -> *mut c_void;
            fn CFRunLoopAddSource(rl: *mut c_void, source: *mut c_void, mode: *const c_void);
            fn CFRunLoopRunInMode(mode: *const c_void, seconds: f64, ret: bool) -> i32;
            static kCFRunLoopDefaultMode: *const c_void;
        }
        unsafe {
            // listen-only (1), keyDown | keyUp
            let tap = CGEventTapCreate(
                1,
                0,
                1,
                (1 << 10) | (1 << 11),
                log_keys,
                std::ptr::null_mut(),
            );
            assert!(!tap.is_null(), "no event tap");
            let src = CFMachPortCreateRunLoopSource(std::ptr::null(), tap, 0);
            CFRunLoopAddSource(CFRunLoopGetCurrent(), src, kCFRunLoopDefaultMode);
            CFRunLoopRunInMode(kCFRunLoopDefaultMode, seconds as f64, false);
            println!(
                "{} key events",
                SEEN.load(std::sync::atomic::Ordering::Relaxed)
            );
        }
    }

    pub fn run() {
        println!("displays (global points, origin = top-left of main):");
        for id in CGDisplay::active_displays().unwrap() {
            let d = CGDisplay::new(id);
            let b = d.bounds();
            let px = d.display_mode().map(|m| m.pixel_width()).unwrap_or(0);
            let scale = if b.size.width > 0.0 {
                px as f64 / b.size.width
            } else {
                0.0
            };
            println!(
                "  id {id:<10} main={:<5} builtin={:<5} origin=({}, {}) size={}x{} scale={scale:.1}",
                d.is_main(),
                d.is_builtin(),
                b.origin.x,
                b.origin.y,
                b.size.width,
                b.size.height,
            );
        }

        unsafe {
            println!(
                "AXIsProcessTrusted (Accessibility): {}",
                AXIsProcessTrusted()
            );
            println!(
                "CGPreflightListenEventAccess (Input Monitoring): {}",
                CGPreflightListenEventAccess()
            );
            println!(
                "CGPreflightPostEventAccess: {}",
                CGPreflightPostEventAccess()
            );
            // kCGSessionEventTap=1, kCGHeadInsertEventTap=0, kCGEventTapOptionDefault=0 (active).
            let mask = (1u64 << 5) | (1u64 << 10); // mouseMoved | keyDown
            let tap = CGEventTapCreate(1, 0, 0, mask, passthrough, std::ptr::null_mut());
            println!("active event tap (can intercept input): {}", !tap.is_null());
            if !tap.is_null() {
                CFRelease(tap);
            }
        }
    }
}
