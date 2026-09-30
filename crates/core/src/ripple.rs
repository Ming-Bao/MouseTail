//! The ripple drawn where the cursor crosses to or from another computer: a short train of
//! waves spreading from the edge, as if the cursor dropped through the surface of the screen.
//!
//! The shape lives here so every platform draws the same thing: the Mac evaluates it in a
//! Metal shader (which mirrors `height` and `shade`), Linux on the CPU.
//! Distances are in points (logical pixels), times in seconds.

use std::time::Instant;

use crate::layout::Point;

/// Distance between crests.
pub const WAVELENGTH: f32 = 13.0;
/// How fast the front travels.
pub const SPEED: f32 = 190.0;
/// How long a ripple lasts, from the crossing until it has faded away.
pub const LIFETIME: f32 = 1.3;
/// How far from its origin a ripple ever shows: the front's travel plus a little ahead of it.
pub const REACH: f32 = SPEED * LIFETIME + WAVELENGTH;
/// Peak white on each crest and black in each trough (premultiplied alpha).
pub const HIGHLIGHT: f32 = 0.45;
pub const SHADOW: f32 = 0.175;
/// How quickly waves shrink as they spread: half height at three times this far out.
pub const SPREAD: f32 = 30.0;
/// Scales the wave's height before shading, so it stays visible as it spreads and fades.
pub const GAIN: f32 = 2.5;

/// The cursor arriving makes a full ripple; leaving, a smaller one.
pub const ARRIVING: f32 = 1.0;
pub const LEAVING: f32 = 0.5;

/// Surface height of one ripple at distance `d` from where it started, `age` seconds in.
/// Overlapping ripples add up, so they interfere like real ones.
pub fn height(d: f32, age: f32, strength: f32) -> f32 {
    let front = SPEED * age;
    let behind = front - d; // > 0 inside the front
    let width = train_width(age);
    let train = if behind > 0.0 {
        (-(behind / width).powi(2)).exp()
    } else {
        (-(behind / (WAVELENGTH * 0.35)).powi(2)).exp()
    };
    let fade = (1.0 - (age / LIFETIME).clamp(0.0, 1.0)).powi(2);
    let spread = 1.0 / (1.0 + d / SPREAD).sqrt();
    strength * fade * spread * train * (std::f32::consts::TAU * behind / WAVELENGTH).sin()
}

/// How far from its origin a ripple `age` seconds in is still flat: the waves trail behind
/// the front, and further back than this they've died down to nothing visible. Lets drawing
/// skip the calm middle.
pub fn calm_within(age: f32) -> f32 {
    (SPEED * age - 2.5 * train_width(age)).max(0.0)
}

fn train_width(age: f32) -> f32 {
    WAVELENGTH * (1.2 + 3.0 * age) // the train spreads out as it travels
}

/// Colour for a summed height, shaded the same in every direction: crests catch the light,
/// troughs go dark. Returns premultiplied (white level, alpha).
pub fn shade(h: f32) -> (f32, f32) {
    let white = (h * GAIN).clamp(0.0, 1.0).powf(1.5) * HIGHLIGHT;
    let black = (-h * GAIN).clamp(0.0, 1.0) * SHADOW;
    (white, white + black * (1.0 - white))
}

/// A ripple in flight.
#[derive(Clone, Copy, Debug)]
pub struct Ripple {
    /// Where it started, in the drawing surface's coordinates.
    pub origin: Point,
    pub started: Instant,
    pub strength: f32,
}

impl Ripple {
    pub fn age(&self, now: Instant) -> f32 {
        now.saturating_duration_since(self.started).as_secs_f32()
    }

    pub fn done(&self, now: Instant) -> bool {
        self.age(now) >= LIFETIME
    }
}

/// Most ripples drawn at once; a new one past this replaces the oldest.
pub const MAX_RIPPLES: usize = 8;

/// Add a ripple, keeping at most `MAX_RIPPLES`.
pub fn push(ripples: &mut Vec<Ripple>, ripple: Ripple) {
    ripples.push(ripple);
    if ripples.len() > MAX_RIPPLES {
        ripples.remove(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_ahead_of_the_front_or_after_it_fades() {
        assert!(height(REACH, 0.5, 1.0).abs() < 1e-3);
        assert!(height(10.0, LIFETIME, 1.0).abs() < 1e-6);
        assert_eq!(shade(0.0), (0.0, 0.0));
    }

    #[test]
    fn calm_middle_shows_nothing() {
        assert_eq!(calm_within(0.1), 0.0); // nothing calm yet
        for age in [0.6, 0.8, 1.0, 1.2] {
            let calm = calm_within(age);
            for d in [0.0, calm * 0.5, calm] {
                let (_, alpha) = shade(height(d, age, 1.0));
                assert!(alpha < 1.0 / 255.0, "age {age} d {d}: {alpha}");
            }
        }
        assert!(calm_within(1.0) > 50.0);
    }

    #[test]
    fn crests_are_light_and_troughs_dark() {
        let (white, alpha) = shade(0.5);
        assert!(white >= HIGHLIGHT * 0.9 && alpha >= white);
        let (white, alpha) = shade(-0.5);
        assert_eq!(white, 0.0);
        assert!(alpha > 0.1);
    }

    #[test]
    fn keeps_the_newest() {
        let now = Instant::now();
        let mut list = vec![];
        for i in 0..MAX_RIPPLES + 2 {
            push(
                &mut list,
                Ripple {
                    origin: Point::new(i as f64, 0.0),
                    started: now,
                    strength: 1.0,
                },
            );
        }
        assert_eq!(list.len(), MAX_RIPPLES);
        assert_eq!(list[0].origin.x, 2.0);
    }
}
