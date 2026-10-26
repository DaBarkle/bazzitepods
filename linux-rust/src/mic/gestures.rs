//! Stem holds while the mic switch is on: the left hold swaps mics, the right hold
//! changes noise control (which the AirPods no longer do themselves once the hold is
//! claimed).

/// Control 0x39 (raw gestures) bits: presses the AirPods hand to this PC instead of
/// acting on themselves. The AirPods keep the value across reconnects, so it is sent on
/// every connect and whenever a setting changes, including 0 to give the presses back.
pub const DOUBLE_PRESS: u8 = 0x02;
pub const TRIPLE_PRESS: u8 = 0x04;
pub const LONG_PRESS: u8 = 0x08;

pub fn stem_claims(track_control: bool, mic_switch: bool) -> u8 {
    let mut mask = 0;
    if track_control {
        mask |= DOUBLE_PRESS | TRIPLE_PRESS;
    }
    if mic_switch {
        mask |= LONG_PRESS;
    }
    mask
}

/// Listening modes in the order a hold steps through them.
const CYCLE: [u8; 4] = [0x02, 0x03, 0x04, 0x01];

/// Bit of a listening mode (0x0D value) in the enabled-modes mask (control 0x1A).
fn bit(mode: u8) -> u8 {
    1 << (mode - 1)
}

/// The mode a hold switches to: the next of the modes picked for press and hold
/// (control 0x1A), like the AirPods do. When that is unknown or picks fewer than two,
/// noise cancellation and transparency.
pub fn next_listening_mode(current: Option<u8>, enabled: Option<u8>) -> u8 {
    let picked: Vec<u8> = match enabled {
        Some(mask) => CYCLE.into_iter().filter(|m| mask & bit(*m) != 0).collect(),
        None => Vec::new(),
    };
    let modes = if picked.len() >= 2 {
        picked
    } else {
        vec![0x02, 0x03]
    };
    match current.and_then(|c| modes.iter().position(|m| *m == c)) {
        Some(i) => modes[(i + 1) % modes.len()],
        None => modes[0],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claims_combine_both_settings() {
        assert_eq!(stem_claims(false, false), 0x00);
        assert_eq!(stem_claims(true, false), 0x06);
        assert_eq!(stem_claims(false, true), 0x08);
        assert_eq!(stem_claims(true, true), 0x0E);
    }

    #[test]
    fn hold_cycles_the_picked_modes() {
        // Noise cancellation, transparency and adaptive picked.
        let mask = 0x02 | 0x04 | 0x08;
        assert_eq!(next_listening_mode(Some(0x02), Some(mask)), 0x03);
        assert_eq!(next_listening_mode(Some(0x03), Some(mask)), 0x04);
        assert_eq!(next_listening_mode(Some(0x04), Some(mask)), 0x02);
        // Off is only in the cycle when picked.
        assert_eq!(next_listening_mode(Some(0x04), Some(mask | 0x01)), 0x01);
        assert_eq!(next_listening_mode(Some(0x01), Some(mask | 0x01)), 0x02);
        // A mode outside the cycle (Off when it isn't picked) goes to the first one.
        assert_eq!(next_listening_mode(Some(0x01), Some(mask)), 0x02);
    }

    #[test]
    fn hold_falls_back_to_noise_cancellation_and_transparency() {
        assert_eq!(next_listening_mode(Some(0x02), None), 0x03);
        assert_eq!(next_listening_mode(Some(0x03), None), 0x02);
        assert_eq!(next_listening_mode(None, None), 0x02);
        assert_eq!(next_listening_mode(Some(0x03), Some(0x04)), 0x02);
    }
}
