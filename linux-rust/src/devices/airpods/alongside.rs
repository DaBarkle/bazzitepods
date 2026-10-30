//! "Stay connected alongside my iPhone": while another Apple device shares the AirPods, tell
//! the AirPods to keep this PC on their host list, so they don't drop it for the iPhone.
//! The device that plays goes first.
//!
//! Idea and packet from EarPort (github.com/Anoryth/earport, daemon/src/tipi.c, GPL-3.0),
//! written anew here.

use crate::bluetooth::aacp::ConnectedDevice;
use std::time::{Duration, Instant};

/// Connected Devices host status: connected (not just connecting).
const CONNECTED: u8 = 0x02;
/// Connected Devices host flags: on the AirPods' priority list / playing.
const LISTED: u8 = 0x01;
const PLAYS: u8 = 0x02;
/// The AirPods sometimes answer before they apply a list, so an unchanged one waits this long.
const REPEAT_GAP: Duration = Duration::from_secs(2);
/// Never more than this many lists per connection.
const MAX_PER_SESSION: u8 = 8;

/// The other host, and whether this PC plays (which decides the order).
type Key = (String, bool);

#[derive(Debug, Default)]
pub(super) struct Alongside {
    last: Option<Key>,
    sent_at: Option<Instant>,
    sent: u8,
}

impl Alongside {
    /// The hosts to send, first one first, when the AirPods' list needs (re)sending.
    pub(super) fn next(
        &mut self,
        devices: &[ConnectedDevice],
        local: &str,
        now: Instant,
    ) -> Option<[String; 2]> {
        let me = devices.iter().find(|d| d.mac.eq_ignore_ascii_case(local))?;
        let other = devices
            .iter()
            .find(|d| !d.mac.eq_ignore_ascii_case(local) && d.info1 == CONNECTED)?;
        let plays = me.info2 & PLAYS != 0;
        let key = (other.mac.clone(), plays);
        let same = self.last.as_ref() == Some(&key);
        if self.sent >= MAX_PER_SESSION
            || (same && me.info2 & LISTED != 0)
            || (same
                && self
                    .sent_at
                    .is_some_and(|t| now.duration_since(t) < REPEAT_GAP))
        {
            return None;
        }
        self.last = Some(key);
        self.sent_at = Some(now);
        self.sent += 1;
        let (me, other) = (local.to_string(), other.mac.clone());
        Some(if plays { [me, other] } else { [other, me] })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PC: &str = "E8:65:38:93:3D:9E";
    const PHONE: &str = "AA:BB:CC:DD:EE:FF";

    fn host(mac: &str, status: u8, flags: u8) -> ConnectedDevice {
        ConnectedDevice {
            mac: mac.into(),
            info1: status,
            info2: flags,
            r#type: None,
        }
    }

    #[test]
    fn lists_the_player_first_and_stops_once_listed() {
        let t = Instant::now();
        let mut a = Alongside::default();
        let idle = [host(PC, CONNECTED, 0), host(PHONE, CONNECTED, 0)];
        assert_eq!(a.next(&idle, PC, t), Some([PHONE.into(), PC.into()]));
        // Unchanged: not again within the gap, and never once the AirPods list this PC.
        assert_eq!(a.next(&idle, PC, t + Duration::from_secs(1)), None);
        let listed = [host(PC, CONNECTED, LISTED), host(PHONE, CONNECTED, 0)];
        assert_eq!(a.next(&listed, PC, t + Duration::from_secs(5)), None);
        // This PC starts playing: it goes first.
        let playing = [
            host(PC, CONNECTED, LISTED | PLAYS),
            host(PHONE, CONNECTED, 0),
        ];
        assert_eq!(
            a.next(&playing, PC, t + Duration::from_secs(6)),
            Some([PC.into(), PHONE.into()])
        );
    }

    #[test]
    fn nothing_without_a_connected_partner_and_at_most_eight() {
        let t = Instant::now();
        let mut a = Alongside::default();
        assert_eq!(a.next(&[host(PC, CONNECTED, 0)], PC, t), None);
        assert_eq!(
            a.next(&[host(PC, CONNECTED, 0), host(PHONE, 0x01, 0)], PC, t),
            None
        );
        let idle = [host(PC, CONNECTED, 0), host(PHONE, CONNECTED, 0)];
        let sent = (0..20)
            .filter(|i| a.next(&idle, PC, t + Duration::from_secs(3 * i)).is_some())
            .count();
        assert_eq!(sent, 8);
    }
}
