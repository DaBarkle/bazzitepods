//! Window motion: SwiftUI-style springs keyed by what they move.
//!
//! The window sets targets after every update ([`Motion::target`]) and reads interpolated values
//! while drawing ([`Motion::value`]). Frames are only requested while something is still moving
//! ([`Motion::is_animating`]), so a window at rest costs nothing.

use std::collections::HashMap;
use std::f32::consts::TAU;
use std::hash::Hash;
use std::time::Instant;

/// A damped spring described the way SwiftUI does: `response` is roughly the time (s) to settle
/// visually, `damping` is the damping ratio (1 = no overshoot, < 1 = a little bounce).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Curve {
    pub response: f32,
    pub damping: f32,
}

/// Selection highlights: quick with a hint of liquid overshoot.
pub const SNAPPY: Curve = Curve {
    response: 0.32,
    damping: 0.82,
};
/// Battery rings and glows: unhurried, no overshoot.
pub const SMOOTH: Curve = Curve {
    response: 0.9,
    damping: 1.0,
};

/// Positions and velocities this small count as settled.
const REST: f32 = 0.0005;

#[derive(Debug, Clone, Copy)]
pub struct Spring {
    curve: Curve,
    target: f32,
    /// Displacement from the target and velocity at `since`.
    x0: f32,
    v0: f32,
    since: Instant,
}

impl Spring {
    pub fn new(value: f32, curve: Curve, now: Instant) -> Self {
        Self {
            curve,
            target: value,
            x0: 0.0,
            v0: 0.0,
            since: now,
        }
    }

    /// Displacement and velocity `t` seconds after `since` (closed form, no stepping).
    fn state(&self, t: f32) -> (f32, f32) {
        let Curve { response, damping } = self.curve;
        let w = TAU / response.max(0.01);
        let (x0, v0) = (self.x0, self.v0);
        if damping >= 1.0 {
            // Critically damped.
            let e = (-w * t).exp();
            let b = v0 + w * x0;
            let x = (x0 + b * t) * e;
            let v = (b - w * (x0 + b * t)) * e;
            (x, v)
        } else {
            let z = damping;
            let wd = w * (1.0 - z * z).sqrt();
            let e = (-z * w * t).exp();
            let b = (v0 + z * w * x0) / wd;
            let (s, c) = (wd * t).sin_cos();
            let x = e * (x0 * c + b * s);
            let v = e * ((-x0 * wd) * s + b * wd * c) - z * w * x;
            (x, v)
        }
    }

    fn elapsed(&self, now: Instant) -> f32 {
        now.saturating_duration_since(self.since).as_secs_f32()
    }

    pub fn value(&self, now: Instant) -> f32 {
        if self.at_rest(now) {
            return self.target;
        }
        self.target + self.state(self.elapsed(now)).0
    }

    pub fn at_rest(&self, now: Instant) -> bool {
        let (x, v) = self.state(self.elapsed(now));
        x.abs() < REST && v.abs() < REST * 10.0
    }

    /// Head for `target`, keeping the current position and velocity (no jumps when a new
    /// target arrives mid-flight).
    pub fn retarget(&mut self, target: f32, now: Instant) {
        if target == self.target {
            return;
        }
        let (x, v) = if self.at_rest(now) {
            (0.0, 0.0)
        } else {
            self.state(self.elapsed(now))
        };
        let pos = self.target + x;
        self.target = target;
        self.x0 = pos - target;
        self.v0 = v;
        self.since = now;
    }

    /// Jump to `value` and spring back to the current target (used for pulses).
    pub fn kick(&mut self, value: f32, now: Instant) {
        self.x0 = value - self.target;
        self.v0 = 0.0;
        self.since = now;
    }
}

/// All springs of one window.
#[derive(Debug)]
pub struct Motion<K> {
    springs: HashMap<K, Spring>,
}

impl<K: Eq + Hash + Copy> Default for Motion<K> {
    fn default() -> Self {
        Self {
            springs: HashMap::new(),
        }
    }
}

impl<K: Eq + Hash + Copy> Motion<K> {
    /// Point `key` at `target`. A key seen for the first time starts at `from` (or at the
    /// target itself when `from` is None, i.e. no animation on first sight).
    pub fn target(&mut self, key: K, target: f32, from: Option<f32>, curve: Curve, now: Instant) {
        match self.springs.get_mut(&key) {
            Some(s) => s.retarget(target, now),
            None => {
                let mut s = Spring::new(from.unwrap_or(target), curve, now);
                s.retarget(target, now);
                self.springs.insert(key, s);
            }
        }
    }

    pub fn kick(&mut self, key: K, value: f32, now: Instant) {
        if let Some(s) = self.springs.get_mut(&key) {
            s.kick(value, now);
        }
    }

    /// Current value, or `fallback` for a key that has never been targeted.
    pub fn value(&self, key: K, fallback: f32, now: Instant) -> f32 {
        self.springs.get(&key).map_or(fallback, |s| s.value(now))
    }

    /// Forget every spring, so the next targets start from `from` again (window reopened).
    pub fn reset(&mut self) {
        self.springs.clear();
    }

    pub fn is_animating(&self, now: Instant) -> bool {
        self.springs.values().any(|s| !s.at_rest(now))
    }
}

#[cfg(test)]
#[path = "motion_tests.rs"]
mod tests;
