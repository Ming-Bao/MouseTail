//! Platform backends behind one small surface so the node stays platform-free.
//!
//! - `Capture`: owns the local keyboard and mouse and feeds the `Controller` (macOS today).
//! - `Emulator`: injects input received from a controller (Linux/Wayland today).
//!
//! Platforms without a backend get stubs whose `start` fails, which simply means that machine
//! can't take that role yet.

#[cfg(not(target_os = "linux"))]
use std::collections::HashSet;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub mod macos_audio;
#[cfg(target_os = "macos")]
pub use macos::{Capture, clipboard_get, clipboard_set, displays, keyboard_blocked, notify};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub mod linux_audio;
#[cfg(target_os = "linux")]
pub use linux::{
    Emulator, clipboard_get, clipboard_set, command_super_keys, displays, notify, on_enter,
    wake_macs,
};

#[cfg(not(target_os = "macos"))]
pub use stubs::Capture;

#[cfg(target_os = "macos")]
pub use macos_audio::Player as AudioPlayer;
#[cfg(not(target_os = "macos"))]
pub use stubs::AudioPlayer;

#[cfg(target_os = "linux")]
pub use linux_audio::VirtualSpeaker as AudioSource;
#[cfg(not(target_os = "linux"))]
pub use stubs::AudioSource;

#[cfg(not(target_os = "linux"))]
pub use stubs::Emulator;

/// Keys that Command should turn into Super on this machine (see `keys::CommandRemap`).
#[cfg(not(target_os = "linux"))]
pub fn command_super_keys() -> HashSet<u16> {
    kiore_core::keys::CommandRemap::default_super_keys()
}

/// Is the OS withholding keystrokes from us (e.g. macOS Secure Input)?
#[cfg(not(target_os = "macos"))]
pub fn keyboard_blocked() -> bool {
    false
}

/// Hardware addresses others can wake this machine through (none reported yet).
#[cfg(not(target_os = "linux"))]
pub fn wake_macs() -> Vec<String> {
    vec![]
}

/// Called when a controller's cursor arrives here.
#[cfg(not(target_os = "linux"))]
pub fn on_enter() {}

#[allow(dead_code)]
mod stubs {
    use std::sync::{Arc, Mutex};

    use kiore_core::controller::{Action, Controller};
    use kiore_core::layout::Rect;
    use kiore_core::proto::Scroll;
    use tokio::sync::mpsc::UnboundedSender;

    pub struct Capture;

    impl Capture {
        pub fn start(
            _controller: Arc<Mutex<Controller>>,
            _actions: UnboundedSender<Action>,
            _prompt: bool,
        ) -> anyhow::Result<Self> {
            anyhow::bail!("capturing input isn't supported on this platform yet")
        }

        pub fn supported() -> bool {
            false
        }

        pub fn apply(&self, _action: &Action) {}
    }

    /// Plays another machine's sound here.
    pub struct AudioPlayer;

    impl AudioPlayer {
        pub fn start() -> anyhow::Result<Self> {
            anyhow::bail!("playing shared sound isn't supported on this platform yet")
        }

        pub fn play(&self, _packet: kiore_core::audio::AudioPacket) {}
    }

    /// A virtual speaker whose sound is sent to another machine.
    pub struct AudioSource;

    impl AudioSource {
        pub fn start(
            _id: &str,
            _description: &str,
        ) -> anyhow::Result<(Self, std::sync::mpsc::Receiver<Vec<i16>>)> {
            anyhow::bail!("sharing sound isn't supported on this platform yet")
        }
    }

    pub struct Emulator;

    impl Emulator {
        pub fn start() -> anyhow::Result<Self> {
            anyhow::bail!("injecting input isn't supported on this platform yet")
        }

        pub fn set_bounds(&self, _bounds: Rect) {}
        pub fn motion(&self, _x: f64, _y: f64) {}
        pub fn button(&self, _code: u16, _down: bool) {}
        pub fn key(&self, _code: u16, _down: bool) {}
        pub fn scroll(&self, _scroll: Scroll) {}
        pub fn release_all(&self) {}
    }
}

/// A readable machine name: the one people see in Finder or their shell prompt.
pub fn machine_name() -> String {
    #[cfg(target_os = "macos")]
    if let Ok(out) = std::process::Command::new("scutil")
        .args(["--get", "ComputerName"])
        .output()
    {
        let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !name.is_empty() {
            return name;
        }
    }
    hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .map(|h| h.trim_end_matches(".local").to_string())
        .unwrap_or_else(|| "Kiore".into())
}
