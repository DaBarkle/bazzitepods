use super::*;
use crate::core::battery::BatterySnapshot;

fn comp(level: u8, charging: bool) -> ComponentBattery {
    ComponentBattery {
        level: Some(level),
        charging,
        present: true,
        stale: false,
        last_seen: Some(SystemTime::now()),
    }
}

fn device(link: LinkState) -> DeviceSnapshot {
    let mut d = DeviceSnapshot::new("AA".into(), "David's AirPods".into());
    d.model = ModelKind::AirPods4Anc;
    d.link = link;
    d.battery = BatterySnapshot {
        left: comp(80, false),
        right: comp(75, false),
        case: comp(40, true),
        ..Default::default()
    };
    d
}

#[test]
fn connected_shows_listening_mode_or_codec() {
    let now = SystemTime::now();
    let mut d = device(LinkState::Connected);
    d.audio = Some("AAC".into());
    let s = status(&d, now);
    assert_eq!(s.headline, "Connected");
    assert_eq!(s.detail.as_deref(), Some("AAC"));
    assert_eq!(s.tone, StatusTone::Good);
    d.listening_mode = Some(0x02);
    assert_eq!(
        status(&d, now).detail.as_deref(),
        Some("Noise Cancellation")
    );
}

#[test]
fn low_pod_beats_connected() {
    let now = SystemTime::now();
    let mut d = device(LinkState::Connected);
    d.battery.right = comp(18, false);
    let s = status(&d, now);
    assert_eq!(s.headline, "Right AirPod low");
    assert_eq!(s.detail.as_deref(), Some("18%"));
    assert_eq!(s.tone, StatusTone::Low);

    d.battery.left = comp(7, false);
    let s = status(&d, now);
    assert_eq!(s.headline, "AirPods low");
    assert_eq!(s.detail.as_deref(), Some("7%"));
    assert_eq!(s.tone, StatusTone::Critical);

    // A charging pod or a low case is not worth the alarm.
    d.battery.left = comp(7, true);
    d.battery.right = comp(80, false);
    d.battery.case = comp(5, false);
    assert_eq!(status(&d, now).headline, "Connected");
}

#[test]
fn single_battery_low() {
    let now = SystemTime::now();
    let mut d = device(LinkState::Connected);
    d.model = ModelKind::AirPodsMax;
    d.battery.headphone = comp(15, false);
    assert_eq!(status(&d, now).headline, "Battery low");
}

#[test]
fn elsewhere_and_busy_and_away() {
    let now = SystemTime::now();
    let mut d = device(LinkState::Connected);
    d.owns_audio = Some(false);
    d.other_devices = 1;
    d.elsewhere_name = Some("iPhone".into());
    let s = status(&d, now);
    assert_eq!(s.headline, "Playing on iPhone");
    assert_eq!(s.tone, StatusTone::Info);
    d.elsewhere_name = None;
    assert_eq!(status(&d, now).headline, "Playing on another device");

    let d = device(LinkState::Connecting);
    assert_eq!(status(&d, now).tone, StatusTone::Busy);

    let mut d = device(LinkState::Disconnected);
    for c in [
        &mut d.battery.left,
        &mut d.battery.right,
        &mut d.battery.case,
    ] {
        c.stale = true;
        c.present = false;
    }
    d.last_seen = now.checked_sub(Duration::from_secs(20 * 60));
    let s = status(&d, now);
    assert_eq!(s.headline, "Not connected");
    assert_eq!(s.detail.as_deref(), Some("seen 20 min ago"));
    assert_eq!(s.tone, StatusTone::Idle);
}

#[test]
fn nearby_has_no_age() {
    let now = SystemTime::now();
    let mut d = device(LinkState::Disconnected);
    d.last_seen = Some(now);
    let s = status(&d, now);
    assert_eq!(s.headline, "Nearby");
    assert_eq!(s.detail, None);
    assert_eq!(s.tone, StatusTone::Info);
}
