//! Spike: can a plain background binary (no app bundle, no window) become macOS's Now Playing
//! app and receive AirPods / media key play-pause? Drives the daemon's own module.
//!
//!   cargo run -p spike-mac-nowplaying         # STAGE=6 for 6 s stages
//!
//! Press an AirPod stem (or F8) and watch the log. It claims Now Playing as playing, then
//! paused after 20 s, then lets go after 40 s. Results in `research/RESULTS.md`.

#[cfg(target_os = "macos")]
#[path = "../../../crates/mousetail/src/platform/macos_media.rs"]
mod media;

#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaCommand {
    PlayPause,
    Play,
    Pause,
    Next,
    Previous,
}

#[cfg(target_os = "macos")]
fn main() {
    use std::time::{Duration, Instant};

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let now_playing = media::NowPlaying::start(tx).unwrap();
    std::thread::spawn(move || {
        let start = Instant::now();
        now_playing.show("Spike computer", true);
        println!("playing; press an AirPod or F8");
        let mut stage = 0;
        let stage_secs: u64 = std::env::var("STAGE")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(20);
        loop {
            while let Ok(c) = rx.try_recv() {
                println!("{:>5.1}s got {c:?}", start.elapsed().as_secs_f64());
            }
            let t = start.elapsed().as_secs() * 20 / stage_secs;
            if stage == 0 && t >= 20 {
                stage = 1;
                now_playing.show("Spike computer", false);
                println!("paused");
            } else if stage == 1 && t >= 40 {
                stage = 2;
                now_playing.clear();
                println!("cleared");
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    });
    media::run_main_loop();
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("macOS only");
}
