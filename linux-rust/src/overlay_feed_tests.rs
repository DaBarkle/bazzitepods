//! What the in-game overlay (and the notification fallback) shows for a given hub state.

use super::*;
use crate::core::battery::BatterySnapshot;
use crate::core::models::ModelKind;
use crate::overlay_protocol::OverlayPosition;
use std::time::Duration;

fn live(level: u8) -> ComponentBattery {
    ComponentBattery {
        level: Some(level),
        present: true,
        last_seen: Some(SystemTime::now()),
        ..Default::default()
    }
}

fn airpods4(left: u8, right: u8, case: u8) -> DeviceSnapshot {
    let mut d = DeviceSnapshot::new("AA:BB:CC:DD:EE:FF".into(), "AirPods".into());
    d.link = LinkState::Connected;
    d.model = ModelKind::AirPods4;
    d.battery = BatterySnapshot {
        left: live(left),
        right: live(right),
        case: live(case),
        ..Default::default()
    };
    d
}

fn snap_of(devices: Vec<DeviceSnapshot>) -> AppSnapshot {
    let mut snap = AppSnapshot::default();
    for d in devices {
        snap.devices.insert(d.mac.clone(), d);
    }
    snap
}

// ---------------------------------------------------------------------------
// Overlay state
// ---------------------------------------------------------------------------

#[test]
fn shows_each_pod_and_the_case_separately() {
    let s = to_overlay_state(&snap_of(vec![airpods4(100, 35, 62)])).unwrap();
    assert_eq!(s.left.as_ref().map(|c| c.level), Some(100));
    assert_eq!(s.right.as_ref().map(|c| c.level), Some(35));
    assert_eq!(s.case.as_ref().map(|c| c.level), Some(62));
    assert!(s.left.as_ref().unwrap().present);
    assert!(!s.elsewhere);
}

#[test]
fn stale_values_carry_their_age_and_fresh_ones_do_not() {
    let mut d = airpods4(80, 80, 40);
    d.battery.case.stale = true;
    d.battery.case.present = false;
    d.battery.case.last_seen = Some(SystemTime::now() - Duration::from_secs(125));
    let s = to_overlay_state(&snap_of(vec![d])).unwrap();
    let case = s.case.unwrap();
    assert!(case.stale && !case.present);
    let age = case.last_seen_secs.unwrap();
    assert!((125..130).contains(&age), "age {age}");
    assert_eq!(s.left.unwrap().last_seen_secs, None);

    // A last-seen time in the future (clock change) gives no age instead of a panic.
    let mut d = airpods4(80, 80, 40);
    d.battery.case.stale = true;
    d.battery.case.last_seen = Some(SystemTime::now() + Duration::from_secs(3600));
    let s = to_overlay_state(&snap_of(vec![d])).unwrap();
    assert_eq!(s.case.unwrap().last_seen_secs, None);
}

#[test]
fn never_seen_components_are_left_out() {
    let mut d = airpods4(80, 80, 40);
    d.battery.case = ComponentBattery::default();
    let s = to_overlay_state(&snap_of(vec![d])).unwrap();
    assert!(s.case.is_none());
    assert!(s.left.is_some());
}

#[test]
fn airpods_max_shows_one_battery_and_no_case() {
    let mut d = DeviceSnapshot::new("11:22:33:44:55:66".into(), "Max".into());
    d.link = LinkState::Connected;
    d.model = ModelKind::AirPodsMax;
    d.battery.headphone = live(57);
    d.battery.case = live(99); // never shown for a headphone without a case battery
    let s = to_overlay_state(&snap_of(vec![d.clone()])).unwrap();
    assert_eq!(s.kind, "airpodsmax");
    assert_eq!(s.headphone.unwrap().level, 57);
    assert!(s.case.is_none());
    assert_eq!(s.name, "Max");
    assert_eq!(battery_line(&d), "Battery 57%");
}

#[test]
fn headphone_battery_is_ignored_for_earbuds() {
    let mut d = airpods4(50, 50, 50);
    d.battery.headphone = live(10);
    assert!(
        to_overlay_state(&snap_of(vec![d]))
            .unwrap()
            .headphone
            .is_none()
    );
}

#[test]
fn shows_the_connected_device_over_a_more_recent_disconnected_one() {
    let mut old = airpods4(10, 10, 10);
    old.mac = "00:00:00:00:00:01".into();
    old.link = LinkState::Disconnected;
    old.last_seen = Some(SystemTime::now());
    let mut current = airpods4(90, 90, 90);
    current.last_seen = Some(SystemTime::now() - Duration::from_secs(600));
    let s = to_overlay_state(&snap_of(vec![old, current])).unwrap();
    assert!(s.connected);
    assert_eq!(s.left.unwrap().level, 90);
}

#[test]
fn disconnected_device_is_shown_as_disconnected() {
    let mut d = airpods4(50, 50, 50);
    d.link = LinkState::Disconnected;
    let s = to_overlay_state(&snap_of(vec![d])).unwrap();
    assert!(!s.connected);
    assert!(!s.elsewhere);
}

#[test]
fn audio_on_another_device_is_flagged() {
    let mut d = airpods4(50, 50, 50);
    d.owns_audio = Some(false);
    assert!(
        to_overlay_state(&snap_of(vec![d.clone()]))
            .unwrap()
            .elsewhere
    );
    // Reconnecting still counts as connected here.
    d.link = LinkState::Reconnecting { attempt: 2 };
    assert!(
        to_overlay_state(&snap_of(vec![d.clone()]))
            .unwrap()
            .elsewhere
    );
    d.owns_audio = Some(true);
    assert!(!to_overlay_state(&snap_of(vec![d])).unwrap().elsewhere);
}

#[test]
fn nothing_to_show_without_devices() {
    assert!(to_overlay_state(&AppSnapshot::default()).is_none());
}

#[test]
fn names_prefer_the_users_name_over_the_model() {
    let mut d = airpods4(50, 50, 50);
    assert_eq!(display_name(&d), "AirPods 4");
    d.name = "airpods".into();
    assert_eq!(display_name(&d), "AirPods 4");
    d.name = String::new();
    assert_eq!(display_name(&d), "AirPods 4");
    d.name = "David's AirPods".into();
    assert_eq!(display_name(&d), "David's AirPods");
    d.name = "AirPods".into();
    d.model = ModelKind::Unknown;
    assert_eq!(display_name(&d), "AirPods");
}

#[test]
fn battery_line_marks_charging_and_missing_parts() {
    let mut d = airpods4(100, 9, 50);
    d.battery.left.charging = true;
    assert_eq!(battery_line(&d), "L 100% ⚡  ·  R 9%  ·  Case 50%");
    d.battery.case.present = false;
    assert_eq!(battery_line(&d), "L 100% ⚡  ·  R 9%  ·  Case —");
    d.battery.right = ComponentBattery::default();
    assert_eq!(battery_line(&d), "L 100% ⚡  ·  R —  ·  Case —");
}

// ---------------------------------------------------------------------------
// Overlay settings
// ---------------------------------------------------------------------------

#[test]
fn overlay_config_defaults_to_compact() {
    for json in ["", "garbage", "[]", "{}"] {
        let cfg = config_from_json(json);
        assert_eq!(cfg.mode, OverlayMode::Compact, "for {json:?}");
        assert_eq!(cfg.position, OverlayPosition::TopCenter);
        assert_eq!(cfg.opacity, 1.0);
    }
}

#[test]
fn overlay_config_reads_and_clamps_values() {
    let cfg = config_from_json(
        r#"{"overlay_mode":"always","overlay_position":"bottom-center",
            "overlay_opacity":0.05,"overlay_glass":3.5}"#,
    );
    assert_eq!(cfg.mode, OverlayMode::Always);
    assert_eq!(cfg.position, OverlayPosition::BottomCenter);
    assert_eq!(cfg.opacity, 0.2);
    assert_eq!(cfg.glass, 1.0);

    let cfg = config_from_json(
        r#"{"overlay_mode":"sideways","overlay_position":7,"overlay_opacity":"x","overlay_glass":-1}"#,
    );
    assert_eq!(cfg.mode, OverlayMode::Compact);
    assert_eq!(cfg.position, OverlayPosition::TopCenter);
    assert_eq!(cfg.opacity, 1.0);
    assert_eq!(cfg.glass, 0.0);

    assert_eq!(
        config_from_json(r#"{"overlay_mode":"off"}"#).mode,
        OverlayMode::Off
    );
}

#[test]
fn overlay_config_reads_style_and_size() {
    use crate::overlay_protocol::{OverlaySize, OverlayStyle};
    let cfg = config_from_json(r#"{"overlay_style":"classic","overlay_size":"extra_large"}"#);
    assert_eq!(cfg.style, OverlayStyle::Classic);
    assert_eq!(cfg.size, OverlaySize::ExtraLarge);
    let cfg = config_from_json(r#"{"overlay_style":3,"overlay_size":"huge"}"#);
    assert_eq!(cfg.style, OverlayStyle::Purist);
    assert_eq!(cfg.size, OverlaySize::Medium);
}

// ---------------------------------------------------------------------------
// Notification fallback (desktops without an overlay)
// ---------------------------------------------------------------------------

fn summaries(n: Vec<(String, String)>) -> Vec<String> {
    n.into_iter().map(|(s, _)| s).collect()
}

#[test]
fn notifies_once_on_connect() {
    let mut n = Notifier::default();
    let d = airpods4(80, 80, 50);
    let first = n.observe(Some(&d));
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].0, "AirPods 4 connected");
    assert_eq!(first[0].1, "L 80%  ·  R 80%  ·  Case 50%");
    assert!(n.observe(Some(&d)).is_empty());
}

#[test]
fn control_channel_blips_do_not_renotify() {
    let mut n = Notifier::default();
    let mut d = airpods4(80, 80, 50);
    n.observe(Some(&d));
    for link in [
        LinkState::Reconnecting { attempt: 1 },
        LinkState::Limited,
        LinkState::Connecting,
    ] {
        d.link = link;
        assert!(n.observe(Some(&d)).is_empty());
    }
    d.link = LinkState::Connected;
    assert!(n.observe(Some(&d)).is_empty());
}

#[test]
fn real_disconnect_then_reconnect_notifies_again() {
    let mut n = Notifier::default();
    let mut d = airpods4(80, 80, 50);
    n.observe(Some(&d));
    d.link = LinkState::Disconnected;
    assert!(n.observe(Some(&d)).is_empty());
    d.link = LinkState::Connected;
    assert_eq!(summaries(n.observe(Some(&d))), ["AirPods 4 connected"]);
    // No device at all also resets.
    n.observe(None);
    assert_eq!(n.observe(Some(&d)).len(), 1);
}

#[test]
fn waits_for_a_battery_reading_before_announcing() {
    let mut n = Notifier::default();
    let mut d = airpods4(80, 80, 50);
    d.battery = BatterySnapshot::default();
    assert!(n.observe(Some(&d)).is_empty());
    d.battery.left = live(80);
    assert_eq!(n.observe(Some(&d)).len(), 1);
}

#[test]
fn low_battery_warns_once_until_charged() {
    let mut n = Notifier::default();
    let mut d = airpods4(14, 80, 50);
    assert_eq!(
        summaries(n.observe(Some(&d))),
        ["AirPods 4 connected", "AirPods 4 battery low"]
    );
    d.battery.left.level = Some(12);
    assert!(n.observe(Some(&d)).is_empty(), "no repeat while still low");

    // Charging is not "low"; dropping again afterwards warns again.
    d.battery.left.charging = true;
    assert!(n.observe(Some(&d)).is_empty());
    d.battery.left.charging = false;
    assert_eq!(summaries(n.observe(Some(&d))), ["AirPods 4 battery low"]);
}

#[test]
fn switching_to_another_pair_announces_it() {
    let mut n = Notifier::default();
    n.observe(Some(&airpods4(80, 80, 50)));
    let mut other = airpods4(70, 70, 70);
    other.mac = "11:11:11:11:11:11".into();
    other.name = "Gym Pods".into();
    assert_eq!(summaries(n.observe(Some(&other))), ["Gym Pods connected"]);
}

#[test]
fn listening_mode_is_sent_only_for_models_with_noise_control() {
    use crate::overlay_protocol::ListeningMode;
    let mut d = airpods4(80, 80, 40);
    d.model = ModelKind::AirPods4Anc;
    d.listening_mode = Some(0x02);
    let s = to_overlay_state(&snap_of(vec![d.clone()])).unwrap();
    assert_eq!(s.listening_mode, Some(ListeningMode::NoiseCancellation));
    d.model = ModelKind::AirPods4;
    let s = to_overlay_state(&snap_of(vec![d])).unwrap();
    assert_eq!(s.listening_mode, None);
}
