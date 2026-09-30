//! Spike: does the daemon's process tap send this Mac's sound (and mute it here while it
//! runs)? Drives the daemon's own module.
//!
//!   cargo run -p spike-mac-tap                # SECS=20 to tap for longer (default 10)
//!
//! Play something first. macOS asks for System Audio Recording permission (for Terminal,
//! since it runs from there). While tapped the Mac should go quiet and the levels below
//! should move; when it ends the sound comes back.

#[cfg(target_os = "macos")]
#[path = "../../../crates/mousetail/src/platform/macos_tap.rs"]
mod tap;

#[cfg(target_os = "macos")]
fn main() {
    use std::time::{Duration, Instant};

    let secs: u64 = std::env::var("SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(10);
    println!("supported: {}", tap::SystemSound::supported());
    let (sound, pcm) = match tap::SystemSound::start("spike", "Spike computer") {
        Ok(s) => s,
        Err(e) => {
            eprintln!("couldn't start: {e:#}");
            return;
        }
    };
    let start = Instant::now();
    let (mut samples, mut peak, mut chunks) = (0usize, 0i16, 0usize);
    let mut next = Duration::from_secs(1);
    while start.elapsed() < Duration::from_secs(secs) {
        if let Ok(chunk) = pcm.recv_timeout(Duration::from_millis(100)) {
            chunks += 1;
            samples += chunk.len();
            peak = chunk.iter().fold(peak, |p, s| p.max(s.saturating_abs()));
        }
        if start.elapsed() >= next {
            // 48 kHz stereo is 96,000 samples a second.
            println!(
                "{:>3}s  {samples:>6} samples in {chunks:>3} chunks, peak {peak:>5}",
                next.as_secs()
            );
            (samples, peak, chunks) = (0, 0, 0);
            next += Duration::from_secs(1);
        }
    }
    drop(sound);
    println!("stopped; the Mac's sound should be back");
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("macOS only");
}
