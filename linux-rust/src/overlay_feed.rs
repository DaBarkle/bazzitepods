//! Feeds the hub into the overlay process, or into desktop notifications where the compositor
//! has no layer-shell (GNOME). The overlay is a separate process drawing its own compositor
//! surface; nothing here ever touches game processes.

use crate::core::battery::ComponentBattery;
use crate::hub::{AppSnapshot, DeviceSnapshot, Hub, LinkState};
use crate::overlay_client::OverlayClient;
use crate::overlay_protocol::{ComponentState, OverlayConfig, OverlayMode, OverlayState};
use crate::utils::get_app_settings_path;
use log::{debug, info};
use std::sync::{Arc, OnceLock};
use std::time::SystemTime;

static CLIENT: OnceLock<Arc<OverlayClient>> = OnceLock::new();

/// Overlay settings as stored in app_settings.json (`overlay_mode`, `overlay_position`).
pub fn load_config() -> OverlayConfig {
    let mut cfg = OverlayConfig::default();
    let Some(v) = std::fs::read_to_string(get_app_settings_path())
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
    else {
        return cfg;
    };
    if let Some(mode) = v.get("overlay_mode").cloned().and_then(|m| serde_json::from_value(m).ok()) {
        cfg.mode = mode;
    }
    if let Some(pos) = v
        .get("overlay_position")
        .cloned()
        .and_then(|p| serde_json::from_value(p).ok())
    {
        cfg.position = pos;
    }
    cfg
}

/// Called by the settings UI after the user changes overlay options.
pub fn apply_config(cfg: OverlayConfig) {
    if let Some(client) = CLIENT.get() {
        client.set_config(cfg);
    }
}

/// Briefly show the overlay (e.g. a "preview" button in settings).
pub fn preview() {
    if let Some(client) = CLIENT.get() {
        client.show("preview");
    }
}

pub fn spawn(hub: Hub) {
    let client = CLIENT.get_or_init(|| Arc::new(OverlayClient::spawn(load_config()))).clone();
    tokio::spawn(run(hub, client));
}

async fn run(hub: Hub, client: Arc<OverlayClient>) {
    let mut rx = hub.subscribe();
    let mut last: Option<OverlayState> = None;
    let mut notifier = Notifier::default();
    loop {
        let snap = rx.borrow_and_update().clone();
        let state = to_overlay_state(&snap);
        if state != last {
            match &state {
                Some(s) => client.update(s.clone()),
                None => client.hide(),
            }
            last = state.clone();
        }
        if client.unsupported() && load_config().mode != OverlayMode::Off {
            notifier.observe(snap.primary());
        }
        if rx.changed().await.is_err() {
            client.shutdown();
            return;
        }
    }
}

/// The featured device as the overlay sees it. Nothing is shown for devices that have never
/// reported a battery level (e.g. a paired but unused headset).
pub fn to_overlay_state(snap: &AppSnapshot) -> Option<OverlayState> {
    let d = snap.primary()?;
    if !d.battery.has_any() {
        return None;
    }
    let now = SystemTime::now();
    let comp = |c: &ComponentBattery| -> Option<ComponentState> {
        let level = c.level?;
        Some(ComponentState {
            level,
            charging: c.charging,
            present: c.present,
            stale: c.stale,
            last_seen_secs: c
                .stale
                .then(|| c.last_seen.and_then(|t| now.duration_since(t).ok()))
                .flatten()
                .map(|age| age.as_secs()),
        })
    };
    let info = d.model.info();
    let kind = serde_json::to_value(d.model)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default();
    Some(OverlayState {
        name: display_name(d),
        kind,
        connected: d.is_connected(),
        left: comp(&d.battery.left),
        right: comp(&d.battery.right),
        case: if info.has_case_battery { comp(&d.battery.case) } else { None },
        headphone: if info.is_headphone { comp(&d.battery.headphone) } else { None },
    })
}

fn display_name(d: &DeviceSnapshot) -> String {
    let model = d.model.info().name;
    // Prefer the friendly model name ("AirPods 4") unless the user renamed them.
    if d.name.is_empty() || d.name.eq_ignore_ascii_case("airpods") {
        model.to_string()
    } else {
        d.name.clone()
    }
}

/// Fallback for compositors without layer-shell: a notification on connect and on low battery.
#[derive(Default)]
struct Notifier {
    connected: Option<String>,
    low_warned: bool,
}

impl Notifier {
    fn observe(&mut self, d: Option<&DeviceSnapshot>) {
        let Some(d) = d.filter(|d| d.link == LinkState::Connected) else {
            self.connected = None;
            return;
        };
        if !d.battery.has_any() {
            return;
        }
        let summary = battery_line(d);
        if self.connected.as_deref() != Some(d.mac.as_str()) {
            self.connected = Some(d.mac.clone());
            self.low_warned = false;
            notify(format!("{} connected", display_name(d)), summary.clone());
        }
        let low = d.battery.is_low(15);
        if low && !self.low_warned {
            self.low_warned = true;
            notify(format!("{} battery low", display_name(d)), summary);
        } else if !low {
            self.low_warned = false;
        }
    }
}

pub fn battery_line(d: &DeviceSnapshot) -> String {
    let fmt = |label: &str, c: &ComponentBattery| match c.level {
        Some(l) if c.present => format!("{label} {l}%{}", if c.charging { " ⚡" } else { "" }),
        _ => format!("{label} —"),
    };
    if d.model.info().is_headphone {
        return fmt("Battery", &d.battery.headphone);
    }
    let mut parts = vec![fmt("L", &d.battery.left), fmt("R", &d.battery.right)];
    if d.model.info().has_case_battery {
        parts.push(fmt("Case", &d.battery.case));
    }
    parts.join("  ·  ")
}

fn notify(summary: String, body: String) {
    tokio::task::spawn_blocking(move || {
        let result = (|| -> Result<(), dbus::Error> {
            let conn = dbus::blocking::Connection::new_session()?;
            let proxy = conn.with_proxy(
                "org.freedesktop.Notifications",
                "/org/freedesktop/Notifications",
                std::time::Duration::from_secs(3),
            );
            let hints: dbus::arg::PropMap = Default::default();
            let _: (u32,) = proxy.method_call(
                "org.freedesktop.Notifications",
                "Notify",
                (
                    "LibrePods",
                    0u32,
                    "io.github.dabarkle.LibrePods",
                    summary.as_str(),
                    body.as_str(),
                    Vec::<String>::new(),
                    hints,
                    4000i32,
                ),
            )?;
            Ok(())
        })();
        if let Err(e) = result {
            debug!("Notification failed: {e}");
        }
    });
    info!("Overlay unsupported; sent notification instead");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::models::ModelKind;

    fn device() -> DeviceSnapshot {
        let mut d = DeviceSnapshot::new("AA".into(), "AirPods".into());
        d.link = LinkState::Connected;
        d.model = ModelKind::AirPods4;
        d.battery.left.level = Some(80);
        d.battery.left.present = true;
        d.battery.right.level = Some(70);
        d.battery.right.present = false;
        d.battery.case.level = Some(40);
        d.battery.case.present = true;
        d.battery.case.charging = true;
        d
    }

    #[test]
    fn maps_primary_device() {
        let mut snap = AppSnapshot::default();
        snap.devices.insert("AA".into(), device());
        let s = to_overlay_state(&snap).expect("state");
        assert!(s.connected);
        assert_eq!(s.kind, "airpods4");
        assert_eq!(s.left.as_ref().map(|c| c.level), Some(80));
        assert_eq!(s.right.as_ref().map(|c| c.present), Some(false));
        assert!(s.case.as_ref().is_some_and(|c| c.charging));
        assert!(s.headphone.is_none());
        assert_eq!(s.name, ModelKind::AirPods4.info().name);
    }

    #[test]
    fn nothing_without_battery() {
        let mut snap = AppSnapshot::default();
        snap.devices
            .insert("AA".into(), DeviceSnapshot::new("AA".into(), "X".into()));
        assert!(to_overlay_state(&snap).is_none());
    }

    #[test]
    fn battery_line_dims_absent_pod() {
        let line = battery_line(&device());
        assert_eq!(line, "L 80%  ·  R —  ·  Case 40% ⚡");
    }
}
