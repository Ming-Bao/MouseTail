//! Wire protocol. Reliable messages travel on one bidirectional QUIC stream as
//! length-prefixed postcard frames; pointer motion travels as unreliable datagrams.
//!
//! Keys and buttons use Linux evdev codes (`input-event-codes.h`) as the neutral format.
//! The sending side reports physical keys; the receiving side interprets them for its own
//! platform (e.g. what Command should mean on Linux).

use serde::{Deserialize, Serialize};

use crate::layout::Rect;

/// Staying compatible (see "Releases" in DESIGN.md): postcard isn't self-describing, so new
/// information goes in new `Message` and `Datagram` variants, added at the end. Older peers
/// skip what they can't decode. Never add fields to existing messages: an older peer doesn't
/// send them, and postcard can't fill in a missing one.
///
/// Anything else is a breaking change: bump `PROTOCOL_VERSION` and `MIN_PROTOCOL_VERSION`.
///
/// History: 5 sends each clipboard on a stream of its own (see `net::send_clipboard`) to peers
/// on 5 or later, so a big one doesn't hold up input; 4 sends it inline.
pub const PROTOCOL_VERSION: u32 = 5;
/// First protocol with clipboards on their own streams.
pub const CLIPBOARD_STREAMS: u32 = 5;
/// First protocol whose peers skip messages they don't know (4 hangs up), so newer messages
/// such as `SoundCaps` and `Media` can go to them.
pub const SKIPS_UNKNOWN: u32 = 5;
/// Oldest protocol this build still talks to. Newer peers are accepted: they only add things
/// we skip, and they check that we're new enough for them.
pub const MIN_PROTOCOL_VERSION: u32 = 4;
pub const ALPN: &[u8] = b"mousetail/1";
/// Largest control frame accepted (clipboard payloads included).
pub const MAX_FRAME: usize = 16 * 1024 * 1024;
/// Largest frame accepted before pairing (hellos and pairing messages are small), so anyone
/// on the network can't make us hold 16 MiB per connection.
pub const MAX_UNPAIRED_FRAME: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Platform {
    MacOs,
    Linux,
    Windows,
    Other,
}

impl Platform {
    pub fn current() -> Self {
        if cfg!(target_os = "macos") {
            Platform::MacOs
        } else if cfg!(target_os = "linux") {
            Platform::Linux
        } else if cfg!(target_os = "windows") {
            Platform::Windows
        } else {
            Platform::Other
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DisplayInfo {
    pub id: String,
    pub name: String,
    /// Machine-local logical coordinates (points).
    pub rect: Rect,
    pub scale: f64,
    pub primary: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Hello {
    pub protocol: u32,
    pub name: String,
    pub platform: Platform,
    pub displays: Vec<DisplayInfo>,
    /// Can capture local input and send it elsewhere.
    pub can_control: bool,
    /// Can inject input received from elsewhere.
    pub can_be_controlled: bool,
    /// Hardware addresses a Wake-on-LAN packet can wake this machine through.
    pub wake_macs: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Scroll {
    /// Wayland convention: positive scrolls the view right/down, in points.
    pub dx: f64,
    pub dy: f64,
    /// Whole wheel notches, for notched wheels; `None` for smooth (trackpad) scrolling.
    pub notches: Option<(i32, i32)>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Message {
    Hello(Hello),
    Displays(Vec<DisplayInfo>),
    /// The cursor has entered the receiver at this point (receiver-local coordinates).
    Enter {
        x: f64,
        y: f64,
    },
    /// The cursor has left; release anything still held.
    Leave,
    Button {
        code: u16,
        down: bool,
        x: f64,
        y: f64,
    },
    Scroll(Scroll),
    Key {
        code: u16,
        down: bool,
    },
    Clipboard {
        mime: String,
        data: Vec<u8>,
    },
    /// Ask the receiver to show a pairing code.
    PairRequest,
    /// SPAKE2 message.
    PairSpake(Vec<u8>),
    /// Key-confirmation tag.
    PairConfirm(Vec<u8>),
    PairFailed(String),
    /// "Play your sound through me" (true) or stop (false), to and from peers that predate
    /// `SoundCaps`; the other side answers by streaming `Datagram::Audio`.
    AudioWanted(bool),
    /// "In my arrangement, your displays' origin sits at (x, y)", so both computers can keep
    /// one arrangement. `updated` is when a person last chose it (Unix ms; 0 = automatic).
    Placement {
        x: f64,
        y: f64,
        updated: u64,
    },
    /// "I'm not paired with you (any more)": the receiver forgets the sender too, so unpairing
    /// on one computer unpairs both.
    NotPaired,
    /// A media control pressed on the computer playing this one's sound (AirPods, media keys)
    /// for whatever is playing here.
    Media(MediaKey),
    /// Which ways this machine can share sound, sent after `Hello`. A peer that never sends
    /// it predates sound going both ways: there a Mac only plays sound and Linux only sends it.
    SoundCaps {
        can_play: bool,
        can_share: bool,
    },
    /// "Play your sound through me" (true) or stop (false), from the computer the user is
    /// sitting at, to peers that sent `SoundCaps` (older ones get `AudioWanted`). `updated` is
    /// when the sender became the one that listens (Unix ms; 0 = by default), so if both think
    /// they are, the newer claim wins.
    SoundWanted {
        wanted: bool,
        updated: u64,
    },
    /// "The link between us is paused" (or resumed), so pausing on either computer pauses
    /// both. `updated` is when a person chose it (Unix ms); the newer choice wins. Peers that
    /// predate it just see us stop taking input (`Hello::can_be_controlled`).
    Paused {
        paused: bool,
        updated: u64,
    },
}

/// Media controls, as the headphones or keyboard sent them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MediaKey {
    PlayPause,
    Next,
    Previous,
}

/// Unreliable, unordered traffic: latest-wins pointer motion and audio packets.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Datagram {
    Motion(Motion),
    Audio(crate::audio::AudioPacket),
}

/// Unreliable pointer motion, receiver-local coordinates. Latest `seq` wins.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Motion {
    pub seq: u64,
    pub x: f64,
    pub y: f64,
}

pub fn encode<T: Serialize>(value: &T) -> Vec<u8> {
    postcard::to_stdvec(value).expect("postcard encoding cannot fail for these types")
}

pub fn decode<'a, T: Deserialize<'a>>(bytes: &'a [u8]) -> anyhow::Result<T> {
    Ok(postcard::from_bytes(bytes)?)
}

pub fn frame(msg: &Message) -> Vec<u8> {
    let body = encode(msg);
    let mut out = Vec::with_capacity(body.len() + 4);
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&body);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn datagrams_round_trip() {
        for d in [
            Datagram::Motion(Motion {
                seq: 7,
                x: 1.5,
                y: 900.0,
            }),
            Datagram::Audio(crate::audio::AudioPacket {
                stream: 3,
                seq: 9,
                data: vec![1, 2, 3],
            }),
        ] {
            assert_eq!(decode::<Datagram>(&encode(&d)).unwrap(), d);
        }
    }

    #[test]
    fn newer_messages_fail_on_their_own() {
        // A newer release adding a variant: older peers fail to decode just that frame (and
        // skip it), not the stream.
        let mut unknown = encode(&Message::Leave);
        unknown[0] = 100;
        assert!(decode::<Message>(&unknown).is_err());
    }

    #[test]
    fn frames_round_trip() {
        let msg = Message::Button {
            code: 0x110,
            down: true,
            x: 1.5,
            y: 2.0,
        };
        let framed = frame(&msg);
        let len = u32::from_le_bytes(framed[..4].try_into().unwrap()) as usize;
        assert_eq!(len, framed.len() - 4);
        assert_eq!(decode::<Message>(&framed[4..]).unwrap(), msg);
    }
}
