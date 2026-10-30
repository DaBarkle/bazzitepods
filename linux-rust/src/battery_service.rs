//! Owns one `BatteryTracker` per AirPods and publishes merged battery state to the hub.
//!
//! Inputs come from two places: AACP battery packets (device tasks, only while the control
//! channel is up) and BLE proximity adverts (le.rs, any time the AirPods are nearby, even when
//! not connected to this PC). Keeping a single owner means the two sources can never race.

use crate::bluetooth::aacp::BatteryInfo;
use crate::core::battery::{BatteryTracker, BleBatteryReading};
use crate::core::models::{ModelInfo, ModelKind};
use crate::hub::{Hub, LinkState};
use bluer::Address;
use log::debug;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, UNIX_EPOCH};
use tokio::sync::mpsc;

#[derive(Debug)]
pub enum BatteryInput {
    Aacp(Address, Vec<BatteryInfo>),
    AacpLink(Address, bool),
    /// The Bluetooth link to this PC is gone (the device task ended).
    LinkLost(Address),
    Ble(Address, BleBatteryReading),
}

#[derive(Clone)]
pub struct BatteryService {
    tx: mpsc::UnboundedSender<BatteryInput>,
}

impl BatteryService {
    pub fn spawn(hub: Hub) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(run(hub, rx, case_cache_path()));
        Self { tx }
    }

    pub fn send(&self, input: BatteryInput) {
        let _ = self.tx.send(input);
    }
}

/// Last real case level per AirPods, so "Case 53% · 2h ago" survives a restart. The case
/// only reports while a pod sits in it, so this is often the only value there is.
fn case_cache_path() -> PathBuf {
    crate::utils::get_devices_path().with_file_name("case_battery.json")
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, PartialEq)]
struct CachedCase {
    level: u8,
    /// Seconds since the Unix epoch.
    at: u64,
}

fn load_case_cache(path: &Path) -> HashMap<String, CachedCase> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn tracker_for<'a>(
    trackers: &'a mut HashMap<Address, BatteryTracker>,
    cache: &HashMap<String, CachedCase>,
    mac: Address,
) -> &'a mut BatteryTracker {
    trackers.entry(mac).or_insert_with(|| {
        let mut t = BatteryTracker::default();
        if let Some(c) = cache.get(&mac.to_string()) {
            t.restore_case(c.level, UNIX_EPOCH + Duration::from_secs(c.at));
        }
        t
    })
}

async fn run(hub: Hub, mut rx: mpsc::UnboundedReceiver<BatteryInput>, cache_path: PathBuf) {
    let mut trackers: HashMap<Address, BatteryTracker> = HashMap::new();
    let mut case_cache = load_case_cache(&cache_path);
    // Periodic tick so staleness (e.g. AACP silent for 60 s) is re-evaluated without new input.
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(15));
    loop {
        let touched = tokio::select! {
            input = rx.recv() => {
                let Some(input) = input else { return };
                let now = Instant::now();
                match input {
                    BatteryInput::Aacp(mac, info) => {
                        tracker_for(&mut trackers, &case_cache, mac).apply_aacp(&info, now);
                        vec![mac]
                    }
                    BatteryInput::AacpLink(mac, up) => {
                        tracker_for(&mut trackers, &case_cache, mac).set_aacp_connected(up);
                        vec![mac]
                    }
                    BatteryInput::LinkLost(mac) => {
                        tracker_for(&mut trackers, &case_cache, mac).link_lost();
                        vec![mac]
                    }
                    BatteryInput::Ble(mac, reading) => {
                        let product = reading.product_id;
                        let in_case = reading.left_in_case && reading.right_in_case;
                        let mac_s = mac.to_string();
                        // Same rule as the ear reports: in the case, none in an ear.
                        crate::devices::airpods::note_put_away(
                            &mac_s,
                            (reading.left_in_case || reading.right_in_case)
                                && reading.left_in_ear != Some(true)
                                && reading.right_in_ear != Some(true),
                            "adverts",
                        );
                        hub.update_device(&mac_s, &known_name(&hub, &mac_s), |d| {
                            if d.link == LinkState::Disconnected {
                                d.in_case = in_case;
                            }
                        });
                        tracker_for(&mut trackers, &case_cache, mac).apply_ble(&reading, now);
                        if let Some(kind) = product
                            .map(|id| ModelInfo::from_ble_product_id(id).kind)
                            .filter(|k| *k != ModelKind::Unknown)
                        {
                            set_model_if_unknown(&hub, mac, kind);
                        }
                        vec![mac]
                    }
                }
            }
            _ = tick.tick() => {
                let now = Instant::now();
                trackers.values_mut().for_each(|t| t.expire(now));
                trackers.keys().copied().collect()
            }
        };
        for mac in touched {
            let Some(tracker) = trackers.get(&mac) else {
                continue;
            };
            let snapshot = tracker.snapshot();
            let mac_s = mac.to_string();
            if let (Some(level), true) = (snapshot.case.level, snapshot.case.present) {
                let at = snapshot
                    .case
                    .last_seen
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map_or(0, |d| d.as_secs());
                let changed = case_cache
                    .get(&mac_s)
                    .is_none_or(|c| c.level != level || at.saturating_sub(c.at) >= 600);
                if changed {
                    case_cache.insert(mac_s.clone(), CachedCase { level, at });
                    if let Ok(json) = serde_json::to_string(&case_cache)
                        && let Err(e) = crate::store::write_atomic(&cache_path, &json)
                    {
                        debug!("could not save case battery: {e}");
                    }
                }
            }
            // BLE adverts from AirPods we have never connected to are ignored: le.rs only resolves
            // addresses for devices whose keys we stored, so every MAC here is a known device.
            hub.update_device(&mac_s, &known_name(&hub, &mac_s), |d| {
                if d.battery != snapshot {
                    debug!("battery {} -> {:?}", mac_s, snapshot);
                    d.battery = snapshot;
                }
                // Keep "last seen" roughly current for nearby-but-disconnected AirPods without
                // waking every subscriber on each 1 s advert.
                let now = std::time::SystemTime::now();
                let old = d
                    .last_seen
                    .and_then(|t| now.duration_since(t).ok())
                    .is_none_or(|age| age.as_secs() >= 30);
                if d.link == LinkState::Disconnected && old {
                    d.last_seen = Some(now);
                }
            });
        }
    }
}

fn set_model_if_unknown(hub: &Hub, mac: Address, kind: ModelKind) {
    let mac_s = mac.to_string();
    hub.update_device(&mac_s, &known_name(hub, &mac_s), |d| {
        if d.model == ModelKind::Unknown {
            d.model = kind;
        }
    });
}

/// Name saved in devices.json, used only when the hub hasn't seen this device yet (adverts
/// come every second; the file is not read again for a device the hub already has).
fn known_name(hub: &Hub, mac: &str) -> String {
    if hub.knows(mac) {
        return String::new();
    }
    crate::devices::airpods::saved_device_name(mac).unwrap_or_else(|| "AirPods".to_string())
}

#[cfg(test)]
#[path = "battery_service_tests.rs"]
mod service_tests;
