//! Reading the audio server snapshot: which card, sink and streams belong to the AirPods.

use super::*;

const MAC: &str = "A0:B1:C2:D3:E4:F5";

fn card(index: u32, mac: Option<&str>) -> CardInfo {
    CardInfo {
        index,
        name: format!("card{index}"),
        mac: mac.map(str::to_string),
        profiles: vec![],
        active_profile: None,
    }
}

fn sink(index: u32, card: Option<u32>, mac: Option<&str>) -> SinkInfo {
    SinkInfo {
        index,
        name: format!("sink{index}"),
        card,
        mac: mac.map(str::to_string),
        state: SinkState::Idle,
        codec: None,
    }
}

fn input(index: u32, sink: u32, corked: bool, app: Option<&str>) -> SinkInputInfo {
    SinkInputInfo {
        index,
        sink,
        corked,
        app: app.map(str::to_string),
    }
}

#[test]
fn card_lookup_ignores_case() {
    let snap = PulseSnapshot {
        cards: vec![card(1, None), card(2, Some(MAC))],
        ..Default::default()
    };
    assert_eq!(snap.card_for(&MAC.to_lowercase()).map(|c| c.index), Some(2));
    assert!(snap.card_for("00:00:00:00:00:00").is_none());
}

#[test]
fn sink_found_by_card_index_or_address() {
    let ours = card(2, Some(MAC));
    let by_index = PulseSnapshot {
        sinks: vec![sink(1, Some(1), None), sink(5, Some(2), None)],
        ..Default::default()
    };
    assert_eq!(by_index.sink_for_card(&ours).map(|s| s.index), Some(5));

    // Some PipeWire versions publish the sink without a card index.
    let by_mac = PulseSnapshot {
        sinks: vec![
            sink(1, None, None),
            sink(7, None, Some(&MAC.to_lowercase())),
        ],
        ..Default::default()
    };
    assert_eq!(by_mac.sink_for_card(&ours).map(|s| s.index), Some(7));
    assert!(by_mac.sink_for_card(&card(3, None)).is_none());
    assert_eq!(by_mac.sink_by_name("sink7").map(|s| s.index), Some(7));
    assert!(by_mac.sink_by_name("nope").is_none());
}

#[test]
fn playing_streams_skip_paused_and_background_apps() {
    let snap = PulseSnapshot {
        sink_inputs: vec![
            input(1, 9, false, Some("Plex")),
            input(2, 9, true, Some("Brave")),
            input(3, 9, false, Some("EasyEffects")),
            input(4, 9, false, Some("WEBRTC VoiceEngine")),
            input(5, 9, false, None),
            input(6, 8, false, Some("PulseEffects")),
        ],
        ..Default::default()
    };
    assert_eq!(snap.playing_streams(), vec![1, 5]);
    assert_eq!(snap.uncorked_inputs_on(9), 4);
    assert_eq!(snap.uncorked_inputs_on(8), 1);
    assert_eq!(snap.uncorked_inputs_on(1), 0);
}

#[test]
fn mac_formats_and_sources() {
    assert_eq!(normalize_mac("a0-b1-c2-d3-e4-f5").as_deref(), Some(MAC));
    assert_eq!(normalize_mac("A0:B1:C2:D3:E4"), None);
    assert_eq!(normalize_mac("A0:B1:C2:D3:E4:F5x").as_deref(), Some(MAC));
    assert_eq!(normalize_mac("A0.B1:C2:D3:E4:F5"), None);
    assert_eq!(normalize_mac("é0:B1:C2:D3:E4:F5"), None);

    // Property order: bluez address, then device.string, then the path, then the name.
    let props = |k: &str| match k {
        "api.bluez5.address" => Some("11:11:11:11:11:11".to_string()),
        "device.string" => Some("22:22:22:22:22:22".to_string()),
        _ => None,
    };
    assert_eq!(
        bluez_mac_from("bluez_card.33_33_33_33_33_33", props).as_deref(),
        Some("11:11:11:11:11:11")
    );
    let device_string = |k: &str| (k == "device.string").then(|| MAC.to_string());
    assert_eq!(bluez_mac_from("x", device_string).as_deref(), Some(MAC));
    let old_path =
        |k: &str| (k == "bluez.path").then(|| "/org/bluez/hci1/dev_A0_B1_C2_D3_E4_F5".to_string());
    assert_eq!(bluez_mac_from("x", old_path).as_deref(), Some(MAC));
    for name in [
        "bluez_input.A0_B1_C2_D3_E4_F5.0",
        "bluez_sink.A0_B1_C2_D3_E4_F5.a2dp_sink",
        "bluez_source.A0_B1_C2_D3_E4_F5",
    ] {
        assert_eq!(
            bluez_mac_from(name, |_| None).as_deref(),
            Some(MAC),
            "{name}"
        );
    }
    assert_eq!(bluez_mac_from("bluez_card.garbage", |_| None), None);
}

#[test]
fn errors_read_well_in_logs() {
    assert!(!PulseError::Connect("refused".into()).to_string().is_empty());
    assert!(!PulseError::Timeout("snapshot").to_string().is_empty());
}
