//! Spike: can an unprivileged Wayland client drive the Hyprland pointer (absolute) and
//! keyboard, without uinput/root?
//!
//! Uses `zwlr_virtual_pointer_v1` for absolute motion and `zwp_virtual_keyboard_v1` for
//! keys, borrowing the seat's current keymap so the user's layout applies.
//!
//! Usage:
//!   spike-linux-inject pointer            move to test points, verify via `hyprctl cursorpos`
//!   spike-linux-inject type <text> [--no-enter|--erase]
//!                                         type ascii + Enter, type without Enter, or
//!                                         press Backspace once per char instead of typing
//!   spike-linux-inject latency            time 1000 absolute moves (client-side send cost)
//!   spike-linux-inject nudge              one tiny move (idle-reset test)
//!   spike-linux-inject push dx dy n       n relative moves (like sliding a real mouse)

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("linux only");
}

#[cfg(target_os = "linux")]
fn main() {
    linux::run();
}

#[cfg(target_os = "linux")]
mod linux {
    use std::os::fd::{AsFd, OwnedFd};
    use std::process::Command;
    use std::thread::sleep;
    use std::time::{Duration, Instant};

    use wayland_client::protocol::{wl_keyboard, wl_registry, wl_seat};
    use wayland_client::{Connection, Dispatch, QueueHandle, WEnum, delegate_noop};
    use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
        zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1,
        zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1,
    };
    use wayland_protocols_wlr::virtual_pointer::v1::client::{
        zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1,
        zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1,
    };

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

    fn cursorpos() -> String {
        let out = Command::new("hyprctl")
            .arg("cursorpos")
            .output()
            .expect("hyprctl");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn evdev_key(c: char) -> Option<(u32, bool)> {
        const ROW: &[(char, u32)] = &[
            ('1', 2),
            ('2', 3),
            ('3', 4),
            ('4', 5),
            ('5', 6),
            ('6', 7),
            ('7', 8),
            ('8', 9),
            ('9', 10),
            ('0', 11),
            ('-', 12),
            ('q', 16),
            ('w', 17),
            ('e', 18),
            ('r', 19),
            ('t', 20),
            ('y', 21),
            ('u', 22),
            ('i', 23),
            ('o', 24),
            ('p', 25),
            ('a', 30),
            ('s', 31),
            ('d', 32),
            ('f', 33),
            ('g', 34),
            ('h', 35),
            ('j', 36),
            ('k', 37),
            ('l', 38),
            ('z', 44),
            ('x', 45),
            ('c', 46),
            ('v', 47),
            ('b', 48),
            ('n', 49),
            ('m', 50),
            ('.', 52),
            (' ', 57),
        ];
        let lower = c.to_ascii_lowercase();
        ROW.iter()
            .find(|(ch, _)| *ch == lower)
            .map(|(_, code)| (*code, c.is_ascii_uppercase()))
    }

    pub fn run() {
        let args: Vec<String> = std::env::args().skip(1).collect();
        let mode = args.first().map(String::as_str).unwrap_or("pointer");

        let conn = Connection::connect_to_env().expect("connect to wayland");
        let mut queue = conn.new_event_queue();
        let qh = queue.handle();
        conn.display().get_registry(&qh, ());
        let mut state = State::default();
        queue.roundtrip(&mut state).unwrap();

        let seat = state.seat.clone().expect("no wl_seat");
        let t0 = Instant::now();
        let now = || t0.elapsed().as_millis() as u32;

        match mode {
            "pointer" | "latency" | "nudge" | "push" => {
                let vpm = state
                    .vpm
                    .clone()
                    .expect("compositor lacks zwlr_virtual_pointer_manager_v1");
                let vp = vpm.create_virtual_pointer(Some(&seat), &qh, ());
                queue.roundtrip(&mut state).unwrap();
                // Extent matches the whole layout; the compositor maps [0,extent] across all outputs.
                let (w, h) = (1920u32, 1080u32);
                if mode == "push" {
                    // Relative motion, like sliding a real mouse: push <dx> <dy> <count>.
                    let n =
                        |i: usize, d: f64| args.get(i).and_then(|a| a.parse().ok()).unwrap_or(d);
                    let (dx, dy, count) = (n(1, 10.0), n(2, 0.0), n(3, 10.0) as usize);
                    for _ in 0..count {
                        vp.motion(now(), dx, dy);
                        vp.frame();
                        queue.roundtrip(&mut state).unwrap();
                        sleep(Duration::from_millis(12));
                    }
                    println!("cursorpos after push: {}", cursorpos());
                    return;
                }
                if mode == "nudge" {
                    vp.motion_absolute(now(), 961, 541, w, h);
                    vp.frame();
                    vp.motion_absolute(now(), 960, 540, w, h);
                    vp.frame();
                    queue.roundtrip(&mut state).unwrap();
                    return;
                }
                if mode == "latency" {
                    let start = Instant::now();
                    let mut stalls = 0;
                    for i in 0..1000u32 {
                        vp.motion_absolute(now(), (i * 7) % w, (i * 3) % h, w, h);
                        vp.frame();
                        // A burst can fill the socket; a real sender must wait rather than drop.
                        while conn.flush().is_err() {
                            stalls += 1;
                            sleep(Duration::from_micros(200));
                        }
                    }
                    queue.roundtrip(&mut state).unwrap();
                    println!(
                        "1000 moves + roundtrip: {:?} ({stalls} socket stalls)",
                        start.elapsed()
                    );
                    println!("final cursorpos: {}", cursorpos());
                    return;
                }
                let before = cursorpos();
                println!("start cursorpos: {before}");
                let mut ok = true;
                for (x, y) in [(100, 100), (960, 540), (1919, 1079), (0, 0), (1500, 200)] {
                    vp.motion_absolute(now(), x, y, w, h);
                    vp.frame();
                    queue.roundtrip(&mut state).unwrap();
                    sleep(Duration::from_millis(60));
                    let got = cursorpos();
                    let want = format!("{x}, {y}");
                    let pass = got == want;
                    ok &= pass;
                    println!(
                        "  want {want:>10}  got {got:>10}  {}",
                        if pass { "ok" } else { "MISMATCH" }
                    );
                }
                // Scroll: vertical axis, 3 notches worth.
                use wayland_client::protocol::wl_pointer::Axis;
                vp.axis(now(), Axis::VerticalScroll, 15.0);
                vp.frame();
                queue.roundtrip(&mut state).unwrap();
                println!("pointer absolute: {}", if ok { "PASS" } else { "FAIL" });
            }
            "key" => {
                // key <evdev code>: press and release one key.
                let code: u32 = args.get(1).and_then(|a| a.parse().ok()).unwrap_or(190);
                let vkm = state.vkm.clone().expect("no virtual keyboard");
                let kb = seat.get_keyboard(&qh, ());
                queue.roundtrip(&mut state).unwrap();
                let (format, fd, size) = state.keymap.take().expect("no keymap");
                let vk = vkm.create_virtual_keyboard(&seat, &qh, ());
                vk.keymap(format, fd.as_fd(), size);
                queue.roundtrip(&mut state).unwrap();
                vk.key(now(), code, 1);
                vk.key(now() + 30, code, 0);
                queue.roundtrip(&mut state).unwrap();
                kb.release();
                println!("pressed evdev key {code}");
            }
            "type" => {
                let text = args.get(1).cloned().unwrap_or_else(|| "kiore ok".into());
                let vkm = state
                    .vkm
                    .clone()
                    .expect("compositor lacks zwp_virtual_keyboard_manager_v1");
                let kb = seat.get_keyboard(&qh, ());
                queue.roundtrip(&mut state).unwrap();
                let (format, fd, size) = state.keymap.take().expect("no keymap from seat");
                let vk = vkm.create_virtual_keyboard(&seat, &qh, ());
                vk.keymap(format, fd.as_fd(), size);
                queue.roundtrip(&mut state).unwrap();
                const SHIFT: u32 = 42;
                const ENTER: u32 = 28;
                let tap = |code: u32| {
                    vk.key(now(), code, 1);
                    vk.key(now(), code, 0);
                };
                let erase_only = args.get(2).map(String::as_str) == Some("--erase");
                for c in text.chars().filter(|_| !erase_only) {
                    let Some((code, shift)) = evdev_key(c) else {
                        continue;
                    };
                    if shift {
                        vk.key(now(), SHIFT, 1);
                        vk.modifiers(1, 0, 0, 0);
                    }
                    tap(code);
                    if shift {
                        vk.key(now(), SHIFT, 0);
                        vk.modifiers(0, 0, 0, 0);
                    }
                    conn.flush().unwrap();
                    sleep(Duration::from_millis(5));
                }
                const BACKSPACE: u32 = 14;
                match args.get(2).map(String::as_str) {
                    Some("--no-enter") => {}
                    Some("--erase") => {
                        for _ in text.chars() {
                            tap(BACKSPACE);
                            conn.flush().unwrap();
                            sleep(Duration::from_millis(5));
                        }
                    }
                    _ => tap(ENTER),
                }
                queue.roundtrip(&mut state).unwrap();
                kb.release();
                println!(
                    "typed {text:?} {}",
                    args.get(2).map(String::as_str).unwrap_or("+ Enter")
                );
            }
            other => eprintln!("unknown mode {other}"),
        }
    }
}
