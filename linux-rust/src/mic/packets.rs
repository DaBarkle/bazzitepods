//! AACP framing of the AirPods' high-quality mic stream (message type 0x58).
//!
//! Found by LibrePods PR #655 (LuanAdemi, packet captures by kavishdevar) and checked on
//! AirPods 4 ANC on 2026-10-26: after START the AirPods send about 33 audio packets a
//! second on the control channel, each with a 22-byte header and then four AAC-ELD access
//! units (480 samples, 7.5 ms each) as `[timestamp: u32 LE][length: u8][access unit]`.

/// AACP message type of the mic stream (start, stop, acks and audio).
pub const MIC_STREAM_OPCODE: u8 = 0x58;

/// Starts the stream. Includes the 04 00 04 00 AACP header.
pub const MIC_START: [u8; 19] = [
    0x04, 0x00, 0x04, 0x00, 0x58, 0x00, 0x00, 0x00, 0x09, 0x00, 0x00, 0x01, 0x82, 0x00, 0x00, 0x00,
    0x04, 0x96, 0x00,
];

/// Stops the stream.
pub const MIC_STOP: [u8; 12] = [
    0x04, 0x00, 0x04, 0x00, 0x58, 0x00, 0x00, 0x00, 0x02, 0x00, 0x03, 0x01,
];

/// AudioSpecificConfig of the stream: AAC-ELD (object type 39), mono. It says 48 kHz, but
/// 480 samples arrive every 7.5 ms, so the audio really runs at 64 kHz.
pub const ELD_ASC: [u8; 4] = [0xF8, 0xE6, 0x30, 0x00];
pub const ELD_SAMPLE_RATE: u32 = 64_000;
pub const ELD_FRAME_SAMPLES: usize = 480;

const AUDIO_HEADER_LEN: usize = 22;

/// Whether an AACP packet is a mic audio packet (subtype 0x0001), not an ack.
pub fn is_audio(packet: &[u8]) -> bool {
    packet.len() >= 8
        && packet[..4] == [0x04, 0x00, 0x04, 0x00]
        && packet[4] == MIC_STREAM_OPCODE
        && packet[5] == 0x00
        && packet[6] == 0x01
        && packet[7] == 0x00
}

/// The access units in one audio packet. A truncated last unit is dropped.
pub fn access_units(packet: &[u8]) -> impl Iterator<Item = &[u8]> {
    let mut off = AUDIO_HEADER_LEN;
    std::iter::from_fn(move || {
        let len = *packet.get(off + 4)? as usize;
        let start = off + 5;
        let unit = packet.get(start..start + len)?;
        off = start + len;
        Some(unit)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn audio_packet(units: &[&[u8]]) -> Vec<u8> {
        let mut p = vec![0x04, 0x00, 0x04, 0x00, 0x58, 0x00, 0x01, 0x00];
        p.resize(AUDIO_HEADER_LEN, 0xAA);
        for (i, u) in units.iter().enumerate() {
            p.extend_from_slice(&(i as u32 * 480).to_le_bytes());
            p.push(u.len() as u8);
            p.extend_from_slice(u);
        }
        p
    }

    #[test]
    fn splits_audio_packets_into_access_units() {
        let p = audio_packet(&[&[1, 2, 3], &[], &[4; 70], &[5]]);
        assert!(is_audio(&p));
        let units: Vec<&[u8]> = access_units(&p).collect();
        assert_eq!(units.len(), 4);
        assert_eq!(units[0], &[1, 2, 3]);
        assert!(units[1].is_empty());
        assert_eq!(units[2].len(), 70);
        assert_eq!(units[3], &[5]);
    }

    #[test]
    fn truncated_units_and_short_packets_are_safe() {
        let mut p = audio_packet(&[&[1, 2, 3], &[9; 40]]);
        p.truncate(p.len() - 10);
        assert_eq!(access_units(&p).count(), 1);
        assert_eq!(access_units(&p[..AUDIO_HEADER_LEN + 3]).count(), 0);
        assert_eq!(access_units(&[]).count(), 0);
        assert!(!is_audio(&p[..7]));
    }

    #[test]
    fn start_stop_and_acks_are_not_audio() {
        assert!(!is_audio(&MIC_START));
        assert!(!is_audio(&MIC_STOP));
        // The START ack seen from AirPods 4 ANC.
        let ack = [0x04, 0x00, 0x04, 0x00, 0x58, 0x00, 0x00, 0x00, 0x01, 0x01];
        assert!(!is_audio(&ack));
    }
}
