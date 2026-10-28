//! Pure, bounds-checked parsers and the `devices.json` merges they feed. None of these may
//! panic on any input.

use super::*;

pub(super) const MAX_BATTERY_RECORDS: usize = 4;
pub(super) const BATTERY_RECORD_LEN: usize = 5;

pub(super) fn battery_component_from_u8(v: u8) -> Option<BatteryComponent> {
    match v {
        0x01 => Some(BatteryComponent::Headphone),
        0x02 => Some(BatteryComponent::Right),
        0x04 => Some(BatteryComponent::Left),
        0x08 => Some(BatteryComponent::Case),
        _ => None,
    }
}

pub(super) fn battery_status_from_u8(v: u8) -> Option<BatteryStatus> {
    match v {
        0x00 => Some(BatteryStatus::Unknown),
        0x01 => Some(BatteryStatus::Charging),
        0x02 => Some(BatteryStatus::NotCharging),
        0x04 => Some(BatteryStatus::Disconnected),
        _ => None,
    }
}

/// Parse a BATTERY_INFO payload (starting at the opcode byte, i.e. packet minus the
/// 4-byte header): `04 00 <count> { <component> 01 <level> <status> 01 }*count`.
///
/// Returns `None` when the count is implausible, the packet is shorter than the
/// count implies, or a record's spacer bytes are wrong (the whole packet is then
/// untrustworthy). Records with an unknown component or status byte are skipped.
pub fn parse_battery_info(payload: &[u8]) -> Option<Vec<BatteryInfo>> {
    if payload.len() < 3 || payload[0] != opcodes::BATTERY_INFO {
        return None;
    }
    let count = payload[2] as usize;
    if count == 0 || count > MAX_BATTERY_RECORDS {
        return None;
    }
    let records = payload.get(3..3 + count * BATTERY_RECORD_LEN)?;
    let mut out = Vec::with_capacity(count);
    for [comp, spacer, level, status, end] in records.as_chunks::<BATTERY_RECORD_LEN>().0 {
        if *spacer != 0x01 || *end != 0x01 {
            return None;
        }
        let Some(component) = battery_component_from_u8(*comp) else {
            debug!("Unknown battery component: {:#04x}", comp);
            continue;
        };
        let Some(status) = battery_status_from_u8(*status) else {
            debug!("Unknown battery status: {:#04x}", status);
            continue;
        };
        out.push(BatteryInfo {
            component,
            level: *level,
            status,
        });
    }
    Some(out)
}

/// Merge freshly parsed records into the accumulated per-component list.
/// Components absent from `new` are kept. A `Disconnected`/`Unknown` record with
/// level 0, or any record with a level above 100, keeps the previously known level
/// (a component seen for the first time is stored as sent).
pub fn merge_battery_info(existing: &mut Vec<BatteryInfo>, new: &[BatteryInfo]) {
    for rec in new {
        let no_data = matches!(
            rec.status,
            BatteryStatus::Disconnected | BatteryStatus::Unknown
        ) && rec.level == 0;
        let level_valid = rec.level <= 100 && !no_data;
        if let Some(old) = existing.iter_mut().find(|b| b.component == rec.component) {
            if level_valid {
                old.level = rec.level;
            }
            old.status = rec.status;
        } else {
            // Kept as sent: the tracker treats a level above 100 as "no reading". Storing
            // 0 instead turned an impossible first report into a live, saved "0%".
            existing.push(rec.clone());
        }
    }
}

pub(super) fn ear_status_from_u8(v: u8) -> Option<EarDetectionStatus> {
    match v {
        0x00 => Some(EarDetectionStatus::InEar),
        0x01 => Some(EarDetectionStatus::OutOfEar),
        0x02 => Some(EarDetectionStatus::InCase),
        0x03 => Some(EarDetectionStatus::Disconnected),
        _ => None,
    }
}

/// Parse an EAR_DETECTION packet (full packet including header). Returns
/// (primary, secondary). A status byte we don't know is rejected rather than guessed:
/// reading it as "out of ear" paused media at random.
pub fn parse_ear_detection(packet: &[u8]) -> Option<(EarDetectionStatus, EarDetectionStatus)> {
    let primary = ear_status_from_u8(*packet.get(6)?)?;
    let secondary = ear_status_from_u8(*packet.get(7)?)?;
    Some((primary, secondary))
}

pub(super) fn empty_le_keys() -> AirPodsLEKeys {
    AirPodsLEKeys {
        irk: String::new(),
        enc_key: String::new(),
    }
}

pub(super) fn empty_airpods_information() -> AirPodsInformation {
    AirPodsInformation {
        name: String::new(),
        model_number: String::new(),
        manufacturer: String::new(),
        serial_number: String::new(),
        version1: String::new(),
        version2: String::new(),
        hardware_revision: String::new(),
        updater_identifier: String::new(),
        left_serial_number: String::new(),
        right_serial_number: String::new(),
        version3: String::new(),
        le_keys: empty_le_keys(),
    }
}

/// Parse an INFORMATION payload (starting at the opcode). The data after the
/// 4-byte sub-header is a binary prefix followed by NUL-separated tokens. Same
/// token selection as the original parser (and the Android app): skip the
/// prefix up to the first NUL, keep non-empty UTF-8 tokens, drop the first one
/// (a binary field), then the rest are name, model number, ... LE keys are empty.
pub fn parse_information(payload: &[u8]) -> Option<AirPodsInformation> {
    let data = payload.get(4..)?;
    if data.len() < 2 {
        return None;
    }
    let first_nul = data.iter().position(|&b| b == 0)?;
    let strings: Vec<String> = data[first_nul..]
        .split(|&b| b == 0)
        .filter(|s| !s.is_empty())
        .filter_map(|s| std::str::from_utf8(s).ok().map(str::to_owned))
        .skip(1)
        .collect();
    let get = |i: usize| strings.get(i).cloned().unwrap_or_default();
    Some(AirPodsInformation {
        name: get(0),
        model_number: get(1),
        manufacturer: get(2),
        serial_number: get(3),
        version1: get(4),
        version2: get(5),
        hardware_revision: get(6),
        updater_identifier: get(7),
        left_serial_number: get(8),
        right_serial_number: get(9),
        version3: get(10),
        le_keys: empty_le_keys(),
    })
}

/// Parse a PROXIMITY_KEYS_RSP payload (starting at the opcode):
/// `31 00 <count> { <type> 00 <len> 00 <key bytes...> }*count`.
pub fn parse_proximity_keys(payload: &[u8]) -> Option<Vec<(u8, Vec<u8>)>> {
    if payload.len() < 3 {
        return None;
    }
    let key_count = payload[2] as usize;
    let mut offset = 3usize;
    let mut keys = Vec::with_capacity(key_count);
    for _ in 0..key_count {
        let header = payload.get(offset..offset + 4)?;
        let key_type = header[0];
        let key_length = header[2] as usize;
        offset += 4;
        let key_data = payload.get(offset..offset + key_length)?;
        keys.push((key_type, key_data.to_vec()));
        offset += key_length;
    }
    Some(keys)
}

/// Parse "AA:BB:CC:DD:EE:FF" into bytes in over-the-air (reversed) order.
/// Smart Routing "media information": tells another device on the AirPods whether this
/// host is streaming media, which decides whether that device may take the audio route.
pub(super) fn media_information_packet(
    self_mac_address: &str,
    target_mac_address: &str,
    streaming_state: bool,
) -> Result<Vec<u8>> {
    let opcode = [opcodes::SMART_ROUTING, 0x00];
    let mut buffer = Vec::with_capacity(138);
    buffer.extend_from_slice(&mac_to_reversed_bytes(target_mac_address)?);
    buffer.extend_from_slice(&[0x82, 0x00]);
    buffer.extend_from_slice(&[0x01, 0xE5, 0x4A]);
    // Keys are lowercase camelCase, as an iPhone sends them.
    buffer.extend_from_slice(b"playingApp");
    buffer.push(0x56);
    buffer.extend_from_slice(b"com.google.ios.youtube");
    buffer.push(0x52);
    buffer.extend_from_slice(b"hostStreamingState");
    // OPACK strings carry their length in the type byte (0x40 + length), so "YES" needs
    // 0x43; upstream sent 0x42 for both, which made "YES" unreadable.
    let state: &[u8] = if streaming_state { b"YES" } else { b"NO" };
    buffer.push(0x40 + state.len() as u8);
    buffer.extend_from_slice(state);
    buffer.push(0x49);
    buffer.extend_from_slice(b"btAddress");
    buffer.push(0x51);
    buffer.extend_from_slice(self_mac_address.as_bytes());
    buffer.push(0x46);
    buffer.extend_from_slice(b"btName");
    buffer.push(0x43);
    buffer.extend_from_slice(b"Mac");
    buffer.push(0x58);
    buffer.extend_from_slice(b"otherDevice");
    buffer.extend_from_slice(b"AudioCategory");
    buffer.extend_from_slice(&[0x31, 0x2D, 0x01]);

    while buffer.len() < 138 {
        buffer.push(0x00);
    }
    Ok([opcode.as_slice(), buffer.as_slice()].concat())
}

/// Reads `hostStreamingState` (any casing) from a Smart Routing message another device
/// sent: `Some(true)` when it streams media from an app, `Some(false)` for NO, `None` when
/// the message has no such key. A YES for `playingApp` "Unknown" is ignored: an iPhone sends
/// it in a quick YES/NO pair around system sounds and when its own playback stops.
pub fn peer_streaming_state(packet: &str) -> Option<bool> {
    let lower = packet.to_ascii_lowercase();
    let key = "hoststreamingstate";
    let rest = &lower[lower.find(key)? + key.len()..];
    // The value follows the key after one OPACK type byte.
    let value: String = rest.chars().skip(1).take(3).collect();
    if value.starts_with("yes") {
        // "playingApp" then a 7-character string tag ("G") and "Unknown".
        (!lower.contains("playingappgunknown")).then_some(true)
    } else if value.starts_with("no") {
        Some(false)
    } else {
        None
    }
}

/// Reads `btAddress` and `btName` from a Smart Routing message another device sent, e.g.
/// ("10:A2:D3:BF:3D:76", "iPhone").
pub fn peer_identity(packet: &str) -> Option<(String, String)> {
    let mac = opack_string_after(packet, "btAddress")?;
    let name = opack_string_after(packet, "btName")?;
    (mac.len() == 17 && !name.is_empty()).then(|| (mac.to_ascii_uppercase(), name))
}

/// The OPACK short string that follows `key`: one type byte (0x40 + length), then the text.
pub(super) fn opack_string_after(packet: &str, key: &str) -> Option<String> {
    let rest = &packet[packet.find(key)? + key.len()..];
    let mut chars = rest.chars();
    let len = (chars.next()? as u32)
        .checked_sub(0x40)
        .filter(|&l| l > 0 && l < 0x20)?;
    let value: String = chars.take(len as usize).collect();
    (value.chars().count() == len as usize).then_some(value)
}

pub fn mac_to_reversed_bytes(mac: &str) -> Result<[u8; 6]> {
    let addr: Address = mac.trim().parse().map_err(|_| {
        Error::from(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("invalid MAC address: {mac:?}"),
        ))
    })?;
    let mut bytes = addr.0;
    bytes.reverse();
    Ok(bytes)
}

// ---------------------------------------------------------------------------
// devices.json persistence
// ---------------------------------------------------------------------------

pub(super) fn airpods_entry<'a>(
    devices: &'a mut HashMap<String, DeviceData>,
    mac: &str,
) -> &'a mut DeviceData {
    let entry = devices
        .entry(mac.to_string())
        .or_insert_with(|| DeviceData {
            name: mac.to_string(),
            type_: DeviceType::AirPods,
            information: None,
        });
    if !matches!(entry.information, Some(DeviceInformation::AirPods(_))) {
        entry.type_ = DeviceType::AirPods;
        entry.information = Some(DeviceInformation::AirPods(empty_airpods_information()));
    }
    entry
}

/// Store device information, creating the entry if needed. Existing non-empty
/// LE keys are never replaced by empty ones.
pub fn merge_information(
    devices: &mut HashMap<String, DeviceData>,
    mac: &str,
    mut info: AirPodsInformation,
) {
    let entry = airpods_entry(devices, mac);
    if !info.name.is_empty() {
        entry.name = info.name.clone();
    }
    if let Some(DeviceInformation::AirPods(old)) = &entry.information {
        if info.le_keys.irk.is_empty() {
            info.le_keys.irk = old.le_keys.irk.clone();
        }
        if info.le_keys.enc_key.is_empty() {
            info.le_keys.enc_key = old.le_keys.enc_key.clone();
        }
    }
    entry.information = Some(DeviceInformation::AirPods(info));
}

/// Store LE keys (hex), creating the entry if needed. `None`/empty values leave
/// the stored key untouched.
pub fn merge_le_keys(
    devices: &mut HashMap<String, DeviceData>,
    mac: &str,
    irk: Option<&str>,
    enc_key: Option<&str>,
) {
    let entry = airpods_entry(devices, mac);
    if let Some(DeviceInformation::AirPods(info)) = entry.information.as_mut() {
        if let Some(irk) = irk.filter(|k| !k.is_empty()) {
            info.le_keys.irk = irk.to_string();
        }
        if let Some(enc) = enc_key.filter(|k| !k.is_empty()) {
            info.le_keys.enc_key = enc.to_string();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_peer_streaming_state() {
        let yes = "v=\u{fffd}JplayingAppVcom.google.ios.youtubeRhostStreamingStateCYESIbtAddressQ10:A2:D3:BF:3D:76FbtNameFiPhone";
        let no = "JplayingAppGUnknownRhostStreamingStateBNOIbtAddressQ10:A2:D3:BF:3D:76";
        let blip = "JplayingAppGUnknownRhostStreamingStateCYESIbtAddressQ10:A2:D3:BF:3D:76";
        assert_eq!(peer_streaming_state(yes), Some(true));
        assert_eq!(peer_streaming_state(no), Some(false));
        assert_eq!(peer_streaming_state(blip), None);
        assert_eq!(
            peer_identity(yes),
            Some(("10:A2:D3:BF:3D:76".into(), "iPhone".into()))
        );
        assert_eq!(peer_identity("btNameFiPh"), None);
        assert_eq!(
            peer_streaming_state("audioRoutingSetOwnershipToFalse"),
            None
        );
    }

    #[test]
    fn media_information_is_tagged_like_an_iphone_sends_it() {
        let find = |hay: &[u8], needle: &[u8]| hay.windows(needle.len()).any(|w| w == needle);
        let local = "E8:65:38:93:3D:9E";
        let phone = "10:A2:D3:BF:3D:76";
        let yes = media_information_packet(local, phone, true).unwrap();
        assert!(find(
            &yes,
            b"RhostStreamingStateCYESIbtAddressQE8:65:38:93:3D:9EFbtNameCMac"
        ));
        let no = media_information_packet(local, phone, false).unwrap();
        assert!(find(&no, b"RhostStreamingStateBNOIbtAddress"));
        assert_eq!(yes.len(), 140);
        // Our own message reads back as streaming, like the iPhone's do.
        assert_eq!(
            peer_streaming_state(&String::from_utf8_lossy(&yes)),
            Some(true)
        );
    }

    fn bi(component: BatteryComponent, level: u8, status: BatteryStatus) -> BatteryInfo {
        BatteryInfo {
            component,
            level,
            status,
        }
    }

    /// Full packet as received (header + payload) -> payload slice like receive_packet.
    fn payload(packet: &[u8]) -> &[u8] {
        &packet[4..]
    }

    #[test]
    fn battery_three_components() {
        let pkt = [
            0x04, 0x00, 0x04, 0x00, 0x04, 0x00, 0x03, //
            0x02, 0x01, 0x64, 0x02, 0x01, // right 100 not charging
            0x04, 0x01, 0x63, 0x01, 0x01, // left 99 charging
            0x08, 0x01, 0x11, 0x02, 0x01, // case 17
        ];
        let b = parse_battery_info(payload(&pkt)).unwrap();
        assert_eq!(
            b,
            vec![
                bi(BatteryComponent::Right, 100, BatteryStatus::NotCharging),
                bi(BatteryComponent::Left, 99, BatteryStatus::Charging),
                bi(BatteryComponent::Case, 17, BatteryStatus::NotCharging),
            ]
        );
    }

    #[test]
    fn battery_unknown_status_kept_and_unknown_component_skipped() {
        let pkt = [
            0x04, 0x00, 0x04, 0x00, 0x04, 0x00, 0x03, //
            0x08, 0x01, 0x32, 0x00, 0x01, // case status 0x00 (Unknown)
            0x10, 0x01, 0x32, 0x02, 0x01, // unknown component 0x10
            0x04, 0x01, 0x00, 0x04, 0x01, // left disconnected
        ];
        let b = parse_battery_info(payload(&pkt)).unwrap();
        assert_eq!(
            b,
            vec![
                bi(BatteryComponent::Case, 50, BatteryStatus::Unknown),
                bi(BatteryComponent::Left, 0, BatteryStatus::Disconnected),
            ]
        );
    }

    #[test]
    fn battery_headphone() {
        let pkt = [
            0x04, 0x00, 0x04, 0x00, 0x04, 0x00, 0x01, 0x01, 0x01, 0x40, 0x01, 0x01,
        ];
        let b = parse_battery_info(payload(&pkt)).unwrap();
        assert_eq!(
            b,
            vec![bi(BatteryComponent::Headphone, 64, BatteryStatus::Charging)]
        );
    }

    #[test]
    fn battery_rejects_bad_packets() {
        // count says 2 but only one record
        let short = [
            0x04, 0x00, 0x04, 0x00, 0x04, 0x00, 0x02, 0x02, 0x01, 0x64, 0x02, 0x01,
        ];
        assert!(parse_battery_info(payload(&short)).is_none());
        // bad spacer
        let spacer = [
            0x04, 0x00, 0x04, 0x00, 0x04, 0x00, 0x01, 0x02, 0x00, 0x64, 0x02, 0x01,
        ];
        assert!(parse_battery_info(payload(&spacer)).is_none());
        // implausible count
        let mut big = vec![0x04, 0x00, 0x04, 0x00, 0x04, 0x00, 0x09];
        big.extend(std::iter::repeat_n([0x02, 0x01, 0x10, 0x02, 0x01], 9).flatten());
        assert!(parse_battery_info(payload(&big)).is_none());
        // truncated / empty
        assert!(parse_battery_info(&[]).is_none());
        assert!(parse_battery_info(&[0x04, 0x00]).is_none());
        // wrong opcode
        assert!(parse_battery_info(&[0x05, 0x00, 0x00]).is_none());
    }

    #[test]
    fn merge_keeps_components_and_levels() {
        let mut acc = Vec::new();
        merge_battery_info(
            &mut acc,
            &[
                bi(BatteryComponent::Left, 80, BatteryStatus::NotCharging),
                bi(BatteryComponent::Right, 70, BatteryStatus::NotCharging),
                bi(BatteryComponent::Case, 50, BatteryStatus::Charging),
            ],
        );
        merge_battery_info(
            &mut acc,
            &[
                bi(BatteryComponent::Left, 0, BatteryStatus::Disconnected),
                bi(BatteryComponent::Right, 69, BatteryStatus::NotCharging),
            ],
        );
        assert_eq!(acc.len(), 3);
        let get = |c| acc.iter().find(|b| b.component == c).unwrap().clone();
        assert_eq!(
            get(BatteryComponent::Left),
            bi(BatteryComponent::Left, 80, BatteryStatus::Disconnected)
        );
        assert_eq!(get(BatteryComponent::Right).level, 69);
        assert_eq!(get(BatteryComponent::Case).level, 50);
    }

    #[test]
    fn ear_detection_bounds() {
        assert!(parse_ear_detection(&[0x04, 0x00, 0x04, 0x00, 0x06, 0x00]).is_none());
        assert!(parse_ear_detection(&[0x04, 0x00, 0x04, 0x00, 0x06, 0x00, 0x00]).is_none());
        // Unknown status bytes (e.g. from newer firmware) are ignored, not read as "out".
        assert!(parse_ear_detection(&[0x04, 0x00, 0x04, 0x00, 0x06, 0x00, 0x04, 0x04]).is_none());
        let (p, s) =
            parse_ear_detection(&[0x04, 0x00, 0x04, 0x00, 0x06, 0x00, 0x00, 0x02]).unwrap();
        assert_eq!(p, EarDetectionStatus::InEar);
        assert_eq!(s, EarDetectionStatus::InCase);
    }

    fn info_payload(strings: &[&str]) -> Vec<u8> {
        // sub-header, binary prefix, NUL, a leading token that is discarded
        let mut v = vec![0x1D, 0x00, 0x00, 0x00, 0xAB, 0xCD, 0x00, 0x02, 0x00];
        for s in strings {
            v.extend_from_slice(s.as_bytes());
            v.push(0x00);
        }
        v
    }

    #[test]
    fn information_parse() {
        let p = info_payload(&["David's AirPods", "A3056", "Apple Inc.", "SERIAL1"]);
        let info = parse_information(&p).unwrap();
        assert_eq!(info.name, "David's AirPods");
        assert_eq!(info.model_number, "A3056");
        assert_eq!(info.manufacturer, "Apple Inc.");
        assert_eq!(info.serial_number, "SERIAL1");
        assert_eq!(info.version3, "");
        assert!(info.le_keys.irk.is_empty());
    }

    #[test]
    fn information_no_panic_on_empty() {
        // Previously `strings.remove(0)` panicked here.
        assert!(parse_information(&[0x1D, 0x00, 0x00, 0x00, 0x00, 0x00]).is_some());
        assert!(parse_information(&[0x1D, 0x00, 0x00, 0x00, 0xAA, 0xBB]).is_none());
        assert!(parse_information(&[0x1D]).is_none());
        let info = parse_information(&[0x1D, 0x00, 0x00, 0x00, 0x00, 0x00]).unwrap();
        assert_eq!(info.name, "");
    }

    #[test]
    fn proximity_keys_parse() {
        let mut p = vec![0x31, 0x00, 0x02];
        p.extend_from_slice(&[0x01, 0x00, 0x10, 0x00]);
        p.extend_from_slice(&[0x11; 16]);
        p.extend_from_slice(&[0x04, 0x00, 0x10, 0x00]);
        p.extend_from_slice(&[0x22; 16]);
        let keys = parse_proximity_keys(&p).unwrap();
        assert_eq!(keys, vec![(0x01, vec![0x11; 16]), (0x04, vec![0x22; 16])]);
        // truncated key data
        assert!(parse_proximity_keys(&p[..p.len() - 1]).is_none());
        assert!(parse_proximity_keys(&[0x31, 0x00, 0x01, 0x01, 0x00]).is_none());
    }

    #[test]
    fn mac_parsing() {
        assert_eq!(
            mac_to_reversed_bytes("AA:BB:CC:DD:EE:FF").unwrap(),
            [0xFF, 0xEE, 0xDD, 0xCC, 0xBB, 0xAA]
        );
        assert!(mac_to_reversed_bytes("").is_err());
        assert!(mac_to_reversed_bytes("ZZ:BB:CC:DD:EE:FF").is_err());
        assert!(mac_to_reversed_bytes("AA:BB").is_err());
    }

    fn le_keys_of(devices: &HashMap<String, DeviceData>, mac: &str) -> (String, String) {
        match &devices[mac].information {
            Some(DeviceInformation::AirPods(i)) => {
                (i.le_keys.irk.clone(), i.le_keys.enc_key.clone())
            }
            _ => panic!("no airpods info"),
        }
    }

    #[test]
    fn keys_then_information_keeps_keys() {
        let mac = "AA:BB:CC:DD:EE:FF";
        let mut devices = HashMap::new();
        merge_le_keys(&mut devices, mac, Some("aa"), Some("bb"));
        let info = parse_information(&info_payload(&["Pods", "A2698"])).unwrap();
        merge_information(&mut devices, mac, info);
        assert_eq!(le_keys_of(&devices, mac), ("aa".into(), "bb".into()));
        assert_eq!(devices[mac].name, "Pods");
        match &devices[mac].information {
            Some(DeviceInformation::AirPods(i)) => assert_eq!(i.model_number, "A2698"),
            _ => panic!(),
        }
    }

    #[test]
    fn information_then_keys_and_no_clobber() {
        let mac = "AA:BB:CC:DD:EE:FF";
        let mut devices = HashMap::new();
        let info = parse_information(&info_payload(&["Pods", "A2698"])).unwrap();
        merge_information(&mut devices, mac, info.clone());
        merge_le_keys(&mut devices, mac, Some("aa"), None);
        merge_le_keys(&mut devices, mac, None, Some("bb"));
        merge_le_keys(&mut devices, mac, Some(""), Some(""));
        assert_eq!(le_keys_of(&devices, mac), ("aa".into(), "bb".into()));
        // A later INFORMATION packet must not wipe keys.
        merge_information(&mut devices, mac, info);
        assert_eq!(le_keys_of(&devices, mac), ("aa".into(), "bb".into()));
    }

    #[tokio::test]
    async fn devices_file_roundtrip_atomic() {
        let dir = std::env::temp_dir().join(format!(
            "librepods-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let path = dir.join("nested").join("devices.json");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        let mac = "AA:BB:CC:DD:EE:FF";
        crate::store::update_devices_at(&path, |d| merge_le_keys(d, mac, Some("aa"), Some("bb")))
            .await
            .unwrap();
        let info = parse_information(&info_payload(&["Pods", "A2698"])).unwrap();
        let devices = crate::store::update_devices_at(&path, |d| merge_information(d, mac, info))
            .await
            .unwrap();
        assert_eq!(le_keys_of(&devices, mac), ("aa".into(), "bb".into()));
        let on_disk: HashMap<String, DeviceData> =
            serde_json::from_str(&tokio::fs::read_to_string(&path).await.unwrap()).unwrap();
        assert_eq!(le_keys_of(&on_disk, mac), ("aa".into(), "bb".into()));
        // no temp file left behind
        let mut entries = tokio::fs::read_dir(path.parent().unwrap()).await.unwrap();
        let mut names = Vec::new();
        while let Some(e) = entries.next_entry().await.unwrap() {
            names.push(e.file_name());
        }
        assert_eq!(names, vec![std::ffi::OsString::from("devices.json")]);
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn receive_packet_never_panics_on_short_input() {
        let m = AACPManager::new();
        for op in [
            opcodes::BATTERY_INFO,
            opcodes::CONTROL_COMMAND,
            opcodes::EAR_DETECTION,
            opcodes::CONVERSATION_AWARENESS,
            opcodes::STEM_PRESS,
            opcodes::AUDIO_SOURCE,
            opcodes::CONNECTED_DEVICES,
            opcodes::SMART_ROUTING_RESP,
            opcodes::EQ_DATA,
        ] {
            for extra in 0..12 {
                let mut p = vec![0x04, 0x00, 0x04, 0x00, op];
                p.extend(std::iter::repeat_n(0x05u8, extra));
                m.receive_packet(&p).await;
            }
        }
        // connected devices: count 1 but only 8 bytes after count (needs 10)
        let p = [
            0x04, 0x00, 0x04, 0x00, 0x2E, 0x00, 0x01, 1, 2, 3, 4, 5, 6, 7, 8,
        ];
        m.receive_packet(&p).await;
    }
}
