//! Pure presentation logic shared by the window and the tray: labels, colour classes,
//! capability gating. No iced types here so everything is unit-testable.

use crate::core::battery::ComponentBattery;
use crate::core::models::ModelKind;
use crate::hub::{AdapterState, AppSnapshot, DeviceSnapshot, LinkState};
use std::time::{Duration, SystemTime};

/// Battery colour class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tone {
    Normal,
    /// <= 20 %
    Low,
    /// <= 10 %
    Critical,
    /// Level unknown or component dimmed.
    Muted,
}

pub const LOW_THRESHOLD: u8 = 20;
pub const CRITICAL_THRESHOLD: u8 = 10;

pub fn tone_for(level: Option<u8>) -> Tone {
    match level {
        None => Tone::Muted,
        Some(l) if l <= CRITICAL_THRESHOLD => Tone::Critical,
        Some(l) if l <= LOW_THRESHOLD => Tone::Low,
        Some(_) => Tone::Normal,
    }
}

pub fn percent_text(level: Option<u8>) -> String {
    match level {
        Some(l) => format!("{}%", l.min(100)),
        None => "—".to_string(),
    }
}

/// "just now", "5 min ago", "3 h ago", "2 d ago".
pub fn ago_text(then: SystemTime, now: SystemTime) -> String {
    let secs = now.duration_since(then).unwrap_or(Duration::ZERO).as_secs();
    match secs {
        0..60 => "just now".to_string(),
        60..3600 => format!("{} min ago", secs / 60),
        3600..86400 => format!("{} h ago", secs / 3600),
        _ => format!("{} d ago", secs / 86400),
    }
}

pub fn last_seen_text(then: SystemTime, now: SystemTime) -> String {
    format!("last seen {}", ago_text(then, now))
}

/// Model display name ("AirPods 4"), falling back to "AirPods".
pub fn model_name(kind: ModelKind) -> &'static str {
    kind.info().name
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PillTone {
    Good,
    Busy,
    Info,
    Idle,
}

/// True when the AirPods are not connected but we still hear fresh BLE adverts from them.
pub fn is_nearby(d: &DeviceSnapshot) -> bool {
    !d.is_connected()
        && [
            &d.battery.left,
            &d.battery.right,
            &d.battery.case,
            &d.battery.headphone,
        ]
        .into_iter()
        .any(|c| c.is_live())
}

pub fn connection_label(d: &DeviceSnapshot) -> (&'static str, PillTone) {
    if d.in_use_elsewhere() {
        return ("On another device", PillTone::Info);
    }
    match d.link {
        LinkState::Connected | LinkState::Limited => ("Connected", PillTone::Good),
        LinkState::Connecting => ("Connecting…", PillTone::Busy),
        LinkState::Reconnecting { .. } => ("Reconnecting…", PillTone::Busy),
        LinkState::Disconnected if is_nearby(d) => ("Nearby", PillTone::Info),
        LinkState::Disconnected => ("Not connected", PillTone::Idle),
    }
}

/// One battery gauge as the views draw it.
#[derive(Debug, Clone, PartialEq)]
pub struct Gauge {
    pub label: &'static str,
    pub level: Option<u8>,
    pub text: String,
    pub charging: bool,
    pub tone: Tone,
    /// Drawn greyed out (not present, stale or device disconnected).
    pub dimmed: bool,
    pub caption: Option<String>,
}

fn gauge(label: &'static str, c: &ComponentBattery, connected: bool, now: SystemTime) -> Gauge {
    // Connected but the component isn't reported (pod out of range): show "—", never 0%.
    let hide_level = connected && !c.present && !c.stale;
    let level = if hide_level { None } else { c.level };
    let dimmed = !connected || !c.present || c.stale || level.is_none();
    // While disconnected the hero shows one "last seen" line instead of one per gauge.
    let caption = match (connected && (c.stale || !c.present), c.last_seen, level) {
        (true, Some(t), Some(_)) => Some(last_seen_text(t, now)),
        _ => None,
    };
    Gauge {
        label,
        level,
        text: percent_text(level),
        charging: connected && c.charging && !dimmed,
        tone: if dimmed { Tone::Muted } else { tone_for(level) },
        dimmed,
        caption,
    }
}

/// True for over-ear headphones, or unknown models that only report a headphone battery.
pub fn is_single_battery(d: &DeviceSnapshot) -> bool {
    let info = d.model.info();
    info.is_headphone
        || (!info.is_known()
            && d.battery.headphone.level.is_some()
            && d.battery.left.level.is_none()
            && d.battery.right.level.is_none())
}

pub fn gauges(d: &DeviceSnapshot, now: SystemTime) -> Vec<Gauge> {
    let connected = d.is_connected();
    let b = &d.battery;
    if is_single_battery(d) {
        return vec![gauge("Battery", &b.headphone, connected, now)];
    }
    let mut out = vec![
        gauge("Left", &b.left, connected, now),
        gauge("Right", &b.right, connected, now),
    ];
    if d.model.info().has_case_battery {
        // A stale case (both pods out) keeps its last level, dimmed, with a "last seen" caption.
        out.push(gauge("Case", &b.case, connected, now));
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ListeningMode {
    Off,
    Transparency,
    Adaptive,
    NoiseCancellation,
}

impl ListeningMode {
    pub fn byte(self) -> u8 {
        match self {
            ListeningMode::Off => 0x01,
            ListeningMode::NoiseCancellation => 0x02,
            ListeningMode::Transparency => 0x03,
            ListeningMode::Adaptive => 0x04,
        }
    }

    pub fn from_byte(b: u8) -> Option<Self> {
        Some(match b {
            0x01 => ListeningMode::Off,
            0x02 => ListeningMode::NoiseCancellation,
            0x03 => ListeningMode::Transparency,
            0x04 => ListeningMode::Adaptive,
            _ => return None,
        })
    }

    pub fn label(self) -> &'static str {
        match self {
            ListeningMode::Off => "Off",
            ListeningMode::Transparency => "Transparency",
            ListeningMode::Adaptive => "Adaptive",
            ListeningMode::NoiseCancellation => "Noise Cancellation",
        }
    }
}

/// Listening modes to offer, in display order. Empty when the model has no ANC.
pub fn listening_modes(d: &DeviceSnapshot) -> Vec<ListeningMode> {
    let info = d.model.info();
    if !info.has_anc {
        return Vec::new();
    }
    let mut v = Vec::with_capacity(4);
    if d.allow_off_mode == Some(true) {
        v.push(ListeningMode::Off);
    }
    v.push(ListeningMode::Transparency);
    if info.has_adaptive {
        v.push(ListeningMode::Adaptive);
    }
    v.push(ListeningMode::NoiseCancellation);
    v
}

pub fn shows_conversation_awareness(d: &DeviceSnapshot) -> bool {
    d.model.info().has_conversation_awareness
}

/// Controls (listening mode etc.) only work over a live control channel.
pub fn controls_enabled(d: &DeviceSnapshot) -> bool {
    d.link == LinkState::Connected
}

fn short(label: &str) -> &str {
    match label {
        "Left" => "L",
        "Right" => "R",
        other => other,
    }
}

/// "L 80% · R 75% · Case 40% ⚡" (the ⚡ follows each charging component).
pub fn battery_line(d: &DeviceSnapshot, now: SystemTime) -> String {
    gauges(d, now)
        .iter()
        .map(|g| {
            let bolt = if g.charging { " ⚡" } else { "" };
            if g.label == "Battery" {
                format!("{}{}", g.text, bolt)
            } else {
                format!("{} {}{}", short(g.label), g.text, bolt)
            }
        })
        .collect::<Vec<_>>()
        .join(" · ")
}

/// Tray tooltip, e.g. `("David's AirPods", "AirPods 4 — L 80% · R 75% · Case 40% ⚡")`.
pub fn tooltip(snap: &AppSnapshot, now: SystemTime) -> (String, String) {
    match snap.adapter {
        AdapterState::Off => return ("BazzitePods".into(), "Bluetooth is off".into()),
        AdapterState::Missing if snap.devices.is_empty() => {
            return ("BazzitePods".into(), "Bluetooth unavailable".into());
        }
        _ => {}
    }
    let Some(d) = snap.primary() else {
        return ("BazzitePods".into(), "No AirPods paired".into());
    };
    let model = model_name(d.model);
    let body = if d.is_connected() || is_nearby(d) {
        format!("{model} — {}", battery_line(d, now))
    } else {
        format!("{model} — Not connected")
    };
    (d.name.clone(), body)
}

/// The battery shown on the tray badge: the lowest live pod (or headphone), and whether any
/// of them is charging.
pub fn tray_level(d: &DeviceSnapshot) -> (Option<u8>, bool) {
    let b = &d.battery;
    let pods = [&b.left, &b.right, &b.headphone];
    let charging = pods.iter().any(|c| c.is_live() && c.charging);
    (b.min_pod_level(), charging)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::battery::BatterySnapshot;

    fn comp(level: u8, charging: bool) -> ComponentBattery {
        ComponentBattery {
            level: Some(level),
            charging,
            present: true,
            stale: false,
            last_seen: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(1000)),
        }
    }

    fn device(kind: ModelKind, link: LinkState) -> DeviceSnapshot {
        let mut d = DeviceSnapshot::new("AA".into(), "David's AirPods".into());
        d.model = kind;
        d.link = link;
        d.battery = BatterySnapshot {
            left: comp(80, false),
            right: comp(75, false),
            case: comp(40, true),
            ..Default::default()
        };
        d
    }

    fn now() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1300)
    }

    #[test]
    fn percent_and_tones() {
        assert_eq!(percent_text(Some(80)), "80%");
        assert_eq!(percent_text(Some(150)), "100%");
        assert_eq!(percent_text(None), "—");
        assert_eq!(tone_for(Some(21)), Tone::Normal);
        assert_eq!(tone_for(Some(20)), Tone::Low);
        assert_eq!(tone_for(Some(11)), Tone::Low);
        assert_eq!(tone_for(Some(10)), Tone::Critical);
        assert_eq!(tone_for(Some(0)), Tone::Critical);
        assert_eq!(tone_for(None), Tone::Muted);
    }

    #[test]
    fn ago() {
        let t = SystemTime::UNIX_EPOCH + Duration::from_secs(10_000);
        let at = |s| t + Duration::from_secs(s);
        assert_eq!(ago_text(t, at(5)), "just now");
        assert_eq!(ago_text(t, at(300)), "5 min ago");
        assert_eq!(ago_text(t, at(3 * 3600 + 5)), "3 h ago");
        assert_eq!(ago_text(t, at(2 * 86400)), "2 d ago");
        // Clock went backwards: never panic.
        assert_eq!(ago_text(at(100), t), "just now");
        assert_eq!(last_seen_text(t, at(300)), "last seen 5 min ago");
    }

    #[test]
    fn gauges_for_pods() {
        let mut d = device(ModelKind::AirPods4, LinkState::Connected);
        d.battery.case.stale = true;
        let g = gauges(&d, now());
        assert_eq!(g.len(), 3);
        assert_eq!(g[0].text, "80%");
        assert!(!g[0].dimmed);
        assert_eq!(g[2].label, "Case");
        assert!(g[2].dimmed);
        assert_eq!(g[2].text, "40%");
        assert_eq!(g[2].caption.as_deref(), Some("last seen 5 min ago"));
        assert!(!g[2].charging);

        // Pod not reported while connected: "—", dimmed, not 0%.
        d.battery.left.present = false;
        let g = gauges(&d, now());
        assert_eq!(g[0].text, "—");
        assert!(g[0].dimmed);
        assert_eq!(g[0].tone, Tone::Muted);
    }

    #[test]
    fn disconnected_keeps_last_levels_greyed() {
        let d = device(ModelKind::AirPods4, LinkState::Disconnected);
        let g = gauges(&d, now());
        assert_eq!(g[0].text, "80%");
        assert!(
            g.iter()
                .all(|g| g.dimmed && !g.charging && g.caption.is_none())
        );
    }

    #[test]
    fn single_ring_for_max() {
        let mut d = device(ModelKind::AirPodsMax, LinkState::Connected);
        d.battery.headphone = comp(64, true);
        let g = gauges(&d, now());
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].text, "64%");
        assert!(g[0].charging);
    }

    #[test]
    fn capability_gating() {
        let mut d = device(ModelKind::AirPods4, LinkState::Connected);
        assert!(listening_modes(&d).is_empty());
        assert!(!shows_conversation_awareness(&d));

        d.model = ModelKind::AirPods4Anc;
        assert_eq!(
            listening_modes(&d),
            vec![
                ListeningMode::Transparency,
                ListeningMode::Adaptive,
                ListeningMode::NoiseCancellation
            ]
        );
        d.allow_off_mode = Some(true);
        assert_eq!(listening_modes(&d)[0], ListeningMode::Off);
        assert!(shows_conversation_awareness(&d));

        d.model = ModelKind::AirPodsPro;
        d.allow_off_mode = None;
        assert_eq!(
            listening_modes(&d),
            vec![
                ListeningMode::Transparency,
                ListeningMode::NoiseCancellation
            ]
        );
        assert!(!shows_conversation_awareness(&d));

        d.model = ModelKind::Unknown;
        assert!(listening_modes(&d).is_empty());

        d.link = LinkState::Limited;
        assert!(!controls_enabled(&d));
    }

    #[test]
    fn mode_bytes_roundtrip() {
        for m in [
            ListeningMode::Off,
            ListeningMode::Transparency,
            ListeningMode::Adaptive,
            ListeningMode::NoiseCancellation,
        ] {
            assert_eq!(ListeningMode::from_byte(m.byte()), Some(m));
        }
        assert_eq!(ListeningMode::from_byte(0x09), None);
    }

    #[test]
    fn tooltip_strings() {
        let mut snap = AppSnapshot {
            adapter: AdapterState::On,
            ..Default::default()
        };
        assert_eq!(tooltip(&snap, now()).1, "No AirPods paired");

        let d = device(ModelKind::AirPods4, LinkState::Connected);
        snap.devices.insert(d.mac.clone(), d);
        let (title, body) = tooltip(&snap, now());
        assert_eq!(title, "David's AirPods");
        assert_eq!(body, "AirPods 4 — L 80% · R 75% · Case 40% ⚡");

        if let Some(d) = snap.devices.get_mut("AA") {
            d.link = LinkState::Disconnected;
            d.battery.left.present = false;
            d.battery.right.present = false;
            d.battery.case.present = false;
        }
        assert_eq!(tooltip(&snap, now()).1, "AirPods 4 — Not connected");

        snap.adapter = AdapterState::Off;
        assert_eq!(tooltip(&snap, now()).1, "Bluetooth is off");
    }

    #[test]
    fn labels() {
        let mut d = device(ModelKind::AirPods4, LinkState::Reconnecting { attempt: 2 });
        assert_eq!(connection_label(&d).0, "Reconnecting…");
        d.link = LinkState::Disconnected;
        assert_eq!(connection_label(&d).0, "Nearby");
        d.battery = BatterySnapshot::default();
        assert_eq!(connection_label(&d).0, "Not connected");
        assert_eq!(model_name(ModelKind::Unknown), "AirPods");
    }

    #[test]
    fn elsewhere_overrides_connected_label() {
        let mut d = device(ModelKind::AirPods4, LinkState::Connected);
        d.owns_audio = Some(false);
        assert_eq!(connection_label(&d).0, "On another device");
        d.owns_audio = Some(true);
        assert_eq!(connection_label(&d).0, "Connected");
        d.owns_audio = Some(false);
        d.link = LinkState::Disconnected;
        assert!(!d.in_use_elsewhere());
    }

    #[test]
    fn tray_level_is_lowest_live_pod() {
        let mut d = device(ModelKind::AirPods4, LinkState::Connected);
        assert_eq!(tray_level(&d), (Some(75), false));
        d.battery.right.present = false;
        d.battery.left.charging = true;
        assert_eq!(tray_level(&d), (Some(80), true));
    }
}
