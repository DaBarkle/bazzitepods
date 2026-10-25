//! Single source of truth for what the UI, tray and overlay show.
//!
//! Device tasks write into the hub; every view subscribes to the `watch` channel
//! and renders the latest `AppSnapshot`. Nothing outside device tasks mutates state.

use crate::core::battery::BatterySnapshot;
use crate::core::models::ModelKind;
use serde::Serialize;
use std::collections::BTreeMap;
use std::time::SystemTime;
use tokio::sync::watch;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
pub enum AdapterState {
    /// No Bluetooth adapter present (or bluetoothd not running).
    #[default]
    Missing,
    /// Adapter exists but is powered off (we never power it on ourselves).
    Off,
    On,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum LinkState {
    /// Bluetooth link is up and the control channel is being opened.
    Connecting,
    /// Control channel is up; battery and controls are live.
    Connected,
    /// Bluetooth link is up but the control channel dropped; retrying.
    Reconnecting {
        attempt: u32,
    },
    /// Bluetooth link is up but the control channel could not be opened.
    /// Audio may still work; battery falls back to BLE adverts.
    Limited,
    Disconnected,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DeviceSnapshot {
    pub mac: String,
    pub name: String,
    pub model: ModelKind,
    pub model_number: Option<String>,
    pub link: LinkState,
    pub battery: BatterySnapshot,
    /// Listening mode byte (0x01 off, 0x02 ANC, 0x03 transparency, 0x04 adaptive).
    pub listening_mode: Option<u8>,
    pub allow_off_mode: Option<bool>,
    pub conversation_awareness: Option<bool>,
    /// (left, right) in-ear state when known.
    pub in_ear: (Option<bool>, Option<bool>),
    /// Human readable audio state from the audio guard (e.g. "AAC", "Recovering").
    pub audio: Option<String>,
    pub last_seen: Option<SystemTime>,
    /// Whether this PC has the AirPods' audio route. `Some(false)` means another Apple
    /// device took it (the AirPods stay connected here, but play on that device).
    pub owns_audio: Option<bool>,
    /// Other devices currently connected to the AirPods besides this PC.
    pub other_devices: usize,
}

impl DeviceSnapshot {
    pub fn new(mac: String, name: String) -> Self {
        Self {
            mac,
            name,
            model: ModelKind::Unknown,
            model_number: None,
            link: LinkState::Disconnected,
            battery: BatterySnapshot::default(),
            listening_mode: None,
            allow_off_mode: None,
            conversation_awareness: None,
            in_ear: (None, None),
            audio: None,
            last_seen: Some(SystemTime::now()),
            owns_audio: None,
            other_devices: 0,
        }
    }

    pub fn is_connected(&self) -> bool {
        !matches!(self.link, LinkState::Disconnected)
    }

    /// Connected here, but another device has the audio.
    pub fn in_use_elsewhere(&self) -> bool {
        self.is_connected() && self.owns_audio == Some(false)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Default)]
pub struct AppSnapshot {
    pub adapter: AdapterState,
    pub devices: BTreeMap<String, DeviceSnapshot>,
}

impl AppSnapshot {
    /// The device the views should feature: the connected one, else the most recently seen.
    pub fn primary(&self) -> Option<&DeviceSnapshot> {
        self.devices
            .values()
            .filter(|d| d.is_connected())
            .max_by_key(|d| d.last_seen)
            .or_else(|| self.devices.values().max_by_key(|d| d.last_seen))
    }
}

#[derive(Clone)]
pub struct Hub {
    tx: watch::Sender<AppSnapshot>,
}

impl Hub {
    pub fn new() -> Self {
        let (tx, _rx) = watch::channel(AppSnapshot::default());
        Self { tx }
    }

    pub fn subscribe(&self) -> watch::Receiver<AppSnapshot> {
        self.tx.subscribe()
    }

    pub fn snapshot(&self) -> AppSnapshot {
        self.tx.borrow().clone()
    }

    pub fn set_adapter(&self, state: AdapterState) {
        self.tx.send_if_modified(|s| {
            if s.adapter == state {
                return false;
            }
            s.adapter = state;
            true
        });
    }

    /// Mutate one device; creates it if missing. Only notifies when something changed.
    pub fn update_device(&self, mac: &str, name: &str, f: impl FnOnce(&mut DeviceSnapshot)) {
        self.tx.send_if_modified(|s| {
            let created = !s.devices.contains_key(mac);
            let entry = s
                .devices
                .entry(mac.to_string())
                .or_insert_with(|| DeviceSnapshot::new(mac.to_string(), name.to_string()));
            let before = entry.clone();
            f(entry);
            created || *entry != before
        });
    }

    pub fn set_link(&self, mac: &str, link: LinkState) {
        self.tx.send_if_modified(|s| match s.devices.get_mut(mac) {
            Some(d) if d.link != link => {
                if link == LinkState::Disconnected {
                    // Routing state belongs to the session that just ended.
                    d.owns_audio = None;
                    d.other_devices = 0;
                }
                d.link = link;
                d.last_seen = Some(SystemTime::now());
                true
            }
            _ => false,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_only_notifies_on_change() {
        let hub = Hub::new();
        let mut rx = hub.subscribe();
        hub.update_device("AA", "Pods", |_| {});
        assert!(rx.has_changed().unwrap());
        rx.borrow_and_update();
        hub.update_device("AA", "Pods", |_| {});
        assert!(!rx.has_changed().unwrap());
        hub.set_link("AA", LinkState::Connected);
        assert!(rx.has_changed().unwrap());
    }

    #[test]
    fn primary_prefers_connected() {
        let hub = Hub::new();
        hub.update_device("AA", "Old", |d| d.link = LinkState::Disconnected);
        hub.update_device("BB", "New", |d| d.link = LinkState::Connected);
        assert_eq!(hub.snapshot().primary().map(|d| d.mac.as_str()), Some("BB"));
    }
}
