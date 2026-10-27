//! Automatic gain for the AirPods mic. The stream arrives quiet (about -33 dBFS of
//! speech on AirPods 4 ANC), so it is lifted towards a steady level, at most +18 dB,
//! with a soft limiter instead of clipping. Adapted from LibrePods PR #655.

pub struct Agc {
    envelope: f32,
    gain: f32,
}

impl Agc {
    /// About -12 dBFS.
    const TARGET: f32 = 0.25;
    /// +18 dB.
    const MAX_GAIN: f32 = 8.0;
    const ATTACK: f32 = 0.02;
    const RELEASE: f32 = 0.001;
    const SMOOTHING: f32 = 0.01;
    /// Below this the input is treated as silence and the gain holds.
    const FLOOR: f32 = 1e-4;

    pub fn new() -> Self {
        Self {
            envelope: 0.0,
            gain: 1.0,
        }
    }

    /// Processes samples in -1.0..=1.0 in place.
    pub fn process(&mut self, samples: &mut [f32]) {
        for s in samples {
            let level = s.abs();
            let rate = if level > self.envelope {
                Self::ATTACK
            } else {
                Self::RELEASE
            };
            self.envelope += (level - self.envelope) * rate;
            let desired = (Self::TARGET / self.envelope.max(Self::FLOOR)).min(Self::MAX_GAIN);
            self.gain += (desired - self.gain) * Self::SMOOTHING;
            *s = (*s * self.gain).tanh();
        }
    }
}

/// -1.0..=1.0 to 16-bit PCM.
pub fn to_s16(s: f32) -> i16 {
    (s.clamp(-1.0, 1.0) * 32767.0).round() as i16
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(amplitude: f32, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| amplitude * (i as f32 * 2.0 * std::f32::consts::PI * 300.0 / 64000.0).sin())
            .collect()
    }

    fn peak(s: &[f32]) -> f32 {
        s.iter().fold(0.0, |m, x| m.max(x.abs()))
    }

    #[test]
    fn quiet_speech_is_lifted_and_loud_sound_never_clips() {
        let mut agc = Agc::new();
        let mut quiet = tone(0.02, 64000);
        agc.process(&mut quiet);
        let tail = peak(&quiet[48000..]);
        assert!(tail > 0.1, "quiet input only reached {tail}");

        let mut agc = Agc::new();
        let mut loud = tone(1.0, 64000);
        agc.process(&mut loud);
        assert!(peak(&loud) < 1.0);
    }

    #[test]
    fn silence_stays_silent() {
        let mut agc = Agc::new();
        let mut s = vec![0.0; 10000];
        agc.process(&mut s);
        assert_eq!(peak(&s), 0.0);
    }

    #[test]
    fn converts_to_16_bit() {
        assert_eq!(to_s16(0.0), 0);
        assert_eq!(to_s16(1.0), 32767);
        assert_eq!(to_s16(-2.0), -32767);
        assert_eq!(to_s16(0.5), 16384);
    }
}
