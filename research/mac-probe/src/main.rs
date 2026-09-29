//! Spike: what does the Mac side see? Displays in global coordinates, and whether this
//! process may tap and post input events.

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("macos only");
}

#[cfg(target_os = "macos")]
fn main() {
    mac::run();
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
