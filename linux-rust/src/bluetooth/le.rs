//! BLE proximity-pairing adverts -> [`BleBatteryReading`].
//!
//! Purely a battery/proximity source: this module never connects, never changes the
//! adapter power state and never shells out. Adverts from resolvable private
//! addresses are matched to our AirPods with the IRK stored in devices.json, then the
//! encrypted part is decrypted with the stored encryption key.
//!
//! Advert layout (Apple manufacturer data, company ID 0x004C already stripped):
//! ```text
//! [0]  0x07 proximity pairing type      [1]  length (0x19)
//! [2]  0x01 paired / 0x00 pairing mode  [3..5] product ID (big-endian, e.g. 0x1420)
//! [5]  status flags                     [6]  pod battery nibbles (10% steps)
//! [7]  hi: charging flags, lo: case nibble
//! [8]  lid: bit3 = closed, bits0-2 = open counter
//! [9]  colour                           [10] connection state
//! [len-16..] AES-128-ECB encrypted: [1],[2] pod levels (primary first), [3] case,
//!            each byte bit7 = charging, bits0-6 = level, 0x7F = unknown.
//! ```
//! Status flags ([5]): bit5 = primary pod is left, bit6 = broadcasting pod is in the
//! case, bit4 = one pod in case, bit2 = both pods in case, bits1/3 = in-ear.
//! Sources: LibrePods Qt `linux/ble/blemanager.cpp`, `linux/battery.hpp`; Android
//! `BLEManager.kt`.

use crate::core::battery::BleBatteryReading;
use crate::core::models::ModelInfo;
use crate::devices::enums::{DeviceData, DeviceInformation, DeviceType};
use crate::utils::{ah, get_devices_path};
use aes::Aes128;
use aes::cipher::Array;
use aes::cipher::{BlockCipherDecrypt, KeyInit};
use bluer::monitor::{Monitor, MonitorEvent, Pattern};
use bluer::{Address, Session};
use futures::StreamExt;
use log::{debug, info, warn};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime};
use tokio::sync::mpsc::UnboundedSender;
use tokio::task::{AbortHandle, JoinSet};

const APPLE_COMPANY_ID: u16 = 0x004C;
const PROXIMITY_PAIRING_TYPE: u8 = 0x07;
const HEADER_LEN: usize = 11;
const ENCRYPTED_LEN: usize = 16;
const BACKOFF_MIN: Duration = Duration::from_secs(2);
const BACKOFF_MAX: Duration = Duration::from_secs(60);
/// devices.json is re-checked at most this often.
const KEY_RELOAD_INTERVAL: Duration = Duration::from_secs(2);

pub type BleReadingSender = UnboundedSender<(Address, BleBatteryReading)>;

fn decrypt(key: &[u8; 16], data: &[u8; 16]) -> [u8; 16] {
    let cipher = Aes128::new(&Array::from(*key));
    let mut block = Array::from(*data);
    cipher.decrypt_block(&mut block);
    block.into()
}

/// True if `addr` is a resolvable private address generated from `irk`.
pub fn verify_rpa(addr: Address, irk: &[u8; 16]) -> bool {
    // RPAs have the two most significant bits set to 0b01.
    if addr.0[0] >> 6 != 0b01 {
        return false;
    }
    let mut rpa = addr.0;
    rpa.reverse(); // little-endian: hash = bytes 0..3, prand = bytes 3..6
    let hash = [rpa[0], rpa[1], rpa[2]];
    let prand = [rpa[3], rpa[4], rpa[5]];
    ah(irk, &prand) == hash
}

/// Decode a battery byte from the encrypted block. `None` for 0x7F/0xFF/>100.
fn decode_level_byte(b: u8) -> Option<(u8, bool)> {
    let level = b & 0x7F;
    (level <= 100).then_some((level, b & 0x80 != 0))
}

/// Decode a 4-bit battery nibble from the clear-text header (10% steps).
fn decode_nibble(n: u8) -> Option<u8> {
    match n {
        0x0..=0x9 => Some(n * 10),
        0xA..=0xE => Some(100),
        _ => None,
    }
}

/// Decode a proximity-pairing advert. `decrypted` is the decrypted last 16 bytes
/// (if an encryption key is known); without it the coarse 10% clear-text nibbles
/// are used. Returns `None` for non-proximity, pairing-mode or truncated adverts.
pub fn decode_proximity(data: &[u8], decrypted: Option<&[u8; 16]>) -> Option<BleBatteryReading> {
    if data.len() < HEADER_LEN || data[0] != PROXIMITY_PAIRING_TYPE || data[2] == 0x00 {
        return None;
    }
    let product_id = u16::from_be_bytes([data[3], data[4]]);
    let status = data[5];
    let pods_nibbles = data[6];
    let flags_case = data[7];
    let lid = data[8];

    let primary_left = status & 0x20 != 0;
    let this_in_case = status & 0x40 != 0;
    let one_in_case = status & 0x10 != 0;
    let both_in_case = status & 0x04 != 0;
    let any_in_case = this_in_case || one_in_case || both_in_case;

    let mut r = BleBatteryReading {
        product_id: Some(product_id),
        ..Default::default()
    };

    if ModelInfo::from_ble_product_id(product_id).is_headphone {
        // AirPods Max: the single battery is in whichever slot is valid.
        r.headphone = match decrypted {
            Some(d) => [d[1], d[2], d[3]].into_iter().find_map(decode_level_byte),
            None => {
                let charging = (flags_case >> 4) & 0x01 != 0;
                [pods_nibbles & 0x0F, pods_nibbles >> 4]
                    .into_iter()
                    .find_map(decode_nibble)
                    .map(|l| (l, charging))
            }
        };
        return Some(r);
    }

    // Primary pod's value comes first; flipped when the right pod is primary.
    let flipped = !primary_left;
    let (left, right, case) = match decrypted {
        Some(d) => {
            let (li, ri) = if flipped { (2, 1) } else { (1, 2) };
            (
                decode_level_byte(d[li]),
                decode_level_byte(d[ri]),
                decode_level_byte(d[3]),
            )
        }
        None => {
            let flags = flags_case >> 4;
            let (ln, rn) = if flipped {
                (pods_nibbles >> 4, pods_nibbles & 0x0F)
            } else {
                (pods_nibbles & 0x0F, pods_nibbles >> 4)
            };
            let (lc, rc) = if flipped {
                (flags & 0x02 != 0, flags & 0x01 != 0)
            } else {
                (flags & 0x01 != 0, flags & 0x02 != 0)
            };
            (
                decode_nibble(ln).map(|l| (l, lc)),
                decode_nibble(rn).map(|l| (l, rc)),
                decode_nibble(flags_case & 0x0F).map(|l| (l, flags & 0x04 != 0)),
            )
        }
    };
    r.left = left;
    r.right = right;

    // In-ear bits (same mapping as the Qt app).
    let xor = primary_left ^ this_in_case;
    let left_in_ear = if xor {
        status & 0x02 != 0
    } else {
        status & 0x08 != 0
    };
    let right_in_ear = if xor {
        status & 0x08 != 0
    } else {
        status & 0x02 != 0
    };
    r.left_in_ear = Some(left_in_ear);
    r.right_in_ear = Some(right_in_ear);

    // Which pod is in the case. Best effort: both-bit is exact; with one pod in the
    // case, a pod that is in an ear cannot be the one in the case; otherwise assume
    // the broadcasting (primary) pod.
    if both_in_case {
        r.left_in_case = true;
        r.right_in_case = true;
    } else if any_in_case {
        let left_is_in_case = match (left_in_ear, right_in_ear) {
            (true, false) => false,
            (false, true) => true,
            _ => primary_left,
        };
        r.left_in_case = left_is_in_case;
        r.right_in_case = !left_is_in_case;
    }

    // The case only reports while a pod is inside it.
    if any_in_case {
        r.case = case;
        r.lid_open = Some(lid & 0x08 == 0);
    }
    Some(r)
}

/// Decrypt (if possible) and decode a full Apple manufacturer-data payload.
pub fn decode_advert(data: &[u8], enc_key: Option<&[u8; 16]>) -> Option<BleBatteryReading> {
    let decrypted = match enc_key {
        Some(key) if data.len() >= HEADER_LEN + ENCRYPTED_LEN => {
            let block: [u8; 16] = data[data.len() - ENCRYPTED_LEN..].try_into().ok()?;
            Some(decrypt(key, &block))
        }
        _ => None,
    };
    decode_proximity(data, decrypted.as_ref())
}

#[derive(Debug, Clone)]
struct DeviceKeys {
    public: Address,
    irk: [u8; 16],
    enc_key: Option<[u8; 16]>,
}

fn decode_key(hex_str: &str) -> Option<[u8; 16]> {
    hex::decode(hex_str).ok()?.as_slice().try_into().ok()
}

/// Extract usable LE keys from a devices.json map.
fn keys_from_devices(devices: &HashMap<String, DeviceData>) -> Vec<DeviceKeys> {
    devices
        .iter()
        .filter(|(_, d)| d.type_ == DeviceType::AirPods)
        .filter_map(|(mac, d)| {
            let Some(DeviceInformation::AirPods(info)) = &d.information else {
                return None;
            };
            Some(DeviceKeys {
                public: mac.parse().ok()?,
                irk: decode_key(&info.le_keys.irk)?,
                enc_key: decode_key(&info.le_keys.enc_key),
            })
        })
        .collect()
}

type EncKeys = Arc<RwLock<HashMap<Address, [u8; 16]>>>;

/// IRK matching with a cache, reloaded whenever devices.json's mtime changes.
struct KeyStore {
    path: PathBuf,
    mtime: Option<SystemTime>,
    last_check: Option<tokio::time::Instant>,
    keys: Vec<DeviceKeys>,
    verified: HashMap<Address, Address>,
    failed: HashSet<Address>,
    enc_keys: EncKeys,
}

impl KeyStore {
    fn new(path: PathBuf) -> Self {
        KeyStore {
            path,
            mtime: None,
            last_check: None,
            keys: Vec::new(),
            verified: HashMap::new(),
            failed: HashSet::new(),
            enc_keys: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    async fn refresh_if_changed(&mut self) {
        let now = tokio::time::Instant::now();
        if self
            .last_check
            .is_some_and(|t| now.duration_since(t) < KEY_RELOAD_INTERVAL)
        {
            return;
        }
        self.last_check = Some(now);
        let mtime = tokio::fs::metadata(&self.path)
            .await
            .and_then(|m| m.modified())
            .ok();
        if mtime == self.mtime && (mtime.is_some() || self.keys.is_empty()) {
            return;
        }
        self.mtime = mtime;
        let devices: HashMap<String, DeviceData> = match tokio::fs::read_to_string(&self.path).await
        {
            Ok(s) => serde_json::from_str(&s).unwrap_or_else(|e| {
                warn!("Could not parse devices file for LE keys: {}", e);
                HashMap::new()
            }),
            Err(_) => HashMap::new(),
        };
        self.set_keys(keys_from_devices(&devices));
    }

    fn set_keys(&mut self, keys: Vec<DeviceKeys>) {
        info!("Loaded LE keys for {} AirPods", keys.len());
        let enc: HashMap<Address, [u8; 16]> = keys
            .iter()
            .filter_map(|k| Some((k.public, k.enc_key?)))
            .collect();
        match self.enc_keys.write() {
            Ok(mut g) => *g = enc,
            Err(poisoned) => *poisoned.into_inner() = enc,
        }
        self.keys = keys;
        self.verified.clear();
        self.failed.clear();
    }

    /// Resolve an advertising address to the public MAC of one of our AirPods.
    fn resolve(&mut self, addr: Address) -> Option<Address> {
        if let Some(public) = self.verified.get(&addr) {
            return Some(*public);
        }
        if self.failed.contains(&addr) {
            return None;
        }
        match self.keys.iter().find(|k| verify_rpa(addr, &k.irk)) {
            Some(k) => {
                info!("Matched BLE address {} to AirPods {}", addr, k.public);
                self.verified.insert(addr, k.public);
                Some(k.public)
            }
            None => {
                // Bound the negative cache; RPAs rotate every ~15 minutes.
                if self.failed.len() > 4096 {
                    self.failed.clear();
                }
                self.failed.insert(addr);
                None
            }
        }
    }
}

fn enc_key_for(enc_keys: &EncKeys, public: Address) -> Option<[u8; 16]> {
    match enc_keys.read() {
        Ok(g) => g.get(&public).copied(),
        Err(poisoned) => poisoned.into_inner().get(&public).copied(),
    }
}

fn handle_manufacturer_data(
    data: &HashMap<u16, Vec<u8>>,
    public: Address,
    enc_keys: &EncKeys,
    tx: &BleReadingSender,
) -> bool {
    let Some(apple) = data.get(&APPLE_COMPANY_ID) else {
        return true;
    };
    let enc_key = enc_key_for(enc_keys, public);
    if let Some(reading) = decode_advert(apple, enc_key.as_ref()) {
        debug!("BLE reading for {}: {:?}", public, reading);
        if tx.send((public, reading)).is_err() {
            return false;
        }
    }
    true
}

/// Watch one matched device's manufacturer data until it goes away.
async fn watch_device(
    dev: bluer::Device,
    public: Address,
    enc_keys: EncKeys,
    tx: BleReadingSender,
) {
    if let Ok(Some(data)) = dev.manufacturer_data().await
        && !handle_manufacturer_data(&data, public, &enc_keys, &tx)
    {
        return;
    }
    let mut events = match dev.events().await {
        Ok(e) => e,
        Err(e) => {
            debug!("Could not subscribe to events for {}: {}", dev.address(), e);
            return;
        }
    };
    while let Some(ev) = events.next().await {
        if let bluer::DeviceEvent::PropertyChanged(bluer::DeviceProperty::ManufacturerData(data)) =
            ev
            && !handle_manufacturer_data(&data, public, &enc_keys, &tx)
        {
            return;
        }
    }
}

/// Run the BLE monitor forever, sending decoded readings keyed by the AirPods'
/// public MAC. Adapter errors (missing, powered off, BlueZ restart) are retried with
/// exponential backoff. Returns only when the receiver is dropped.
pub async fn start_le_monitor(tx: BleReadingSender) -> bluer::Result<()> {
    let mut backoff = BACKOFF_MIN;
    let mut keys = KeyStore::new(get_devices_path());
    loop {
        let started = tokio::time::Instant::now();
        match run_monitor(&tx, &mut keys).await {
            Ok(()) => {
                info!("LE monitor stopped: reading receiver closed");
                return Ok(());
            }
            Err(e) => {
                if started.elapsed() > BACKOFF_MAX {
                    backoff = BACKOFF_MIN;
                }
                MODE.store(LeMode::NotRunning as u8, Ordering::Relaxed);
                warn!("LE monitor unavailable ({}); retrying in {:?}", e, backoff);
            }
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(BACKOFF_MAX);
    }
}

/// How battery adverts are being read, for the Diagnostics section.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeMode {
    NotRunning = 0,
    /// bluetoothd's advertisement monitor (needs its experimental features).
    Monitor = 1,
    /// Plain discovery, only while the AirPods are away.
    Discovery = 2,
}

static MODE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

pub fn mode() -> LeMode {
    match MODE.load(Ordering::Relaxed) {
        1 => LeMode::Monitor,
        2 => LeMode::Discovery,
        _ => LeMode::NotRunning,
    }
}

async fn run_monitor(tx: &BleReadingSender, keys: &mut KeyStore) -> bluer::Result<()> {
    let session = Session::new().await?;
    let adapter = crate::bluetooth::adapter::airpods_adapter(&session).await?;
    if !adapter.is_powered().await? {
        return Err(bluer::Error {
            kind: bluer::ErrorKind::NotReady,
            message: "adapter is powered off".into(),
        });
    }

    let pattern = Pattern {
        data_type: 0xFF, // Manufacturer specific data
        start_position: 0,
        content: vec![0x4C, 0x00], // Apple company ID, little-endian
    };
    let registered = match adapter.monitor().await {
        Ok(mm) => {
            mm.register(Monitor {
                monitor_type: bluer::monitor::Type::OrPatterns,
                patterns: Some(vec![pattern]),
                ..Default::default()
            })
            .await
        }
        Err(e) => Err(e),
    };
    let mut monitor_handle = match registered {
        Ok(h) => h,
        Err(e) if monitor_unsupported(&e) => return run_discovery(&adapter, tx, keys).await,
        Err(e) => return Err(e),
    };
    debug!("Started LE monitor on {}", adapter.name());
    MODE.store(LeMode::Monitor as u8, Ordering::Relaxed);

    let mut tasks: JoinSet<()> = JoinSet::new();
    let mut watching: HashMap<Address, AbortHandle> = HashMap::new();
    let mut power_check = tokio::time::interval(Duration::from_secs(10));

    loop {
        tokio::select! {
            ev = monitor_handle.next() => {
                let Some(ev) = ev else {
                    return Err(bluer::Error {
                        kind: bluer::ErrorKind::NotReady,
                        message: "advertisement monitor ended".into(),
                    });
                };
                match ev {
                    MonitorEvent::DeviceFound(devid) => {
                        let addr = devid.device;
                        if watching.get(&addr).is_some_and(|h| !h.is_finished()) {
                            continue;
                        }
                        keys.refresh_if_changed().await;
                        let Some(public) = keys.resolve(addr) else {
                            continue;
                        };
                        let dev = match adapter.device(addr) {
                            Ok(d) => d,
                            Err(e) => {
                                debug!("No device object for {}: {}", addr, e);
                                continue;
                            }
                        };
                        let handle = tasks.spawn(watch_device(
                            dev,
                            public,
                            keys.enc_keys.clone(),
                            tx.clone(),
                        ));
                        watching.insert(addr, handle);
                    }
                    MonitorEvent::DeviceLost(devid) => {
                        if let Some(h) = watching.remove(&devid.device) {
                            h.abort();
                        }
                    }
                    _ => {}
                }
            }
            _ = power_check.tick() => {
                if tx.is_closed() {
                    return Ok(());
                }
                while tasks.try_join_next().is_some() {}
                watching.retain(|_, h| !h.is_finished());
                if !adapter.is_powered().await.unwrap_or(false) {
                    return Err(bluer::Error {
                        kind: bluer::ErrorKind::NotReady,
                        message: "adapter was powered off".into(),
                    });
                }
            }
        }
    }
}

/// AdvertisementMonitorManager1 only exists when bluetoothd runs with experimental
/// features (`-E`), which Bazzite and Fedora don't enable by default.
fn monitor_unsupported(e: &bluer::Error) -> bool {
    if matches!(e.kind, bluer::ErrorKind::NotSupported) {
        return true;
    }
    let text = format!("{e} {e:?}");
    [
        "doesn't exist",
        "UnknownMethod",
        "UnknownInterface",
        "UnknownObject",
    ]
    .iter()
    .any(|needle| text.contains(needle))
}

/// True while any of our AirPods has a classic link to this PC. AACP then supplies
/// battery, and an LE scan would only compete with A2DP for radio time.
async fn any_connected(adapter: &bluer::Adapter, keys: &KeyStore) -> bool {
    for k in &keys.keys {
        if let Ok(dev) = adapter.device(k.public)
            && dev.is_connected().await.unwrap_or(false)
        {
            return true;
        }
    }
    false
}

/// Fallback for bluetoothd without the advertisement monitor API: a plain LE
/// discovery session, run only while all of our AirPods are away from this PC
/// (that is when adverts matter: walk-away reconnect, case battery). Discovery
/// never connects or pairs anything.
async fn run_discovery(
    adapter: &bluer::Adapter,
    tx: &BleReadingSender,
    keys: &mut KeyStore,
) -> bluer::Result<()> {
    info!("bluetoothd has no LE advertisement monitor; scanning only while AirPods are away");
    MODE.store(LeMode::Discovery as u8, Ordering::Relaxed);
    type Scan = std::pin::Pin<Box<dyn futures::Stream<Item = bluer::AdapterEvent> + Send>>;
    let mut scan: Option<Scan> = None;
    let mut tasks: JoinSet<()> = JoinSet::new();
    let mut watching: HashMap<Address, AbortHandle> = HashMap::new();
    let mut check = tokio::time::interval(Duration::from_secs(10));

    loop {
        tokio::select! {
            ev = async {
                match scan.as_mut() {
                    Some(s) => s.next().await,
                    None => std::future::pending().await,
                }
            } => {
                match ev {
                    Some(bluer::AdapterEvent::DeviceAdded(addr)) => {
                        if watching.get(&addr).is_some_and(|h| !h.is_finished()) {
                            continue;
                        }
                        let Some(public) = keys.resolve(addr) else {
                            continue;
                        };
                        let Ok(dev) = adapter.device(addr) else {
                            continue;
                        };
                        let handle = tasks.spawn(watch_device(
                            dev,
                            public,
                            keys.enc_keys.clone(),
                            tx.clone(),
                        ));
                        watching.insert(addr, handle);
                    }
                    Some(bluer::AdapterEvent::DeviceRemoved(addr)) => {
                        if let Some(h) = watching.remove(&addr) {
                            h.abort();
                        }
                    }
                    Some(_) => {}
                    // Discovery was stopped under us; the next check restarts it.
                    None => scan = None,
                }
            }
            _ = check.tick() => {
                if tx.is_closed() {
                    return Ok(());
                }
                while tasks.try_join_next().is_some() {}
                watching.retain(|_, h| !h.is_finished());
                if !adapter.is_powered().await.unwrap_or(false) {
                    return Err(bluer::Error {
                        kind: bluer::ErrorKind::NotReady,
                        message: "adapter was powered off".into(),
                    });
                }
                keys.refresh_if_changed().await;
                let wanted = !keys.keys.is_empty() && !any_connected(adapter, keys).await;
                if wanted && scan.is_none() {
                    adapter
                        .set_discovery_filter(bluer::DiscoveryFilter {
                            transport: bluer::DiscoveryTransport::Le,
                            duplicate_data: true,
                            ..Default::default()
                        })
                        .await?;
                    scan = Some(Box::pin(adapter.discover_devices().await?));
                    debug!("LE discovery started");
                } else if !wanted && scan.is_some() {
                    scan = None;
                    for (_, h) in watching.drain() {
                        h.abort();
                    }
                    debug!("LE discovery stopped");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aes::cipher::BlockCipherEncrypt;

    fn encrypt(key: &[u8; 16], data: &[u8; 16]) -> [u8; 16] {
        let cipher = Aes128::new(&Array::from(*key));
        let mut block = Array::from(*data);
        cipher.encrypt_block(&mut block);
        block.into()
    }

    /// Clear-text header for an AirPods Pro 2 (0x1420).
    fn header(status: u8, pods: u8, flags_case: u8, lid: u8) -> Vec<u8> {
        vec![
            0x07, 0x19, 0x01, 0x14, 0x20, status, pods, flags_case, lid, 0x00, 0x04,
        ]
    }

    fn advert(hdr: Vec<u8>, key: &[u8; 16], plain: [u8; 16]) -> Vec<u8> {
        let mut v = hdr;
        // pad to the real advert length (27 bytes = 11 header + 16 encrypted)
        v.extend_from_slice(&encrypt(key, &plain));
        v
    }

    const KEY: [u8; 16] = *b"0123456789abcdef";

    #[test]
    fn decrypted_left_primary_both_in_ear() {
        // primary left (bit5), both in ear (bits1,3), nobody in case
        let status = 0x20 | 0x02 | 0x08;
        let mut plain = [0u8; 16];
        plain[1] = 85; // primary = left
        plain[2] = 0x80 | 60; // right charging
        plain[3] = 40; // case: must be ignored (no pod in case)
        let data = advert(header(status, 0x00, 0x00, 0x00), &KEY, plain);
        let r = decode_advert(&data, Some(&KEY)).unwrap();
        assert_eq!(r.product_id, Some(0x1420));
        assert_eq!(r.left, Some((85, false)));
        assert_eq!(r.right, Some((60, true)));
        assert_eq!(r.case, None);
        assert!(!r.left_in_case && !r.right_in_case);
        assert_eq!(r.lid_open, None);
        assert_eq!(r.left_in_ear, Some(true));
        assert_eq!(r.right_in_ear, Some(true));
        assert_eq!(r.headphone, None);
    }

    #[test]
    fn decrypted_right_primary_flips() {
        let status = 0x00; // right primary
        let mut plain = [0u8; 16];
        plain[1] = 33; // primary = right
        plain[2] = 44; // left
        let data = advert(header(status, 0, 0, 0), &KEY, plain);
        let r = decode_advert(&data, Some(&KEY)).unwrap();
        assert_eq!(r.left, Some((44, false)));
        assert_eq!(r.right, Some((33, false)));
    }

    #[test]
    fn sentinels_are_invalid() {
        let status = 0x20 | 0x04; // both in case
        let mut plain = [0u8; 16];
        plain[1] = 0x7F; // 127 unknown
        plain[2] = 0xFF; // 255 unknown
        plain[3] = 0x80 | 101; // >100 invalid even when charging
        let data = advert(header(status, 0, 0, 0), &KEY, plain);
        let r = decode_advert(&data, Some(&KEY)).unwrap();
        assert_eq!(r.left, None);
        assert_eq!(r.right, None);
        assert_eq!(r.case, None);
        assert!(r.left_in_case && r.right_in_case);
    }

    #[test]
    fn case_trusted_with_pod_in_case_and_lid() {
        // left primary, broadcasting pod in case (bit6), one in case (bit4),
        // right in ear. Lid open (bit3 clear).
        let status = 0x20 | 0x40 | 0x10 | 0x08;
        let mut plain = [0u8; 16];
        plain[1] = 0x80 | 90;
        plain[2] = 70;
        plain[3] = 0x80 | 55;
        let data = advert(header(status, 0, 0, 0x01), &KEY, plain);
        let r = decode_advert(&data, Some(&KEY)).unwrap();
        assert_eq!(r.case, Some((55, true)));
        assert_eq!(r.lid_open, Some(true));
        assert!(r.left_in_case || r.right_in_case);
        // lid closed
        let data = advert(header(status, 0, 0, 0x08), &KEY, plain);
        let r = decode_advert(&data, Some(&KEY)).unwrap();
        assert_eq!(r.lid_open, Some(false));
    }

    #[test]
    fn clear_text_fallback_without_key() {
        // left primary: left = low nibble, right = high nibble.
        let status = 0x20 | 0x04;
        // left 8 (80%), right 5 (50%); flags: left charging (bit0), case charging (bit2); case 3
        let data = header(status, 0x58, 0x53, 0x00);
        let r = decode_advert(&data, None).unwrap();
        assert_eq!(r.left, Some((80, true)));
        assert_eq!(r.right, Some((50, false)));
        assert_eq!(r.case, Some((30, true)));
        // 0xF nibble = unknown
        let data = header(status, 0xF8, 0x0F, 0x00);
        let r = decode_advert(&data, None).unwrap();
        assert_eq!(r.right, None);
        assert_eq!(r.case, None);
    }

    #[test]
    fn airpods_max_headphone() {
        let mut hdr = header(0x20, 0, 0, 0);
        hdr[3] = 0x1F; // AirPods Max USB-C 0x1F20
        let mut plain = [0u8; 16];
        plain[1] = 0x7F;
        plain[2] = 0x80 | 72;
        plain[3] = 0x7F;
        let data = advert(hdr, &KEY, plain);
        let r = decode_advert(&data, Some(&KEY)).unwrap();
        assert_eq!(r.headphone, Some((72, true)));
        assert_eq!(r.left, None);
        assert_eq!(r.right, None);
        assert_eq!(r.case, None);
        assert_eq!(r.product_id, Some(0x1F20));
    }

    #[test]
    fn rejects_garbage_without_panicking() {
        assert!(decode_advert(&[], Some(&KEY)).is_none());
        assert!(decode_advert(&[0x07], None).is_none());
        assert!(decode_advert(&[0x10, 0x05, 0x01, 0, 0, 0, 0, 0, 0, 0, 0], None).is_none());
        // pairing mode
        let mut hdr = header(0x20, 0, 0, 0);
        hdr[2] = 0x00;
        assert!(decode_advert(&hdr, None).is_none());
        // header-only advert with a key: falls back to nibbles
        assert!(decode_advert(&header(0x20, 0x55, 0x05, 0), Some(&KEY)).is_some());
        for len in 0..40 {
            let v: Vec<u8> = (0..len).map(|i| (i * 37) as u8).collect();
            let _ = decode_advert(&v, Some(&KEY));
            let _ = decode_advert(&v, None);
        }
    }

    fn make_rpa(irk: &[u8; 16], prand_le: [u8; 3]) -> Address {
        let mut prand = prand_le;
        prand[2] = (prand[2] & 0x3F) | 0x40; // MSB bits 0b01
        let hash = ah(irk, &prand);
        // little-endian bytes: hash(3) then prand(3); Address is big-endian
        let le = [hash[0], hash[1], hash[2], prand[0], prand[1], prand[2]];
        let mut be = le;
        be.reverse();
        Address(be)
    }

    #[test]
    fn rpa_verification() {
        let irk = [7u8; 16];
        let rpa = make_rpa(&irk, [1, 2, 3]);
        assert!(verify_rpa(rpa, &irk));
        assert!(!verify_rpa(rpa, &[8u8; 16]));
        // static random address (0b11) is never an RPA
        assert!(!verify_rpa(Address([0xC0, 0, 0, 0, 0, 0]), &irk));
    }

    #[test]
    fn keystore_reload_clears_negative_cache() {
        let irk = [9u8; 16];
        let rpa = make_rpa(&irk, [4, 5, 6]);
        let public: Address = "AA:BB:CC:DD:EE:FF".parse().unwrap();
        let mut ks = KeyStore::new(PathBuf::from("/nonexistent"));
        assert_eq!(ks.resolve(rpa), None);
        assert!(ks.failed.contains(&rpa));
        ks.set_keys(vec![DeviceKeys {
            public,
            irk,
            enc_key: Some(KEY),
        }]);
        assert!(ks.failed.is_empty());
        assert_eq!(ks.resolve(rpa), Some(public));
        assert_eq!(enc_key_for(&ks.enc_keys, public), Some(KEY));
    }

    #[test]
    fn keys_from_devices_json() {
        let json = r#"{
            "AA:BB:CC:DD:EE:FF": {"name":"x","type_":"AirPods","information":{"kind":"AirPods","data":{
                "name":"x","model_number":"A2698","manufacturer":"Apple","serial_number":"",
                "version1":"","version2":"","hardware_revision":"","updater_identifier":"",
                "left_serial_number":"","right_serial_number":"","version3":"",
                "le_keys":{"irk":"00112233445566778899aabbccddeeff","enc_key":""}}}},
            "11:22:33:44:55:66": {"name":"y","type_":"AirPods","information":null}
        }"#;
        let devices: HashMap<String, DeviceData> = serde_json::from_str(json).unwrap();
        let keys = keys_from_devices(&devices);
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].irk[0], 0x00);
        assert_eq!(keys[0].irk[15], 0xFF);
        assert!(keys[0].enc_key.is_none());
    }
}

#[cfg(test)]
#[path = "le_tests.rs"]
mod advert_tests;
