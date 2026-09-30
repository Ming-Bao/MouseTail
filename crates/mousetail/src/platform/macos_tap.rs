//! Sends this Mac's sound to another computer: a Core Audio process tap on everything the
//! Mac plays (except MouseTail itself), muted here while it's tapped. macOS 14.2 or later.
//!
//! The tap sits alone in a private aggregate device, whose IO proc turns the tap's float
//! samples into 48 kHz stereo i16 for the channel. MouseTail is left out of the tap because
//! the daemon also plays other computers' sound here, which mustn't be sent back.
//!
//! Tapping needs the "System Audio Recording" permission. The daemon is started by the app,
//! so macOS asks on the app's behalf the first time.

use std::ffi::{CStr, c_void};
use std::mem::{MaybeUninit, size_of};
use std::ptr::{self, NonNull};
use std::sync::mpsc;

use core_foundation::array::CFArray;
use core_foundation::base::TCFType;
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::CFDictionary;
use core_foundation::string::{CFString, CFStringRef};
use mousetail_core::audio::SAMPLE_RATE;
use objc2::AnyThread;
use objc2_core_audio::{
    AudioDeviceCreateIOProcID, AudioDeviceDestroyIOProcID, AudioDeviceIOProcID, AudioDeviceStart,
    AudioDeviceStop, AudioHardwareCreateAggregateDevice, AudioHardwareCreateProcessTap,
    AudioHardwareDestroyAggregateDevice, AudioHardwareDestroyProcessTap,
    AudioObjectGetPropertyData, AudioObjectID, AudioObjectPropertyAddress,
    AudioObjectPropertySelector, CATapDescription, CATapMuteBehavior,
    kAudioAggregateDeviceIsPrivateKey, kAudioAggregateDeviceNameKey,
    kAudioAggregateDeviceTapAutoStartKey, kAudioAggregateDeviceTapListKey,
    kAudioAggregateDeviceUIDKey, kAudioHardwarePropertyTranslatePIDToProcessObject,
    kAudioObjectPropertyElementMain, kAudioObjectPropertyScopeGlobal, kAudioObjectSystemObject,
    kAudioSubTapDriftCompensationKey, kAudioSubTapUIDKey, kAudioTapPropertyFormat,
    kAudioTapPropertyUID,
};
use objc2_core_audio_types::{
    AudioBuffer, AudioBufferList, AudioStreamBasicDescription, AudioTimeStamp,
    kAudioFormatFlagIsFloat, kAudioFormatLinearPCM,
};
use objc2_foundation::{NSArray, NSNumber, NSString};

type OSStatus = i32;

pub struct SystemSound {
    tap: AudioObjectID,
    device: AudioObjectID,
    io_proc: AudioDeviceIOProcID,
    capture: *mut Capture,
}

// The capture state is only touched by the IO proc until it's stopped, then freed in drop.
unsafe impl Send for SystemSound {}

impl SystemSound {
    pub fn supported() -> bool {
        objc2::available!(macos = 14.2)
    }

    /// Start tapping. 48 kHz interleaved stereo PCM arrives on the returned channel; dropping
    /// the `SystemSound` stops the tap and unmutes the Mac.
    pub fn start(id: &str, description: &str) -> anyhow::Result<(Self, mpsc::Receiver<Vec<i16>>)> {
        anyhow::ensure!(
            Self::supported(),
            "sending this Mac's sound needs macOS 14.2 or later"
        );
        // Filled in as we go, so an early return tears down whatever was made.
        let mut sound = Self {
            tap: 0,
            device: 0,
            io_proc: None,
            capture: ptr::null_mut(),
        };

        unsafe {
            let exclude: Vec<_> = own_process_object()
                .map(NSNumber::new_u32)
                .into_iter()
                .collect();
            let tap = CATapDescription::initStereoGlobalTapButExcludeProcesses(
                CATapDescription::alloc(),
                &NSArray::from_retained_slice(&exclude),
            );
            tap.setName(&NSString::from_str(&format!("MouseTail to {description}")));
            tap.setPrivate(true);
            tap.setMuteBehavior(CATapMuteBehavior::MutedWhenTapped);
            check(
                AudioHardwareCreateProcessTap(Some(&tap), &mut sound.tap),
                "couldn't tap this Mac's sound (is MouseTail allowed under System Audio \
                 Recording in System Settings > Privacy & Security?)",
            )?;
        }

        let tap_uid: CFStringRef = unsafe { property(sound.tap, kAudioTapPropertyUID, None)? };
        anyhow::ensure!(!tap_uid.is_null(), "the sound tap has no UID");
        let tap_uid = unsafe { CFString::wrap_under_create_rule(tap_uid) };
        let format: AudioStreamBasicDescription =
            unsafe { property(sound.tap, kAudioTapPropertyFormat, None)? };
        anyhow::ensure!(
            format.mFormatID == kAudioFormatLinearPCM
                && format.mFormatFlags & kAudioFormatFlagIsFloat != 0
                && format.mBitsPerChannel == 32,
            "the sound tap isn't 32-bit float PCM"
        );
        tracing::debug!(
            "tapping sound: {} Hz, {} channels",
            format.mSampleRate,
            format.mChannelsPerFrame
        );

        // An aggregate device with nothing in it but the tap, which also clocks it.
        let key = |k: &CStr| CFString::new(k.to_str().unwrap_or_default());
        let tap_list = CFArray::from_CFTypes(&[CFDictionary::from_CFType_pairs(&[
            (key(kAudioSubTapUIDKey), tap_uid.as_CFType()),
            (
                key(kAudioSubTapDriftCompensationKey),
                CFBoolean::true_value().as_CFType(),
            ),
        ])]);
        let device = CFDictionary::from_CFType_pairs(&[
            (
                key(kAudioAggregateDeviceNameKey),
                CFString::new(&format!("MouseTail to {description}")).as_CFType(),
            ),
            (
                key(kAudioAggregateDeviceUIDKey),
                CFString::new(&format!("mousetail.{id}.{tap_uid}")).as_CFType(),
            ),
            (
                key(kAudioAggregateDeviceIsPrivateKey),
                CFBoolean::true_value().as_CFType(),
            ),
            (
                key(kAudioAggregateDeviceTapAutoStartKey),
                CFBoolean::true_value().as_CFType(),
            ),
            (key(kAudioAggregateDeviceTapListKey), tap_list.as_CFType()),
        ]);
        unsafe {
            // core-foundation's and objc2's CFDictionary are the same object.
            let device =
                &*(device.as_concrete_TypeRef() as *const objc2_core_foundation::CFDictionary);
            check(
                AudioHardwareCreateAggregateDevice(device, NonNull::from(&mut sound.device)),
                "couldn't create the device for this Mac's sound",
            )?;
        }

        let (tx, rx) = mpsc::channel();
        sound.capture = Box::into_raw(Box::new(Capture {
            tx,
            resampler: Resampler::new(format.mSampleRate),
        }));
        unsafe {
            check(
                AudioDeviceCreateIOProcID(
                    sound.device,
                    Some(io_proc),
                    sound.capture.cast(),
                    NonNull::from(&mut sound.io_proc),
                ),
                "couldn't read this Mac's sound",
            )?;
            check(
                AudioDeviceStart(sound.device, sound.io_proc),
                "couldn't start reading this Mac's sound",
            )?;
        }
        Ok((sound, rx))
    }
}

impl Drop for SystemSound {
    fn drop(&mut self) {
        unsafe {
            if self.io_proc.is_some() {
                // Stopping waits for a running callback, so the capture state is free after.
                AudioDeviceStop(self.device, self.io_proc);
                AudioDeviceDestroyIOProcID(self.device, self.io_proc);
            }
            if self.device != 0 {
                AudioHardwareDestroyAggregateDevice(self.device);
            }
            if self.tap != 0 {
                // Destroying the tap is what unmutes the Mac.
                AudioHardwareDestroyProcessTap(self.tap);
            }
            if !self.capture.is_null() {
                drop(Box::from_raw(self.capture));
            }
        }
    }
}

/// What the IO proc keeps between callbacks.
struct Capture {
    tx: mpsc::Sender<Vec<i16>>,
    resampler: Resampler,
}

unsafe extern "C-unwind" fn io_proc(
    _device: AudioObjectID,
    _now: NonNull<AudioTimeStamp>,
    input: NonNull<AudioBufferList>,
    _input_time: NonNull<AudioTimeStamp>,
    _output: NonNull<AudioBufferList>,
    _output_time: NonNull<AudioTimeStamp>,
    client: *mut c_void,
) -> OSStatus {
    let capture = unsafe { &mut *client.cast::<Capture>() };
    let list = input.as_ptr();
    let buffers: &[AudioBuffer] = unsafe {
        std::slice::from_raw_parts(
            (&raw const (*list).mBuffers).cast(),
            (*list).mNumberBuffers as usize,
        )
    };
    let channels: Vec<(&[f32], usize)> = buffers
        .iter()
        .filter(|b| !b.mData.is_null() && b.mNumberChannels > 0)
        .map(|b| unsafe {
            (
                std::slice::from_raw_parts(
                    b.mData.cast::<f32>(),
                    b.mDataByteSize as usize / size_of::<f32>(),
                ),
                b.mNumberChannels as usize,
            )
        })
        .collect();
    let frames = stereo(&channels);
    if !frames.is_empty() {
        let mut pcm = Vec::with_capacity(frames.len() * 3);
        capture.resampler.process(&frames, &mut pcm);
        let _ = capture.tx.send(pcm);
    }
    0
}

/// The first two channels across `buffers` (each a slice of samples and how many channels
/// are interleaved in it), as stereo frames. Interleaved audio is one buffer of two channels;
/// non-interleaved is a buffer per channel. Mono is copied to both sides.
fn stereo(buffers: &[(&[f32], usize)]) -> Vec<[f32; 2]> {
    let channel = |n: usize| {
        let mut n = n;
        for &(samples, count) in buffers {
            if n < count {
                return Some((samples, count, n));
            }
            n -= count;
        }
        None
    };
    let Some(left) = channel(0) else {
        return vec![];
    };
    let right = channel(1).unwrap_or(left);
    let frames = (left.0.len() / left.1).min(right.0.len() / right.1);
    let sample =
        |(samples, count, n): (&[f32], usize, usize), frame: usize| samples[frame * count + n];
    (0..frames)
        .map(|f| [sample(left, f), sample(right, f)])
        .collect()
}

/// Linear resampling to 48 kHz, carrying its position across chunks.
struct Resampler {
    /// Input frames per output frame.
    step: f64,
    /// Where the next output falls, counting `previous` as 0 and the chunk from 1.
    position: f64,
    previous: [f32; 2],
}

impl Resampler {
    fn new(rate: f64) -> Self {
        let rate = if rate > 0.0 { rate } else { SAMPLE_RATE as f64 };
        Self {
            step: rate / SAMPLE_RATE as f64,
            position: 1.0,
            previous: [0.0; 2],
        }
    }

    /// Append `input`, resampled, to `out` as interleaved i16.
    fn process(&mut self, input: &[[f32; 2]], out: &mut Vec<i16>) {
        let Some(&last) = input.last() else { return };
        let len = input.len() as f64;
        while self.position <= len {
            let i = self.position.floor() as usize;
            let t = (self.position - i as f64) as f32;
            let a = if i == 0 { self.previous } else { input[i - 1] };
            let b = input.get(i).copied().unwrap_or(a);
            for c in 0..2 {
                out.push(to_i16(a[c] + (b[c] - a[c]) * t));
            }
            self.position += self.step;
        }
        self.position -= len;
        self.previous = last;
    }
}

fn to_i16(sample: f32) -> i16 {
    (sample.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16
}

/// MouseTail's own Core Audio process object, if it has one.
fn own_process_object() -> Option<AudioObjectID> {
    let pid = std::process::id() as i32;
    let object: AudioObjectID = unsafe {
        property(
            kAudioObjectSystemObject as AudioObjectID,
            kAudioHardwarePropertyTranslatePIDToProcessObject,
            Some(pid),
        )
        .ok()?
    };
    (object != 0).then_some(object)
}

/// Read a fixed-size property. `T` must be the property's type (zeroed is a valid value).
unsafe fn property<T>(
    object: AudioObjectID,
    selector: AudioObjectPropertySelector,
    pid: Option<i32>,
) -> anyhow::Result<T> {
    let address = AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    };
    let mut value = MaybeUninit::<T>::zeroed();
    let mut size = size_of::<T>() as u32;
    let (qualifier_size, qualifier) = match &pid {
        Some(pid) => (size_of::<i32>() as u32, ptr::from_ref(pid).cast::<c_void>()),
        None => (0, ptr::null()),
    };
    unsafe {
        check(
            AudioObjectGetPropertyData(
                object,
                NonNull::from(&address),
                qualifier_size,
                qualifier,
                NonNull::from(&mut size),
                NonNull::new_unchecked(value.as_mut_ptr().cast()),
            ),
            "couldn't read an audio property",
        )?;
        Ok(value.assume_init())
    }
}

fn check(status: OSStatus, what: &str) -> anyhow::Result<()> {
    anyhow::ensure!(status == 0, "{what} (error {status})");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interleaved_stereo() {
        let samples = [0.1, 0.2, 0.3, 0.4];
        assert_eq!(stereo(&[(&samples, 2)]), vec![[0.1, 0.2], [0.3, 0.4]]);
    }

    #[test]
    fn separate_channels() {
        let (l, r) = ([0.1, 0.3], [0.2, 0.4]);
        assert_eq!(stereo(&[(&l, 1), (&r, 1)]), vec![[0.1, 0.2], [0.3, 0.4]]);
    }

    #[test]
    fn mono_goes_to_both_sides() {
        assert_eq!(stereo(&[(&[0.5, -0.5], 1)]), vec![[0.5, 0.5], [-0.5, -0.5]]);
    }

    #[test]
    fn extra_channels_are_ignored() {
        let samples = [0.1, 0.2, 9.0, 0.3, 0.4, 9.0];
        assert_eq!(stereo(&[(&samples, 3)]), vec![[0.1, 0.2], [0.3, 0.4]]);
        assert!(stereo(&[]).is_empty());
    }

    #[test]
    fn samples_clamp_to_i16() {
        assert_eq!(to_i16(0.0), 0);
        assert_eq!(to_i16(1.0), i16::MAX);
        assert_eq!(to_i16(-1.0), -i16::MAX);
        assert_eq!(to_i16(2.0), i16::MAX);
        assert_eq!(to_i16(f32::NAN), 0);
    }

    #[test]
    fn same_rate_passes_through() {
        let mut r = Resampler::new(48_000.0);
        let mut out = vec![];
        r.process(&[[0.5, -0.5], [0.25, 0.0]], &mut out);
        r.process(&[[1.0, -1.0]], &mut out);
        assert_eq!(out, [16384, -16384, 8192, 0, 32767, -32767]);
    }

    #[test]
    fn upsampling_keeps_time_and_shape() {
        // One second of a ramp at 44.1 kHz in uneven chunks.
        let input: Vec<[f32; 2]> = (0..44_100)
            .map(|i| {
                let x = i as f32 / 44_100.0;
                [x, -x]
            })
            .collect();
        let mut r = Resampler::new(44_100.0);
        let mut out = vec![];
        for chunk in input.chunks(441) {
            r.process(chunk, &mut out);
        }
        let frames = out.len() / 2;
        assert!((47_990..=48_000).contains(&frames), "{frames} frames");
        // Still a smooth ramp: each step is about 1/48000 of full scale.
        for pair in out.chunks(2).collect::<Vec<_>>().windows(2) {
            let step = pair[1][0] - pair[0][0];
            assert!((0..=2).contains(&step), "step {step}");
            assert_eq!(pair[1][1], -pair[1][0]);
        }
    }

    #[test]
    fn downsampling_matches_one_pass() {
        let input: Vec<[f32; 2]> = (0..9600).map(|i| [(i as f32 * 0.01).sin(); 2]).collect();
        let mut whole = vec![];
        Resampler::new(96_000.0).process(&input, &mut whole);
        let mut chunked = vec![];
        let mut r = Resampler::new(96_000.0);
        for chunk in input.chunks(333) {
            r.process(chunk, &mut chunked);
        }
        assert_eq!(whole, chunked);
        assert_eq!(whole.len() / 2, 4800);
    }
}
