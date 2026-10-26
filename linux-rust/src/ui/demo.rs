//! `--ui-demo[=scenario]`: seeds the hub with fake state so the UI can be previewed and
//! screenshotted without Bluetooth. Scenarios: airpods4 (default), pro, max, low, disconnected,
//! incase, putaway, empty, btoff.

use crate::core::battery::ComponentBattery;
use crate::core::models::ModelKind;
use crate::hub::{AdapterState, Hub, LinkState};
use std::time::{Duration, SystemTime};

fn comp(level: u8, charging: bool) -> ComponentBattery {
    ComponentBattery {
        level: Some(level),
        charging,
        present: true,
        stale: false,
        last_seen: Some(SystemTime::now()),
    }
}

fn stale(level: u8, mins: u64) -> ComponentBattery {
    ComponentBattery {
        level: Some(level),
        charging: false,
        present: false,
        stale: true,
        last_seen: SystemTime::now().checked_sub(Duration::from_secs(mins * 60)),
    }
}

pub fn seed(hub: &Hub, scenario: &str) {
    let mac = "DE:MO:00:00:00:01";
    if scenario == "btoff" {
        hub.set_adapter(AdapterState::Off);
        return;
    }
    hub.set_adapter(AdapterState::On);
    if scenario == "empty" {
        return;
    }
    hub.update_device(mac, "David's AirPods", |d| {
        d.model = ModelKind::AirPods4;
        d.model_number = Some("A3053".into());
        d.link = LinkState::Connected;
        d.audio = Some("AAC".into());
        d.battery.left = comp(80, false);
        d.battery.right = comp(75, false);
        d.battery.case = comp(40, true);
        // Settings as the AirPods report them over the control channel.
        for (id, value) in [(0x0A, 0x01), (0x26, 0x01), (0x01, 0x00), (0x20, 0x02)] {
            d.controls.insert(id, value);
        }
        match scenario {
            "pro" => {
                d.name = "David's AirPods Pro".into();
                d.model = ModelKind::AirPodsPro2;
                d.model_number = Some("A3048".into());
                d.listening_mode = Some(0x04);
                d.allow_off_mode = Some(false);
                d.conversation_awareness = Some(true);
                d.controls.insert(0x2E, 50);
                d.controls.insert(0x1B, 0x01);
                d.battery.left = comp(64, false);
                d.battery.right = comp(18, false);
                d.battery.case = stale(52, 5);
            }
            "max" => {
                d.name = "David's AirPods Max".into();
                d.model = ModelKind::AirPodsMax;
                d.model_number = Some("A3184".into());
                d.listening_mode = Some(0x02);
                d.battery.left = ComponentBattery::default();
                d.battery.right = ComponentBattery::default();
                d.battery.case = ComponentBattery::default();
                d.battery.headphone = comp(72, true);
            }
            "low" => {
                d.battery.left = comp(9, false);
                d.battery.right = comp(17, false);
                d.battery.case = stale(12, 42);
            }
            "elsewhere" => {
                d.owns_audio = Some(false);
                d.other_devices = 1;
                d.elsewhere_name = Some("iPhone".into());
            }
            "incase" => {
                // In the case on a charger, away from this PC: the adverts are live.
                d.model = ModelKind::AirPods4Anc;
                d.link = LinkState::Disconnected;
                d.audio = None;
                d.controls.clear();
                d.battery.left = comp(71, true);
                d.battery.right = comp(73, true);
                d.battery.case = comp(46, true);
            }
            "putaway" => {
                // Both pods in the closed case: last readings, and a hint to open the lid.
                d.model = ModelKind::AirPods4Anc;
                d.link = LinkState::Disconnected;
                d.in_case = true;
                d.audio = None;
                d.last_seen = SystemTime::now().checked_sub(Duration::from_secs(20 * 60));
                d.battery.left = stale(72, 20);
                d.battery.right = stale(71, 20);
                d.battery.case = stale(71, 20);
            }
            "disconnected" => {
                d.link = LinkState::Disconnected;
                d.audio = None;
                d.last_seen = SystemTime::now().checked_sub(Duration::from_secs(90 * 60));
                d.battery.left = stale(62, 90);
                d.battery.right = stale(60, 90);
                d.battery.case = stale(35, 90);
            }
            _ => {}
        }
    });
}
