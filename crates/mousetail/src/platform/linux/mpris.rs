//! An MPRIS media player on the session bus while another computer's sound plays here, so
//! media keys, `playerctl` and the desktop's media controls reach that computer.
//!
//! The player only has its bus name while it's shown, so nothing lists a dead player when no
//! sound is coming in. Every change is announced, so desktops that follow the most recently
//! active player (Omarchy's shell, `playerctld`) pick it.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::Context;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tracing::{debug, warn};
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{ObjectPath, OwnedValue, Value};
use zbus::{Connection, interface};

use crate::platform::MediaCommand;

const BUS_NAME: &str = "org.mpris.MediaPlayer2.mousetail";
const PATH: &str = "/org/mpris/MediaPlayer2";

enum Update {
    Show { source: String, playing: bool },
    Clear,
}

pub struct NowPlaying {
    updates: UnboundedSender<Update>,
}

impl NowPlaying {
    pub fn start(commands: UnboundedSender<MediaCommand>) -> anyhow::Result<Self> {
        let handle = tokio::runtime::Handle::current();
        let player = Player {
            commands,
            source: String::new(),
            playing: false,
            track: 0,
        };
        // Connect on another thread so this one (a runtime worker) isn't asked to block on
        // the runtime; it only takes a moment.
        let connect = async {
            let builder = zbus::connection::Builder::session()?
                .serve_at(PATH, Root)?
                .serve_at(PATH, player)?;
            tokio::time::timeout(Duration::from_secs(2), builder.build())
                .await
                .context("the session bus didn't answer")?
                .context("connecting to the session bus")
        };
        let conn = std::thread::scope(|s| s.spawn(|| handle.block_on(connect)).join())
            .map_err(|_| anyhow::anyhow!("session bus thread died"))??;
        let (tx, rx) = mpsc::unbounded_channel();
        handle.spawn(run(conn, rx));
        Ok(Self { updates: tx })
    }

    /// Be the player for sound from `source`, playing or paused.
    pub fn show(&self, source: &str, playing: bool) {
        let _ = self.updates.send(Update::Show {
            source: source.to_string(),
            playing,
        });
    }

    /// Leave the bus, so media keys go back to this machine's own players.
    pub fn clear(&self) {
        let _ = self.updates.send(Update::Clear);
    }
}

async fn run(conn: Connection, mut updates: UnboundedReceiver<Update>) {
    let player = match conn.object_server().interface::<_, Player>(PATH).await {
        Ok(p) => p,
        Err(e) => {
            warn!("media controls: {e}");
            return;
        }
    };
    let mut shown = false;
    while let Some(update) = updates.recv().await {
        match update {
            Update::Show { source, playing } => {
                let mut p = player.get_mut().await;
                let new_source = p.source != source;
                if new_source {
                    p.source = source;
                    p.track += 1;
                }
                let changed = p.playing != playing;
                p.playing = playing;
                drop(p);
                let appeared = !shown;
                if appeared {
                    // Named only once the state is right, so the first look is accurate.
                    if let Err(e) = conn.request_name(BUS_NAME).await {
                        warn!("couldn't offer media controls: {e}");
                        continue;
                    }
                    shown = true;
                }
                let p = player.get().await;
                let emitter = player.signal_emitter();
                if new_source || appeared {
                    let _ = p.metadata_changed(emitter).await;
                }
                if changed || new_source || appeared {
                    let _ = p.playback_status_changed(emitter).await;
                }
            }
            Update::Clear if shown => {
                if let Err(e) = conn.release_name(BUS_NAME).await {
                    debug!("releasing the media player name: {e}");
                }
                shown = false;
            }
            Update::Clear => {}
        }
    }
}

/// `org.mpris.MediaPlayer2`: who we are. There's no window to raise or app to quit.
struct Root;

#[interface(name = "org.mpris.MediaPlayer2")]
impl Root {
    fn raise(&self) {}
    fn quit(&self) {}

    #[zbus(property)]
    fn can_quit(&self) -> bool {
        false
    }

    #[zbus(property)]
    fn can_raise(&self) -> bool {
        false
    }

    #[zbus(property)]
    fn has_track_list(&self) -> bool {
        false
    }

    #[zbus(property)]
    fn identity(&self) -> &str {
        "MouseTail"
    }

    #[zbus(property)]
    fn supported_uri_schemes(&self) -> Vec<String> {
        vec![]
    }

    #[zbus(property)]
    fn supported_mime_types(&self) -> Vec<String> {
        vec![]
    }
}

/// `org.mpris.MediaPlayer2.Player`: the other computer's sound, as one endless track named
/// after it. Controls go to that computer; seeking isn't possible.
struct Player {
    commands: UnboundedSender<MediaCommand>,
    source: String,
    playing: bool,
    /// Bumped per source, so each gets its own track id.
    track: u64,
}

impl Player {
    fn send(&self, command: MediaCommand) {
        let _ = self.commands.send(command);
    }
}

#[interface(name = "org.mpris.MediaPlayer2.Player")]
impl Player {
    fn play_pause(&self) {
        self.send(MediaCommand::PlayPause);
    }

    fn play(&self) {
        self.send(MediaCommand::Play);
    }

    fn pause(&self) {
        self.send(MediaCommand::Pause);
    }

    fn stop(&self) {
        self.send(MediaCommand::Pause);
    }

    fn next(&self) {
        self.send(MediaCommand::Next);
    }

    fn previous(&self) {
        self.send(MediaCommand::Previous);
    }

    fn seek(&self, _offset: i64) {}

    fn set_position(&self, _track_id: ObjectPath<'_>, _position: i64) {}

    fn open_uri(&self, _uri: &str) {}

    #[zbus(signal)]
    async fn seeked(emitter: &SignalEmitter<'_>, position: i64) -> zbus::Result<()>;

    #[zbus(property)]
    fn playback_status(&self) -> &str {
        if self.playing { "Playing" } else { "Paused" }
    }

    #[zbus(property)]
    fn metadata(&self) -> HashMap<String, OwnedValue> {
        metadata(&self.source, self.track)
    }

    #[zbus(property)]
    fn rate(&self) -> f64 {
        1.0
    }

    #[zbus(property)]
    fn set_rate(&self, _rate: f64) {}

    #[zbus(property)]
    fn minimum_rate(&self) -> f64 {
        1.0
    }

    #[zbus(property)]
    fn maximum_rate(&self) -> f64 {
        1.0
    }

    #[zbus(property)]
    fn volume(&self) -> f64 {
        1.0
    }

    #[zbus(property)]
    fn set_volume(&self, _volume: f64) {}

    #[zbus(property(emits_changed_signal = "false"))]
    fn position(&self) -> i64 {
        0
    }

    #[zbus(property)]
    fn can_go_next(&self) -> bool {
        true
    }

    #[zbus(property)]
    fn can_go_previous(&self) -> bool {
        true
    }

    #[zbus(property)]
    fn can_play(&self) -> bool {
        true
    }

    #[zbus(property)]
    fn can_pause(&self) -> bool {
        true
    }

    #[zbus(property)]
    fn can_seek(&self) -> bool {
        false
    }

    #[zbus(property(emits_changed_signal = "const"))]
    fn can_control(&self) -> bool {
        true
    }
}

fn metadata(source: &str, track: u64) -> HashMap<String, OwnedValue> {
    let track_id = format!("/org/mousetail/track/{track}");
    let entries = [
        (
            "mpris:trackid",
            Value::from(ObjectPath::from_string_unchecked(track_id)),
        ),
        ("xesam:title", Value::from(source)),
        ("xesam:artist", Value::from(vec!["MouseTail"])),
    ];
    entries
        .into_iter()
        .filter_map(|(k, v)| Some((k.to_string(), v.try_into().ok()?)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_names_the_source() {
        let m = metadata("Galen's MacBook", 3);
        let title: String = m["xesam:title"].clone().try_into().unwrap();
        assert_eq!(title, "Galen's MacBook");
        let id: zbus::zvariant::OwnedObjectPath = m["mpris:trackid"].clone().try_into().unwrap();
        assert_eq!(id.as_str(), "/org/mousetail/track/3");
        let artist: Vec<String> = m["xesam:artist"].clone().try_into().unwrap();
        assert_eq!(artist, ["MouseTail"]);
    }
}
