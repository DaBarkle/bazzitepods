//! Battery scenarios end to end: raw AACP packets and BLE adverts go through the real
//! parsers into the tracker, and the snapshot is checked the way the overlay reads it.

use super::*;
use crate::bluetooth::aacp::{merge_battery_info, parse_battery_info};
use crate::bluetooth::le::decode_proximity;

const LEFT: u8 = 0x04;
const RIGHT: u8 = 0x02;
const CASE: u8 = 0x08;
const CHARGING: u8 = 0x01;
const NOT_CHARGING: u8 = 0x02;
const GONE: u8 = 0x04;

/// Feeds AACP battery packets the way the device task does: parse, merge, then apply.
struct Aacp {
    merged: Vec<BatteryInfo>,
}

impl Aacp {
    fn new() -> Self {
        Self { merged: Vec::new() }
    }

    fn send(&mut self, t: &mut BatteryTracker, records: &[(u8, u8, u8)], now: Instant) {
        let mut payload = vec![0x04, 0x00, records.len() as u8];
        for (component, level, status) in records {
            payload.extend_from_slice(&[*component, 0x01, *level, *status, 0x01]);
        }
        let parsed = parse_battery_info(&payload).expect("valid battery packet");
        merge_battery_info(&mut self.merged, &parsed);
        t.apply_aacp(&self.merged, now);
    }
}

fn setup() -> (BatteryTracker, Instant, SystemTime) {
    let t0 = Instant::now();
    let w0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
    (BatteryTracker::with_anchor(t0, w0), t0, w0)
}

fn secs(t0: Instant, s: u64) -> Instant {
    t0 + Duration::from_secs(s)
}

/// An AirPods 4 (0x1920) proximity advert with an already-decrypted block.
/// `status` uses the advert bit layout (bit5 primary left, bit6 this pod in case,
/// bit4 one in case, bit2 both in case, bits 1/3 in ear).
fn advert(status: u8, primary: u8, secondary: u8, case: u8) -> BleBatteryReading {
    let header = [
        0x07, 0x19, 0x01, 0x19, 0x20, status, 0x00, 0x00, 0x00, 0x00, 0x04,
    ];
    let mut block = [0u8; 16];
    block[1] = primary;
    block[2] = secondary;
    block[3] = case;
    decode_proximity(&header, Some(&block)).expect("valid advert")
}

#[test]
fn taking_pods_out_of_the_case_keeps_the_case_level_dimmed() {
    let (mut t, t0, w0) = setup();
    let mut aacp = Aacp::new();
    t.set_aacp_connected(true);

    // Lid open, both pods in the case and charging.
    aacp.send(
        &mut t,
        &[
            (LEFT, 90, CHARGING),
            (RIGHT, 88, CHARGING),
            (CASE, 64, NOT_CHARGING),
        ],
        t0,
    );
    let s = t.snapshot();
    assert!(s.left.charging && s.right.charging);
    assert!(s.case.is_live());
    assert_eq!(s.case.level, Some(64));

    // Pods go in the ears: the case stops reporting.
    aacp.send(
        &mut t,
        &[
            (LEFT, 90, NOT_CHARGING),
            (RIGHT, 88, NOT_CHARGING),
            (CASE, 0, GONE),
        ],
        secs(t0, 20),
    );
    let s = t.snapshot();
    assert_eq!(s.case.level, Some(64), "never shows 0% for a quiet case");
    assert!(!s.case.present && s.case.stale);
    assert_eq!(s.case.last_seen, Some(w0));
    assert!(!s.left.charging && s.left.is_live());
    assert_eq!(s.min_pod_level(), Some(88));
}

#[test]
fn one_pod_out_of_range_shows_its_last_level_dimmed() {
    let (mut t, t0, _) = setup();
    let mut aacp = Aacp::new();
    t.set_aacp_connected(true);
    aacp.send(
        &mut t,
        &[(LEFT, 70, NOT_CHARGING), (RIGHT, 40, NOT_CHARGING)],
        t0,
    );
    aacp.send(&mut t, &[(RIGHT, 0, GONE)], secs(t0, 5));
    let s = t.snapshot();
    assert!(s.left.is_live());
    assert_eq!(s.right.level, Some(40));
    assert!(!s.right.present && s.right.stale);
    // The missing pod no longer counts for "lowest" or "low battery".
    assert_eq!(s.min_pod_level(), Some(70));
    assert!(!s.is_low(50));
}

#[test]
fn ble_only_when_another_device_holds_the_control_channel() {
    let (mut t, t0, _) = setup();
    // Right pod is primary (bit5 clear): its level comes first in the advert.
    // Both in ears (bits 1 and 3), nobody in the case.
    let r = advert(0x02 | 0x08, 61, 0x80 | 77, 50);
    t.apply_ble(&r, t0);
    let s = t.snapshot();
    assert_eq!(s.right.level, Some(61));
    assert_eq!(s.left.level, Some(77));
    assert!(s.left.charging && !s.right.charging);
    assert_eq!(s.case.level, None, "case is unknown while no pod is inside");
    assert_eq!(s.source, Some(BatterySource::Ble));
}

#[test]
fn ble_advert_with_a_pod_in_the_case_reports_the_case() {
    let (mut t, t0, _) = setup();
    // Left primary and in the case (bits 5, 6, 4); right in an ear (bit1 after the flip).
    let r = advert(0x20 | 0x40 | 0x10 | 0x02, 0x80 | 55, 80, 0x80 | 30);
    assert!(r.left_in_case && !r.right_in_case);
    t.apply_ble(&r, t0);
    let s = t.snapshot();
    assert_eq!(s.left.level, Some(55));
    assert!(s.left.charging);
    assert_eq!(s.case.level, Some(30));
    assert!(s.case.charging && s.case.is_live());
}

#[test]
fn control_channel_drop_hands_over_to_ble_and_back() {
    let (mut t, t0, _) = setup();
    let mut aacp = Aacp::new();
    t.set_aacp_connected(true);
    aacp.send(
        &mut t,
        &[(LEFT, 80, NOT_CHARGING), (RIGHT, 80, NOT_CHARGING)],
        t0,
    );

    // A BLE advert while AACP is fresh is ignored for the pods.
    t.apply_ble(&advert(0x20 | 0x0A, 50, 50, 0x7F), secs(t0, 10));
    assert_eq!(t.snapshot().left.level, Some(80));

    // The control channel drops; the very next advert counts.
    t.set_aacp_connected(false);
    assert!(!t.aacp_connected());
    t.apply_ble(&advert(0x20 | 0x0A, 79, 78, 0x7F), secs(t0, 11));
    let s = t.snapshot();
    assert_eq!((s.left.level, s.right.level), (Some(79), Some(78)));
    assert_eq!(s.source, Some(BatterySource::Ble));

    // It comes back: AACP wins again straight away.
    t.set_aacp_connected(true);
    aacp.send(
        &mut t,
        &[(LEFT, 77, NOT_CHARGING), (RIGHT, 76, NOT_CHARGING)],
        secs(t0, 12),
    );
    t.apply_ble(&advert(0x20 | 0x0A, 10, 10, 0x7F), secs(t0, 13));
    let s = t.snapshot();
    assert_eq!((s.left.level, s.right.level), (Some(77), Some(76)));
}

#[test]
fn quiet_aacp_never_goes_stale_but_quiet_ble_does() {
    let (mut t, t0, _) = setup();
    let mut aacp = Aacp::new();
    t.set_aacp_connected(true);
    aacp.send(
        &mut t,
        &[(LEFT, 50, NOT_CHARGING), (RIGHT, 50, NOT_CHARGING)],
        t0,
    );
    // AACP can stay silent for many minutes while nothing changes.
    t.expire(secs(t0, 600));
    assert!(t.snapshot().left.is_live());

    t.set_aacp_connected(false);
    t.expire(secs(t0, 601));
    let s = t.snapshot();
    assert!(s.left.stale && !s.left.present && !s.left.charging);
    assert_eq!(s.left.level, Some(50));
}

#[test]
fn unknown_status_counts_only_with_a_real_level() {
    let (mut t, t0, _) = setup();
    t.set_aacp_connected(true);
    let unknown = |component, level| BatteryInfo {
        component,
        level,
        status: BatteryStatus::Unknown,
    };
    t.apply_aacp(&[unknown(BatteryComponent::Left, 42)], t0);
    assert!(t.snapshot().left.is_live());
    t.apply_aacp(&[unknown(BatteryComponent::Left, 0)], t0);
    let s = t.snapshot();
    assert_eq!(s.left.level, Some(42));
    assert!(!s.left.present);
}

#[test]
fn empty_aacp_packet_changes_nothing() {
    let (mut t, t0, _) = setup();
    t.apply_ble(&advert(0x20 | 0x0A, 60, 60, 0x7F), t0);
    let before = t.snapshot();
    t.apply_aacp(&[], t0);
    assert_eq!(t.snapshot(), before);
    assert_eq!(t.snapshot().source, Some(BatterySource::Ble));
}

#[test]
fn reset_forgets_everything() {
    let (mut t, t0, _) = setup();
    t.apply_ble(&advert(0x20 | 0x0A, 60, 60, 0x7F), t0);
    t.reset();
    let s = t.snapshot();
    assert!(!s.has_any());
    assert_eq!(s.source, None);
    assert_eq!(s.min_pod_level(), None);
    assert!(!s.is_low(100));
}

#[test]
fn restore_case_ignores_impossible_levels() {
    let (mut t, _, w0) = setup();
    t.restore_case(101, w0);
    assert_eq!(t.snapshot().case.level, None);
    t.restore_case(100, w0);
    assert_eq!(t.snapshot().case.level, Some(100));
}

#[test]
fn last_seen_follows_the_clock_even_before_the_anchor() {
    let t0 = Instant::now() + Duration::from_secs(100);
    let w0 = SystemTime::UNIX_EPOCH + Duration::from_secs(10_000);
    let mut t = BatteryTracker::with_anchor(t0, w0);
    let earlier = t0 - Duration::from_secs(30);
    t.apply_ble(&advert(0x20 | 0x0A, 60, 60, 0x7F), earlier);
    assert_eq!(
        t.snapshot().left.last_seen,
        Some(w0 - Duration::from_secs(30))
    );
}

#[test]
fn equality_ignores_last_seen_so_adverts_do_not_wake_the_ui() {
    let a = ComponentBattery {
        level: Some(50),
        present: true,
        last_seen: Some(SystemTime::UNIX_EPOCH),
        ..Default::default()
    };
    let b = ComponentBattery {
        last_seen: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(1)),
        ..a.clone()
    };
    assert_eq!(a, b);
    assert_ne!(
        a,
        ComponentBattery {
            charging: true,
            ..b
        }
    );
    assert_eq!(a.display_level(), Some(50));
}

#[test]
fn low_battery_uses_the_threshold_inclusively() {
    let snap = BatterySnapshot {
        left: ComponentBattery {
            level: Some(15),
            present: true,
            ..Default::default()
        },
        right: ComponentBattery {
            level: Some(90),
            present: true,
            ..Default::default()
        },
        ..Default::default()
    };
    assert!(snap.is_low(15));
    assert!(!snap.is_low(14));
    // A case at 5% is not a low-battery warning for the pods.
    let mut with_case = snap.clone();
    with_case.left.level = Some(80);
    with_case.case = ComponentBattery {
        level: Some(5),
        present: true,
        ..Default::default()
    };
    assert!(!with_case.is_low(15));
}

#[test]
fn link_lost_turns_control_channel_levels_into_last_seen() {
    let (mut t, t0, w0) = setup();
    let mut aacp = Aacp::new();
    t.set_aacp_connected(true);
    aacp.send(
        &mut t,
        &[(LEFT, 70, CHARGING), (RIGHT, 72, CHARGING), (CASE, 40, CHARGING)],
        t0,
    );
    t.link_lost();
    let s = t.snapshot();
    for c in [&s.left, &s.right, &s.case] {
        assert!(c.stale && !c.present && !c.charging, "{c:?}");
    }
    assert_eq!(s.case.level, Some(40));
    assert_eq!(s.case.last_seen, Some(w0));
    assert!(!t.aacp_connected());

    // The case advertises while the lid is open: its adverts are live again at once.
    t.apply_ble(&advert(0x20 | 0x40 | 0x04, 0x80 | 71, 0x80 | 72, 0x80 | 39), secs(t0, 5));
    let s = t.snapshot();
    assert!(s.case.is_live() && s.case.charging);
    assert_eq!(s.case.level, Some(39));
    assert!(s.left.is_live() && s.left.charging);
}

#[test]
fn link_lost_keeps_advert_levels_live() {
    let (mut t, t0, _) = setup();
    t.apply_ble(&advert(0x20 | 0x40 | 0x04, 50, 51, 0x80 | 30), t0);
    t.link_lost();
    assert!(t.snapshot().case.is_live());
    assert!(t.snapshot().case.charging);
}
