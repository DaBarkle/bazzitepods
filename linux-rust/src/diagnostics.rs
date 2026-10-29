//! The Diagnostics section in Settings: what Bluetooth and audio look like on this PC, so a
//! problem report (or the user) can see at a glance what's missing.

use crate::bluetooth::le::{self, LeMode};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Diagnostics {
    /// Adapter name (hci0) and whether it's powered; None if there's no adapter.
    pub adapter: Option<(String, bool)>,
    /// Blocked by rfkill (airplane mode, a hardware switch or the BIOS).
    pub blocked: Option<bool>,
    /// A known-troublesome Bluetooth chip, from the adapter's USB id.
    pub chip_warning: Option<&'static str>,
    pub le: Option<LeMode>,
}

/// USB vendor and product id from a sysfs modalias (`usb:v0BDAp8771d0200...`).
pub fn usb_ids(modalias: &str) -> Option<(u16, u16)> {
    let rest = modalias.trim().strip_prefix("usb:v")?;
    let vid = u16::from_str_radix(rest.get(..4)?, 16).ok()?;
    let pid = u16::from_str_radix(rest.get(4..)?.strip_prefix('p')?.get(..4)?, 16).ok()?;
    Some((vid, pid))
}

/// Bluetooth chips with known trouble keeping AirPods connected.
pub fn known_problem(vid: u16, pid: u16) -> Option<&'static str> {
    match (vid, pid) {
        // Realtek RTL8761B/BU, sold under many brands (TP-Link UB500, ASUS BT500, UGREEN).
        (0x0bda, 0x8771) | (0x2357, 0x0604) | (0x0b05, 0x190e) | (0x2550, 0x8761) => Some(
            "Realtek RTL8761B dongle: known for audio dropouts and failed reconnects with \
             AirPods. Keep linux-firmware up to date, or try a different adapter.",
        ),
        _ => None,
    }
}

/// What the codec line says, from the audio state the guard reports.
pub fn codec_note(audio: Option<&str>) -> String {
    match audio {
        Some(a) if a.to_ascii_uppercase().starts_with("AAC") => "AAC".into(),
        Some(a) if a.to_ascii_uppercase().starts_with("SBC") => format!(
            "{a} only. Your distribution's Bluetooth audio has no AAC (common on Ubuntu, \
             Debian and openSUSE); music still plays, at slightly lower quality."
        ),
        Some(a) => a.to_string(),
        None => "Connect your AirPods to check".into(),
    }
}

pub fn le_note(mode: LeMode) -> &'static str {
    match mode {
        LeMode::Monitor => "Advertisement monitor (battery while away)",
        LeMode::Discovery => {
            "Fallback scanning: bluetoothd's experimental features are off, so battery while \
             away updates less often"
        }
        LeMode::NotRunning => "Not running",
    }
}

fn sysfs(path: &str) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

/// Reads the adapter the AirPods use, its rfkill state and chip.
pub async fn collect() -> Diagnostics {
    let mut d = Diagnostics {
        le: Some(le::mode()),
        ..Default::default()
    };
    let Ok(session) = bluer::Session::new().await else {
        return d;
    };
    let Ok(adapter) = crate::bluetooth::adapter::airpods_adapter(&session).await else {
        return d;
    };
    let name = adapter.name().to_string();
    d.adapter = Some((name.clone(), adapter.is_powered().await.unwrap_or(false)));
    let dir = format!("/sys/class/bluetooth/{name}");
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for e in entries.flatten() {
            let p = e.path();
            if p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("rfkill"))
            {
                let on = |f: &str| {
                    sysfs(&format!("{}/{f}", p.display())).is_some_and(|v| v.trim() == "1")
                };
                d.blocked = Some(on("soft") || on("hard"));
            }
        }
    }
    d.chip_warning = sysfs(&format!("{dir}/device/modalias"))
        .and_then(|m| usb_ids(&m))
        .and_then(|(v, p)| known_problem(v, p));
    d
}

#[cfg(test)]
#[path = "diagnostics_tests.rs"]
mod tests;
