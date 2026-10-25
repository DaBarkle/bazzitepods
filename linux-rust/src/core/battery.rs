//! Pure battery model. Merges AACP and BLE readings per component.
//!
//! Rules (see docs/ARCHITECTURE.md, "Battery rules"):
//! * AACP is authoritative while the link is up. BLE fills a component only when
//!   AACP is down or has not reported that component for more than [`AACP_FRESH_FOR`].
//! * A component reported `Disconnected` (or missing from an AACP packet) keeps its
//!   last level, with `present = false`.
//! * BLE levels `0x7F`, `0xFF` or anything >100 after masking are invalid and ignored.
//! * The case only reports while at least one pod is in it; otherwise it keeps its
//!   last value with `stale = true`.
//!
//! Field semantics on [`ComponentBattery`]:
//! * `level`: last known valid level (0..=100), `None` if never seen.
//! * `present`: the active source currently reports this component.
//! * `stale`: `level` is a retained value, not freshly confirmed. The UI should
//!   dim a component when `!present || stale` and never show "0%" for it.
//! * `last_seen`: wall-clock time the level was last freshly reported.
//!
//! All time inputs are an injected [`Instant`] so the logic is testable.

use crate::bluetooth::aacp::{BatteryComponent, BatteryInfo, BatteryStatus};
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant, SystemTime};

/// How long an AACP report for a component suppresses BLE for that component.
pub const AACP_FRESH_FOR: Duration = Duration::from_secs(60);
/// With AACP down, a component not refreshed by BLE for this long is marked stale.
pub const BLE_STALE_AFTER: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ComponentBattery {
    pub level: Option<u8>,
    pub charging: bool,
    pub present: bool,
    pub stale: bool,
    pub last_seen: Option<SystemTime>,
}

/// `last_seen` is deliberately not compared: it ticks on every BLE advert, and treating that
/// as a change would wake the tray, UI and overlay about once a second.
impl PartialEq for ComponentBattery {
    fn eq(&self, other: &Self) -> bool {
        self.level == other.level
            && self.charging == other.charging
            && self.present == other.present
            && self.stale == other.stale
    }
}

impl Eq for ComponentBattery {}

impl ComponentBattery {
    /// Level suitable for display: `None` if never seen.
    pub fn display_level(&self) -> Option<u8> {
        self.level
    }

    /// True when the level is fresh and the component is reported.
    pub fn is_live(&self) -> bool {
        self.present && !self.stale && self.level.is_some()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BatterySource {
    Aacp,
    Ble,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatterySnapshot {
    pub left: ComponentBattery,
    pub right: ComponentBattery,
    pub case: ComponentBattery,
    /// AirPods Max single battery.
    pub headphone: ComponentBattery,
    /// Source of the most recent accepted update.
    pub source: Option<BatterySource>,
}

impl BatterySnapshot {
    /// Lowest level among present, non-stale pods (or the headphone battery).
    pub fn min_pod_level(&self) -> Option<u8> {
        [&self.left, &self.right, &self.headphone]
            .into_iter()
            .filter(|c| c.is_live())
            .filter_map(|c| c.level)
            .min()
    }

    /// True when any present, non-charging pod (or headphone) is at or below
    /// `threshold` percent.
    pub fn is_low(&self, threshold: u8) -> bool {
        [&self.left, &self.right, &self.headphone]
            .into_iter()
            .filter(|c| c.is_live() && !c.charging)
            .filter_map(|c| c.level)
            .any(|l| l <= threshold)
    }

    /// True if any component has ever reported a level.
    pub fn has_any(&self) -> bool {
        [&self.left, &self.right, &self.case, &self.headphone]
            .into_iter()
            .any(|c| c.level.is_some())
    }
}

/// A decoded BLE proximity-pairing advert for one of our AirPods. Levels are
/// already validated (0..=100); `None` means the advert carried no valid value.
/// Left/right have already been resolved using the primary-left flip bit.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BleBatteryReading {
    pub left: Option<(u8, bool)>,
    pub right: Option<(u8, bool)>,
    /// Only `Some` when at least one pod is in the case.
    pub case: Option<(u8, bool)>,
    /// AirPods Max.
    pub headphone: Option<(u8, bool)>,
    pub left_in_case: bool,
    pub right_in_case: bool,
    pub lid_open: Option<bool>,
    pub product_id: Option<u16>,
    pub left_in_ear: Option<bool>,
    pub right_in_ear: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Slot {
    Left = 0,
    Right = 1,
    Case = 2,
    Headphone = 3,
}

const SLOTS: [Slot; 4] = [Slot::Left, Slot::Right, Slot::Case, Slot::Headphone];

#[derive(Debug, Clone, Default)]
struct SlotState {
    battery: ComponentBattery,
    /// Last time AACP reported this component (any status).
    aacp_at: Option<Instant>,
    /// Last time any source freshly reported a valid level.
    fresh_at: Option<Instant>,
}

#[derive(Debug, Clone)]
pub struct BatteryTracker {
    slots: [SlotState; 4],
    aacp_connected: bool,
    source: Option<BatterySource>,
    /// Maps injected `Instant`s to wall-clock time for `last_seen`.
    anchor: (Instant, SystemTime),
}

impl Default for BatteryTracker {
    fn default() -> Self {
        Self::new()
    }
}

fn slot_of(c: BatteryComponent) -> Slot {
    match c {
        BatteryComponent::Left => Slot::Left,
        BatteryComponent::Right => Slot::Right,
        BatteryComponent::Case => Slot::Case,
        BatteryComponent::Headphone => Slot::Headphone,
    }
}

impl BatteryTracker {
    pub fn new() -> Self {
        Self::with_anchor(Instant::now(), SystemTime::now())
    }

    /// Construct with an explicit mapping between `Instant` and wall-clock time
    /// (`last_seen = wall + (now - instant)`). Useful for deterministic tests.
    pub fn with_anchor(instant: Instant, wall: SystemTime) -> Self {
        BatteryTracker {
            slots: Default::default(),
            aacp_connected: false,
            source: None,
            anchor: (instant, wall),
        }
    }

    fn wall(&self, now: Instant) -> SystemTime {
        let (a_inst, a_wall) = self.anchor;
        match now.checked_duration_since(a_inst) {
            Some(d) => a_wall + d,
            None => a_wall - a_inst.duration_since(now),
        }
    }

    fn slot(&mut self, s: Slot) -> &mut SlotState {
        &mut self.slots[s as usize]
    }

    fn set_fresh(&mut self, s: Slot, level: u8, charging: bool, now: Instant) {
        let wall = self.wall(now);
        let st = self.slot(s);
        st.battery.level = Some(level);
        st.battery.charging = charging;
        st.battery.present = true;
        st.battery.stale = false;
        st.battery.last_seen = Some(wall);
        st.fresh_at = Some(now);
    }

    /// Keep the last level but mark the component as not currently reported.
    fn set_absent(&mut self, s: Slot) {
        let st = self.slot(s);
        st.battery.present = false;
        st.battery.charging = false;
        st.battery.stale = st.battery.level.is_some();
    }

    /// Apply an AACP BATTERY_INFO packet. Components missing from the packet are
    /// marked not present (keeping their level).
    pub fn apply_aacp(&mut self, infos: &[BatteryInfo], now: Instant) {
        let mut seen = [false; 4];
        for info in infos {
            let s = slot_of(info.component);
            seen[s as usize] = true;
            self.slot(s).aacp_at = Some(now);
            let valid = info.level <= 100;
            match info.status {
                BatteryStatus::Charging | BatteryStatus::NotCharging if valid => {
                    self.set_fresh(s, info.level, info.status == BatteryStatus::Charging, now);
                }
                BatteryStatus::Unknown if valid && info.level > 0 => {
                    self.set_fresh(s, info.level, false, now);
                }
                _ => self.set_absent(s),
            }
        }
        if infos.is_empty() {
            return;
        }
        for s in SLOTS {
            if !seen[s as usize] {
                self.set_absent(s);
            }
        }
        self.source = Some(BatterySource::Aacp);
    }

    fn aacp_owns(&self, s: Slot, now: Instant) -> bool {
        self.aacp_connected
            && self.slots[s as usize]
                .aacp_at
                .is_some_and(|t| now.saturating_duration_since(t) <= AACP_FRESH_FOR)
    }

    /// Apply a decoded BLE advert. Ignored per component while AACP is fresh.
    pub fn apply_ble(&mut self, r: &BleBatteryReading, now: Instant) {
        let mut applied = false;
        let is_headphone = r.headphone.is_some() && r.left.is_none() && r.right.is_none();
        let valid = |v: Option<(u8, bool)>| v.filter(|(l, _)| *l <= 100);

        if is_headphone {
            if !self.aacp_owns(Slot::Headphone, now)
                && let Some((l, c)) = valid(r.headphone)
            {
                self.set_fresh(Slot::Headphone, l, c, now);
                applied = true;
            }
        } else {
            for (s, v) in [(Slot::Left, r.left), (Slot::Right, r.right)] {
                if self.aacp_owns(s, now) {
                    continue;
                }
                match valid(v) {
                    Some((l, c)) => self.set_fresh(s, l, c, now),
                    None => self.set_absent(s),
                }
                applied = true;
            }
            if !self.aacp_owns(Slot::Case, now) {
                let any_in_case = r.left_in_case || r.right_in_case;
                match valid(r.case) {
                    Some((l, c)) if any_in_case => self.set_fresh(Slot::Case, l, c, now),
                    _ => self.set_absent(Slot::Case),
                }
                applied = true;
            }
        }
        if applied {
            self.source = Some(BatterySource::Ble);
        }
    }

    /// Tell the tracker whether the AACP link is up. When it goes down, BLE may
    /// immediately take over every component.
    pub fn set_aacp_connected(&mut self, connected: bool) {
        self.aacp_connected = connected;
        if !connected {
            for st in &mut self.slots {
                st.aacp_at = None;
            }
        }
    }

    pub fn aacp_connected(&self) -> bool {
        self.aacp_connected
    }

    /// Mark components stale that no source has refreshed recently. Only applies
    /// while AACP is down (AACP may legitimately stay quiet for minutes).
    /// Call periodically, e.g. every 10 s.
    pub fn expire(&mut self, now: Instant) {
        if self.aacp_connected {
            return;
        }
        for st in &mut self.slots {
            let old = st
                .fresh_at
                .is_none_or(|t| now.saturating_duration_since(t) > BLE_STALE_AFTER);
            if old && st.battery.level.is_some() {
                st.battery.stale = true;
                st.battery.present = false;
                st.battery.charging = false;
            }
        }
    }

    /// Forget everything (e.g. a different device).
    pub fn reset(&mut self) {
        self.slots = Default::default();
        self.source = None;
    }

    pub fn snapshot(&self) -> BatterySnapshot {
        BatterySnapshot {
            left: self.slots[Slot::Left as usize].battery.clone(),
            right: self.slots[Slot::Right as usize].battery.clone(),
            case: self.slots[Slot::Case as usize].battery.clone(),
            headphone: self.slots[Slot::Headphone as usize].battery.clone(),
            source: self.source,
        }
    }

    pub fn min_pod_level(&self) -> Option<u8> {
        self.snapshot().min_pod_level()
    }

    pub fn is_low(&self, threshold: u8) -> bool {
        self.snapshot().is_low(threshold)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bi(component: BatteryComponent, level: u8, status: BatteryStatus) -> BatteryInfo {
        BatteryInfo {
            component,
            level,
            status,
        }
    }

    fn setup() -> (BatteryTracker, Instant, SystemTime) {
        let t0 = Instant::now();
        let w0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        (BatteryTracker::with_anchor(t0, w0), t0, w0)
    }

    fn pods(l: u8, r: u8, c: Option<u8>) -> BleBatteryReading {
        BleBatteryReading {
            left: Some((l, false)),
            right: Some((r, false)),
            case: c.map(|c| (c, false)),
            left_in_case: c.is_some(),
            right_in_case: c.is_some(),
            ..Default::default()
        }
    }

    use BatteryComponent::*;
    use BatteryStatus::*;

    #[test]
    fn aacp_basic_and_last_seen() {
        let (mut t, t0, w0) = setup();
        t.set_aacp_connected(true);
        let now = t0 + Duration::from_secs(5);
        t.apply_aacp(
            &[
                bi(Left, 80, NotCharging),
                bi(Right, 75, Charging),
                bi(Case, 40, Charging),
            ],
            now,
        );
        let s = t.snapshot();
        assert_eq!(s.left.level, Some(80));
        assert!(s.left.present && !s.left.stale && !s.left.charging);
        assert!(s.right.charging);
        assert_eq!(s.case.level, Some(40));
        assert!(s.case.charging);
        assert_eq!(s.left.last_seen, Some(w0 + Duration::from_secs(5)));
        assert_eq!(s.source, Some(BatterySource::Aacp));
        assert!(!s.headphone.present);
        assert_eq!(s.headphone.level, None);
    }

    #[test]
    fn disconnected_pod_keeps_last_value_dimmed() {
        let (mut t, t0, _) = setup();
        t.set_aacp_connected(true);
        t.apply_aacp(&[bi(Left, 80, NotCharging), bi(Right, 70, NotCharging)], t0);
        t.apply_aacp(&[bi(Left, 0, Disconnected), bi(Right, 69, NotCharging)], t0);
        let s = t.snapshot();
        assert_eq!(s.left.level, Some(80));
        assert!(!s.left.present && s.left.stale);
        assert_eq!(s.right.level, Some(69));
        // min ignores the dim pod
        assert_eq!(s.min_pod_level(), Some(69));
    }

    #[test]
    fn missing_component_kept_not_present() {
        let (mut t, t0, _) = setup();
        t.set_aacp_connected(true);
        t.apply_aacp(&[bi(Left, 80, NotCharging), bi(Right, 70, NotCharging)], t0);
        t.apply_aacp(&[bi(Right, 68, NotCharging)], t0);
        let s = t.snapshot();
        assert_eq!(s.left.level, Some(80));
        assert!(!s.left.present);
        assert_eq!(s.right.level, Some(68));
    }

    #[test]
    fn unknown_status() {
        let (mut t, t0, _) = setup();
        t.set_aacp_connected(true);
        t.apply_aacp(&[bi(Case, 55, Unknown), bi(Left, 0, Unknown)], t0);
        let s = t.snapshot();
        assert_eq!(s.case.level, Some(55));
        assert!(s.case.present && !s.case.charging);
        assert_eq!(s.left.level, None);
        assert!(!s.left.present);
    }

    #[test]
    fn aacp_invalid_level_ignored() {
        let (mut t, t0, _) = setup();
        t.set_aacp_connected(true);
        t.apply_aacp(&[bi(Left, 50, NotCharging)], t0);
        t.apply_aacp(&[bi(Left, 0xFF, NotCharging)], t0);
        assert_eq!(t.snapshot().left.level, Some(50));
    }

    #[test]
    fn ble_sentinels_ignored() {
        let (mut t, t0, _) = setup();
        t.apply_ble(&pods(60, 61, Some(30)), t0);
        // Decoder maps 0x7F/0xFF to None; tracker must also reject >100 defensively.
        let r = BleBatteryReading {
            left: None,
            right: Some((127, false)),
            case: Some((255, false)),
            left_in_case: true,
            right_in_case: true,
            ..Default::default()
        };
        t.apply_ble(&r, t0 + Duration::from_secs(1));
        let s = t.snapshot();
        assert_eq!(s.left.level, Some(60));
        assert_eq!(s.right.level, Some(61));
        assert_eq!(s.case.level, Some(30));
        assert!(!s.left.present && !s.right.present && !s.case.present);
        assert!(s.right.stale);
        assert_eq!(s.source, Some(BatterySource::Ble));
    }

    #[test]
    fn case_stale_when_both_pods_out() {
        let (mut t, t0, w0) = setup();
        t.apply_ble(&pods(60, 61, Some(30)), t0);
        let mut out = pods(59, 60, None);
        // Even if the advert carries a case byte, it must not be trusted.
        out.case = Some((99, false));
        t.apply_ble(&out, t0 + Duration::from_secs(30));
        let s = t.snapshot();
        assert_eq!(s.case.level, Some(30));
        assert!(s.case.stale && !s.case.present);
        assert_eq!(s.case.last_seen, Some(w0));
        assert_eq!(s.left.level, Some(59));
    }

    #[test]
    fn case_trusted_with_one_pod_in() {
        let (mut t, t0, _) = setup();
        let r = BleBatteryReading {
            left: Some((50, true)),
            right: Some((70, false)),
            case: Some((45, false)),
            left_in_case: true,
            right_in_case: false,
            ..Default::default()
        };
        t.apply_ble(&r, t0);
        let s = t.snapshot();
        assert_eq!(s.case.level, Some(45));
        assert!(s.case.present && !s.case.stale);
        assert!(s.left.charging && !s.right.charging);
    }

    #[test]
    fn ble_ignored_while_aacp_fresh_then_used_after_60s() {
        let (mut t, t0, _) = setup();
        t.set_aacp_connected(true);
        t.apply_aacp(
            &[
                bi(Left, 80, NotCharging),
                bi(Right, 80, NotCharging),
                bi(Case, 50, NotCharging),
            ],
            t0,
        );
        t.apply_ble(&pods(70, 70, Some(40)), t0 + Duration::from_secs(59));
        let s = t.snapshot();
        assert_eq!(s.left.level, Some(80));
        assert_eq!(s.case.level, Some(50));
        assert_eq!(s.source, Some(BatterySource::Aacp));

        t.apply_ble(&pods(70, 71, Some(40)), t0 + Duration::from_secs(61));
        let s = t.snapshot();
        assert_eq!(s.left.level, Some(70));
        assert_eq!(s.right.level, Some(71));
        assert_eq!(s.case.level, Some(40));
        assert_eq!(s.source, Some(BatterySource::Ble));
    }

    #[test]
    fn ble_used_immediately_when_aacp_down() {
        let (mut t, t0, _) = setup();
        t.set_aacp_connected(true);
        t.apply_aacp(&[bi(Left, 80, NotCharging), bi(Right, 80, NotCharging)], t0);
        t.set_aacp_connected(false);
        t.apply_ble(&pods(79, 78, None), t0 + Duration::from_secs(1));
        let s = t.snapshot();
        assert_eq!(s.left.level, Some(79));
        assert_eq!(s.right.level, Some(78));
    }

    #[test]
    fn ble_fills_component_aacp_never_reported() {
        let (mut t, t0, _) = setup();
        t.set_aacp_connected(true);
        t.apply_aacp(&[bi(Left, 80, NotCharging), bi(Right, 80, NotCharging)], t0);
        t.apply_ble(&pods(70, 70, Some(33)), t0 + Duration::from_secs(1));
        let s = t.snapshot();
        assert_eq!(s.left.level, Some(80));
        assert_eq!(s.case.level, Some(33));
    }

    #[test]
    fn charging_flags_and_low() {
        let (mut t, t0, _) = setup();
        t.set_aacp_connected(true);
        t.apply_aacp(&[bi(Left, 10, Charging), bi(Right, 30, NotCharging)], t0);
        assert_eq!(t.min_pod_level(), Some(10));
        assert!(!t.is_low(15), "charging pod is not low");
        assert!(t.is_low(30));
        t.apply_aacp(&[bi(Left, 10, NotCharging), bi(Right, 30, NotCharging)], t0);
        assert!(t.is_low(15));
        assert!(!t.snapshot().left.charging);
    }

    #[test]
    fn airpods_max_headphone() {
        let (mut t, t0, _) = setup();
        t.set_aacp_connected(true);
        t.apply_aacp(&[bi(Headphone, 64, Charging)], t0);
        let s = t.snapshot();
        assert_eq!(s.headphone.level, Some(64));
        assert!(s.headphone.charging && s.headphone.present);
        assert!(!s.left.present && !s.case.present);
        assert_eq!(s.min_pod_level(), Some(64));

        // BLE path for Max, after AACP drops
        t.set_aacp_connected(false);
        let r = BleBatteryReading {
            headphone: Some((63, false)),
            product_id: Some(0x1F20),
            ..Default::default()
        };
        t.apply_ble(&r, t0 + Duration::from_secs(2));
        let s = t.snapshot();
        assert_eq!(s.headphone.level, Some(63));
        assert!(!s.headphone.charging);
        assert_eq!(s.left.level, None);
    }

    #[test]
    fn expire_marks_stale_when_aacp_down() {
        let (mut t, t0, _) = setup();
        t.apply_ble(&pods(60, 60, Some(20)), t0);
        t.expire(t0 + Duration::from_secs(30));
        assert!(t.snapshot().left.is_live());
        t.expire(t0 + Duration::from_secs(61));
        let s = t.snapshot();
        assert!(s.left.stale && !s.left.present);
        assert_eq!(s.left.level, Some(60));
        assert_eq!(s.min_pod_level(), None);
        assert!(s.has_any());
    }

    #[test]
    fn snapshot_serializes() {
        let (mut t, t0, _) = setup();
        t.apply_ble(&pods(60, 60, Some(20)), t0);
        let json = serde_json::to_string(&t.snapshot()).unwrap();
        assert!(json.contains("\"source\":\"ble\""));
        let back: BatterySnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(back, t.snapshot());
    }
}
