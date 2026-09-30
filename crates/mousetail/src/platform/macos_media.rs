//! Makes MouseTail the Mac's "Now Playing" app while another computer's sound is playing
//! here, so AirPods presses, media keys and Control Centre's buttons reach that computer
//! rather than opening Music.
//!
//! macOS only delivers these commands on the main thread, so the daemon keeps its main
//! thread for the run loop (`run_main_loop`) and everything here hops onto the main queue.

use std::ptr::NonNull;

use block2::RcBlock;
use core_foundation::date::CFAbsoluteTimeGetCurrent;
use core_foundation::runloop::{
    CFRunLoop, CFRunLoopTimer, CFRunLoopTimerRef, kCFRunLoopDefaultMode,
};
use dispatch2::DispatchQueue;
use objc2::runtime::AnyObject;
use objc2_foundation::{NSDictionary, NSNumber, NSString};
use objc2_media_player::{
    MPMediaItemPropertyArtist, MPMediaItemPropertyTitle, MPNowPlayingInfoCenter,
    MPNowPlayingInfoPropertyPlaybackRate, MPNowPlayingPlaybackState, MPRemoteCommand,
    MPRemoteCommandCenter, MPRemoteCommandEvent, MPRemoteCommandHandlerStatus,
};
use tokio::sync::mpsc::UnboundedSender;

use super::MediaCommand;

/// Run the main thread's run loop forever (commands arrive through it).
pub fn run_main_loop() -> ! {
    // A run loop with nothing in it returns at once; a far-off timer keeps it going.
    extern "C" fn never(_: CFRunLoopTimerRef, _: *mut std::ffi::c_void) {}
    let far = 1.0e10;
    let timer = CFRunLoopTimer::new(
        unsafe { CFAbsoluteTimeGetCurrent() } + far,
        far,
        0,
        0,
        never,
        std::ptr::null_mut(),
    );
    let run_loop = CFRunLoop::get_current();
    run_loop.add_timer(&timer, unsafe { kCFRunLoopDefaultMode });
    loop {
        CFRunLoop::run_current();
    }
}

pub struct NowPlaying;

impl NowPlaying {
    pub fn start(commands: UnboundedSender<MediaCommand>) -> anyhow::Result<Self> {
        DispatchQueue::main().exec_async(move || unsafe {
            let center = MPRemoteCommandCenter::sharedCommandCenter();
            for (command, what) in [
                (center.togglePlayPauseCommand(), MediaCommand::PlayPause),
                (center.playCommand(), MediaCommand::Play),
                (center.pauseCommand(), MediaCommand::Pause),
                (center.nextTrackCommand(), MediaCommand::Next),
                (center.previousTrackCommand(), MediaCommand::Previous),
            ] {
                let commands = commands.clone();
                let handler = RcBlock::new(move |_: NonNull<MPRemoteCommandEvent>| {
                    let _ = commands.send(what);
                    MPRemoteCommandHandlerStatus::Success
                });
                command.setEnabled(false);
                command.addTargetWithHandler(&handler);
            }
        });
        Ok(Self)
    }

    /// Claim Now Playing for sound from `source`, playing or paused.
    pub fn show(&self, source: &str, playing: bool) {
        let source = source.to_string();
        DispatchQueue::main().exec_async(move || unsafe {
            set_enabled(true);
            let title = NSString::from_str(&source);
            let artist = NSString::from_str("MouseTail");
            let rate = NSNumber::new_f64(if playing { 1.0 } else { 0.0 });
            let info = NSDictionary::<NSString, AnyObject>::from_slices(
                &[
                    MPMediaItemPropertyTitle,
                    MPMediaItemPropertyArtist,
                    MPNowPlayingInfoPropertyPlaybackRate,
                ],
                &[&**title, &**artist, &**rate],
            );
            let center = MPNowPlayingInfoCenter::defaultCenter();
            center.setNowPlayingInfo(Some(&info));
            center.setPlaybackState(if playing {
                MPNowPlayingPlaybackState::Playing
            } else {
                MPNowPlayingPlaybackState::Paused
            });
        });
    }

    /// Let go of Now Playing, so the controls go back to the Mac's own apps.
    pub fn clear(&self) {
        DispatchQueue::main().exec_async(|| unsafe {
            let center = MPNowPlayingInfoCenter::defaultCenter();
            center.setNowPlayingInfo(None);
            center.setPlaybackState(MPNowPlayingPlaybackState::Stopped);
            set_enabled(false);
        });
    }
}

unsafe fn set_enabled(on: bool) {
    let center = unsafe { MPRemoteCommandCenter::sharedCommandCenter() };
    let commands: [objc2::rc::Retained<MPRemoteCommand>; 5] = unsafe {
        [
            center.togglePlayPauseCommand(),
            center.playCommand(),
            center.pauseCommand(),
            center.nextTrackCommand(),
            center.previousTrackCommand(),
        ]
    };
    for c in commands {
        unsafe { c.setEnabled(on) };
    }
}
