//! Sound sharing: Opus in 10 ms packets, and a playout buffer that smooths network jitter.
//!
//! The sending machine exposes a virtual speaker; whatever plays into it is encoded and sent
//! as unreliable datagrams. The receiving machine decodes into a `Playout` buffer that the
//! audio device drains, keeping a small cushion (about 40 ms) so Wi-Fi jitter doesn't click.

use std::collections::VecDeque;
use std::ffi::c_int;

use serde::{Deserialize, Serialize};

use crate::proto::Platform;

pub const SAMPLE_RATE: u32 = 48_000;
pub const CHANNELS: usize = 2;
/// 10 ms per packet.
pub const FRAME: usize = 480;
const BITRATE: c_int = 160_000;
/// A generous upper bound for one encoded 10 ms packet.
pub const MAX_PACKET: usize = 1200;

/// Before either computer has been used to push the cursor onto the other, which one plays
/// the other's sound? A Mac (usually the one with the headphones) over anything else, else
/// the smaller id. Both sides reach the same answer.
pub fn listens_by_default(me: (&str, Platform), peer: (&str, Platform)) -> bool {
    match (me.1 == Platform::MacOs, peer.1 == Platform::MacOs) {
        (true, false) => true,
        (false, true) => false,
        _ => me.0 < peer.0,
    }
}

/// One encoded packet on the wire.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AudioPacket {
    /// Changes whenever the sender restarts its stream, so the receiver resets.
    pub stream: u32,
    pub seq: u32,
    pub data: Vec<u8>,
}

// ---------------------------------------------------------------------------------------------
// Opus

pub struct Encoder(*mut opusic_sys::OpusEncoder);

// The encoder is only ever used from one thread at a time.
unsafe impl Send for Encoder {}

impl Encoder {
    pub fn new() -> anyhow::Result<Self> {
        let mut err = 0;
        let e = unsafe {
            opusic_sys::opus_encoder_create(
                SAMPLE_RATE as i32,
                CHANNELS as c_int,
                opusic_sys::OPUS_APPLICATION_AUDIO,
                &mut err,
            )
        };
        anyhow::ensure!(
            !e.is_null() && err == opusic_sys::OPUS_OK,
            "opus encoder: {err}"
        );
        unsafe {
            opusic_sys::opus_encoder_ctl(e, opusic_sys::OPUS_SET_BITRATE_REQUEST, BITRATE);
            // Carry a little redundancy so a lost Wi-Fi packet can often be rebuilt.
            opusic_sys::opus_encoder_ctl(e, opusic_sys::OPUS_SET_INBAND_FEC_REQUEST, 1 as c_int);
            opusic_sys::opus_encoder_ctl(
                e,
                opusic_sys::OPUS_SET_PACKET_LOSS_PERC_REQUEST,
                5 as c_int,
            );
        }
        Ok(Self(e))
    }

    /// Encode one frame of interleaved stereo (`FRAME * CHANNELS` samples).
    pub fn encode(&mut self, pcm: &[i16]) -> anyhow::Result<Vec<u8>> {
        anyhow::ensure!(pcm.len() == FRAME * CHANNELS, "wrong frame size");
        let mut out = vec![0u8; MAX_PACKET];
        let n = unsafe {
            opusic_sys::opus_encode(
                self.0,
                pcm.as_ptr(),
                FRAME as c_int,
                out.as_mut_ptr(),
                out.len() as i32,
            )
        };
        anyhow::ensure!(n >= 0, "opus encode: {n}");
        out.truncate(n as usize);
        Ok(out)
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        unsafe { opusic_sys::opus_encoder_destroy(self.0) }
    }
}

pub struct Decoder(*mut opusic_sys::OpusDecoder);

unsafe impl Send for Decoder {}

impl Decoder {
    pub fn new() -> anyhow::Result<Self> {
        let mut err = 0;
        let d = unsafe {
            opusic_sys::opus_decoder_create(SAMPLE_RATE as i32, CHANNELS as c_int, &mut err)
        };
        anyhow::ensure!(
            !d.is_null() && err == opusic_sys::OPUS_OK,
            "opus decoder: {err}"
        );
        Ok(Self(d))
    }

    /// Decode one packet, or with `None` conceal a lost one. Returns interleaved stereo.
    pub fn decode(&mut self, packet: Option<&[u8]>) -> anyhow::Result<Vec<i16>> {
        self.decode_with(packet, false)
    }

    /// Rebuild the frame lost just before `next` from the redundancy `next` carries (the
    /// encoder's in-band FEC). Decode `next` itself afterwards as usual.
    pub fn recover(&mut self, next: &[u8]) -> anyhow::Result<Vec<i16>> {
        self.decode_with(Some(next), true)
    }

    fn decode_with(&mut self, packet: Option<&[u8]>, fec: bool) -> anyhow::Result<Vec<i16>> {
        let mut out = vec![0i16; FRAME * CHANNELS];
        let (ptr, len) = match packet {
            Some(p) => (p.as_ptr(), p.len() as i32),
            None => (std::ptr::null(), 0),
        };
        let n = unsafe {
            opusic_sys::opus_decode(
                self.0,
                ptr,
                len,
                out.as_mut_ptr(),
                FRAME as c_int,
                fec as c_int,
            )
        };
        anyhow::ensure!(n >= 0, "opus decode: {n}");
        out.truncate(n as usize * CHANNELS);
        Ok(out)
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        unsafe { opusic_sys::opus_decoder_destroy(self.0) }
    }
}

// ---------------------------------------------------------------------------------------------
// Receiving

/// Turns arriving packets into a continuous stream: conceals short gaps, resets on long ones
/// or a new stream, and drops late packets.
pub struct Receiver {
    decoder: Decoder,
    stream: Option<u32>,
    next_seq: u32,
}

/// More missing packets than this is a pause, not loss: start afresh instead of concealing.
const MAX_CONCEAL: u32 = 5;

impl Receiver {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self {
            decoder: Decoder::new()?,
            stream: None,
            next_seq: 0,
        })
    }

    /// Decode a packet into samples for the playout buffer. `reset` is true when the stream
    /// restarted (the buffer should drop what it holds and re-buffer).
    pub fn receive(&mut self, p: &AudioPacket) -> (Vec<i16>, bool) {
        // How far ahead of the expected packet this one is; negative is late. Signed, so the
        // count carries on through wrapping round.
        let ahead = p.seq.wrapping_sub(self.next_seq) as i32;
        let fresh = self.stream != Some(p.stream) || ahead > MAX_CONCEAL as i32;
        if !fresh && ahead < 0 {
            return (vec![], false); // late or duplicate
        }
        let mut out = vec![];
        if fresh {
            if let Ok(d) = Decoder::new() {
                self.decoder = d;
            }
            self.stream = Some(p.stream);
        } else if ahead > 0 {
            // Guess the older missing frames; the last one this packet can usually rebuild.
            for _ in 1..ahead {
                out.extend(self.decoder.decode(None).unwrap_or_default());
            }
            out.extend(self.decoder.recover(&p.data).unwrap_or_default());
        }
        out.extend(self.decoder.decode(Some(&p.data)).unwrap_or_default());
        self.next_seq = p.seq.wrapping_add(1);
        (out, fresh)
    }
}

/// The receiving side's cushion between the network and the sound card.
///
/// Fills with 48 kHz stereo; the device pulls at its own rate and channel count. Starts
/// playing once `target` is banked, goes quiet and re-buffers on underrun, and skips ahead if
/// it falls more than `max` behind (clock drift, a burst after a stall).
pub struct Playout {
    samples: VecDeque<i16>,
    playing: bool,
    target: usize,
    max: usize,
    /// Fractional read position for rate conversion, in 48 kHz frames.
    phase: f64,
}

impl Default for Playout {
    fn default() -> Self {
        Self::new(40, 120)
    }
}

impl Playout {
    pub fn new(target_ms: usize, max_ms: usize) -> Self {
        let per_ms = SAMPLE_RATE as usize / 1000 * CHANNELS;
        Self {
            samples: VecDeque::new(),
            playing: false,
            target: target_ms * per_ms,
            max: max_ms.max(target_ms) * per_ms,
            phase: 0.0,
        }
    }

    pub fn reset(&mut self) {
        self.samples.clear();
        self.playing = false;
        self.phase = 0.0;
    }

    pub fn push(&mut self, pcm: &[i16]) {
        self.samples.extend(pcm);
        if self.samples.len() > self.max {
            let excess = self.samples.len() - self.target;
            self.samples.drain(..excess - excess % CHANNELS);
        }
    }

    /// Buffered audio, in milliseconds.
    pub fn level_ms(&self) -> usize {
        self.samples.len() / (SAMPLE_RATE as usize / 1000 * CHANNELS)
    }

    /// Fill `out` (interleaved, `channels` per frame, at `rate` Hz) with audio or silence.
    pub fn pull(&mut self, out: &mut [f32], channels: usize, rate: u32) {
        if !self.playing && self.samples.len() >= self.target {
            self.playing = true;
        }
        let step = SAMPLE_RATE as f64 / rate.max(1) as f64;
        for frame in out.chunks_mut(channels.max(1)) {
            let available = self.samples.len() / CHANNELS;
            if !self.playing || available < 2 {
                self.playing = false;
                frame.fill(0.0);
                continue;
            }
            // Linear interpolation between the two nearest 48 kHz frames.
            let i = self.phase as usize;
            let t = (self.phase - i as f64) as f32;
            let at = |f: usize, c: usize| self.samples[f * CHANNELS + c] as f32 / 32768.0;
            let l = at(i, 0) * (1.0 - t) + at(i + 1, 0) * t;
            let r = at(i, 1) * (1.0 - t) + at(i + 1, 1) * t;
            match frame.len() {
                1 => frame[0] = (l + r) / 2.0,
                _ => {
                    frame[0] = l;
                    frame[1] = r;
                    frame[2..].fill(0.0);
                }
            }
            self.phase += step;
            let consumed = self.phase as usize;
            if consumed > 0 {
                let n = (consumed * CHANNELS).min(self.samples.len());
                self.samples.drain(..n);
                self.phase -= consumed as f64;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(frames: usize, offset: usize) -> Vec<i16> {
        (0..frames)
            .flat_map(|i| {
                let v = ((((i + offset) as f64) * 440.0 * std::f64::consts::TAU / 48_000.0).sin()
                    * 12_000.0) as i16;
                [v, v]
            })
            .collect()
    }

    #[test]
    fn exactly_one_side_listens_by_default() {
        use Platform::*;
        for (a, b) in [
            (MacOs, Linux),
            (Linux, MacOs),
            (MacOs, MacOs),
            (Linux, Linux),
        ] {
            for (x, y) in [("aa", "bb"), ("bb", "aa")] {
                assert_ne!(
                    listens_by_default((x, a), (y, b)),
                    listens_by_default((y, b), (x, a))
                );
            }
        }
        assert!(listens_by_default(("zz", MacOs), ("aa", Linux)));
    }

    #[test]
    fn opus_round_trip_keeps_the_sound() {
        let mut enc = Encoder::new().unwrap();
        let mut dec = Decoder::new().unwrap();
        let mut energy = 0.0;
        for n in 0..20 {
            let pcm = tone(FRAME, n * FRAME);
            let packet = enc.encode(&pcm).unwrap();
            assert!(packet.len() < MAX_PACKET);
            let out = dec.decode(Some(&packet)).unwrap();
            assert_eq!(out.len(), FRAME * CHANNELS);
            if n > 5 {
                energy += out.iter().map(|s| (*s as f64).powi(2)).sum::<f64>();
            }
        }
        assert!(energy > 1e9, "decoded audio is silent");
    }

    #[test]
    fn receiver_conceals_gaps_drops_late_and_resets_on_new_stream() {
        let mut enc = Encoder::new().unwrap();
        let mut rx = Receiver::new().unwrap();
        let packet = |enc: &mut Encoder, stream, seq| AudioPacket {
            stream,
            seq,
            data: enc.encode(&tone(FRAME, 0)).unwrap(),
        };
        let (out, reset) = rx.receive(&packet(&mut enc, 1, 0));
        assert!(reset);
        assert_eq!(out.len(), FRAME * 2);
        // Packet 1 lost: packet 2 arrives with one concealed frame in front.
        let (out, reset) = rx.receive(&packet(&mut enc, 1, 2));
        assert!(!reset);
        assert_eq!(out.len(), 2 * FRAME * 2);
        // Packet 1 turns up late: ignored.
        assert_eq!(rx.receive(&packet(&mut enc, 1, 1)).0.len(), 0);
        // A long pause: start afresh rather than conceal a second of noise.
        let (out, reset) = rx.receive(&packet(&mut enc, 1, 500));
        assert!(reset);
        assert_eq!(out.len(), FRAME * 2);
        // Sender restarted.
        assert!(rx.receive(&packet(&mut enc, 2, 0)).1);
    }

    #[test]
    fn receiver_counts_on_through_wrapping_round() {
        let mut enc = Encoder::new().unwrap();
        let mut rx = Receiver::new().unwrap();
        let mut packet = |seq| AudioPacket {
            stream: 1,
            seq,
            data: enc.encode(&tone(FRAME, 0)).unwrap(),
        };
        assert!(rx.receive(&packet(u32::MAX - 1)).1);
        let (out, reset) = rx.receive(&packet(u32::MAX));
        assert!(!reset && out.len() == FRAME * 2);
        // 0 lost across the wrap: 1 brings it back (rebuilt) plus itself.
        let (out, reset) = rx.receive(&packet(1));
        assert!(!reset);
        assert_eq!(out.len(), 2 * FRAME * 2);
        // And something from before the wrap is late, not a new stream.
        assert_eq!(rx.receive(&packet(u32::MAX)), (vec![], false));
    }

    #[test]
    fn playout_waits_for_its_cushion_then_plays() {
        let mut p = Playout::new(40, 120);
        let mut out = vec![1.0f32; 480 * 2];
        p.push(&tone(960, 0)); // 20 ms: not enough yet
        p.pull(&mut out, 2, 48_000);
        assert!(out.iter().all(|s| *s == 0.0));
        p.push(&tone(1440, 960)); // now 50 ms
        p.pull(&mut out, 2, 48_000);
        assert!(out.iter().any(|s| *s != 0.0));
        assert_eq!(p.level_ms(), 40);
    }

    #[test]
    fn playout_skips_ahead_when_too_far_behind() {
        let mut p = Playout::new(40, 120);
        p.push(&tone(48_000 * 200 / 1000, 0)); // 200 ms arrives in a burst
        assert_eq!(p.level_ms(), 40);
    }

    #[test]
    fn playout_resamples_and_maps_channels() {
        let mut p = Playout::new(10, 120);
        p.push(&tone(4800, 0)); // 100 ms at 48 kHz
        let mut out = vec![0.0f32; 441 * 6]; // 10 ms at 44.1 kHz, 6 channels
        p.pull(&mut out, 6, 44_100);
        assert!(out.chunks(6).any(|f| f[0] != 0.0));
        assert!(out.chunks(6).all(|f| f[2..].iter().all(|s| *s == 0.0)));
        // 10 ms of output consumed ~10 ms of input.
        assert_eq!(p.level_ms(), 90);
    }
}
