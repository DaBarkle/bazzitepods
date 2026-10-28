//! Damped springs, the motion model behind the island (as in SwiftUI's
//! `spring(duration:bounce:)`). A spring keeps its velocity when its target moves, so an
//! animation that is interrupted half way bends smoothly towards the new target instead
//! of restarting.

/// How a spring moves: `duration` is the perceptual duration in seconds, `bounce` is
/// 0 for no overshoot and ~0.3 for a clearly bouncy settle.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Feel {
    pub duration: f32,
    pub bounce: f32,
}

impl Feel {
    pub const fn new(duration: f32, bounce: f32) -> Self {
        Self { duration, bounce }
    }

    /// Unit-mass stiffness and damping, with SwiftUI's mapping.
    fn coefficients(self) -> (f32, f32) {
        let d = self.duration.max(0.05);
        let stiffness = (2.0 * std::f32::consts::PI / d).powi(2);
        let b = self.bounce.clamp(-0.9, 0.9);
        let damping = if b >= 0.0 {
            4.0 * std::f32::consts::PI * (1.0 - b) / d
        } else {
            4.0 * std::f32::consts::PI / (d * (1.0 + b))
        };
        (stiffness, damping)
    }
}

/// Integration step; small enough to be stable for the stiffest feel used here.
const STEP: f32 = 1.0 / 480.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Spring {
    pub value: f32,
    pub velocity: f32,
    pub target: f32,
    feel: Feel,
    /// Distance and speed below which the spring snaps to its target and stops.
    epsilon: f32,
}

impl Spring {
    pub fn new(value: f32, feel: Feel, epsilon: f32) -> Self {
        Self {
            value,
            velocity: 0.0,
            target: value,
            feel,
            epsilon,
        }
    }

    /// Moves the target, keeping the current velocity.
    pub fn to(&mut self, target: f32, feel: Feel) {
        self.target = target;
        self.feel = feel;
    }

    /// Jumps to `value` with no motion.
    pub fn snap(&mut self, value: f32) {
        self.value = value;
        self.target = value;
        self.velocity = 0.0;
    }

    pub fn kick(&mut self, velocity: f32) {
        self.velocity += velocity;
    }

    pub fn at_rest(&self) -> bool {
        self.value == self.target && self.velocity == 0.0
    }

    /// Advances by `dt` seconds. Returns true while still moving.
    pub fn step(&mut self, dt: f32) -> bool {
        if self.at_rest() {
            return false;
        }
        let (k, c) = self.feel.coefficients();
        let mut left = dt.max(0.0);
        while left > 0.0 {
            let h = left.min(STEP);
            // Semi-implicit Euler: stable and cheap for these stiffnesses.
            let accel = -k * (self.value - self.target) - c * self.velocity;
            self.velocity += accel * h;
            self.value += self.velocity * h;
            left -= h;
        }
        if (self.value - self.target).abs() < self.epsilon && self.velocity.abs() < self.epsilon {
            self.snap(self.target);
            return false;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settle(s: &mut Spring, max_secs: f32) -> (f32, f32) {
        let mut t = 0.0;
        let mut peak = s.value;
        while s.step(1.0 / 60.0) {
            t += 1.0 / 60.0;
            peak = peak.max(s.value);
            assert!(t < max_secs, "spring did not settle within {max_secs}s");
        }
        (t, peak)
    }

    #[test]
    fn smooth_spring_does_not_overshoot() {
        let mut s = Spring::new(0.0, Feel::new(0.4, 0.0), 0.01);
        s.to(100.0, Feel::new(0.4, 0.0));
        let (t, peak) = settle(&mut s, 2.0);
        assert_eq!(s.value, 100.0);
        assert!(peak <= 100.0 + 0.01, "peak {peak}");
        assert!(t > 0.2 && t < 1.2, "settled in {t}s");
    }

    #[test]
    fn bouncy_spring_overshoots_a_little() {
        let mut s = Spring::new(0.0, Feel::new(0.5, 0.25), 0.01);
        s.to(100.0, Feel::new(0.5, 0.25));
        let (_, peak) = settle(&mut s, 3.0);
        assert!(peak > 101.0 && peak < 112.0, "peak {peak}");
    }

    #[test]
    fn retarget_keeps_velocity() {
        let mut s = Spring::new(0.0, Feel::new(0.4, 0.0), 0.01);
        s.to(100.0, Feel::new(0.4, 0.0));
        for _ in 0..6 {
            s.step(1.0 / 60.0);
        }
        let v = s.velocity;
        assert!(v > 0.0);
        s.to(0.0, Feel::new(0.4, 0.0));
        assert_eq!(s.velocity, v, "velocity carried into the new target");
        settle(&mut s, 2.0);
        assert_eq!(s.value, 0.0);
    }

    #[test]
    fn large_steps_stay_stable() {
        let mut s = Spring::new(0.0, Feel::new(0.25, 0.3), 0.01);
        s.to(50.0, Feel::new(0.25, 0.3));
        for _ in 0..40 {
            s.step(0.05);
            assert!(s.value.is_finite() && s.value.abs() < 200.0);
        }
        assert_eq!(s.value, 50.0);
    }

    #[test]
    fn rest_and_kick() {
        let mut s = Spring::new(1.0, Feel::new(0.3, 0.5), 0.0005);
        assert!(s.at_rest());
        assert!(!s.step(0.016));
        s.kick(0.5);
        assert!(s.step(0.016));
        assert!(s.value > 1.0);
        settle(&mut s, 3.0);
        assert_eq!(s.value, 1.0);
    }
}
