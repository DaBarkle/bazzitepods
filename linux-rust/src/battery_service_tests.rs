use super::*;
use crate::bluetooth::aacp::{BatteryComponent, BatteryStatus};
use crate::core::battery::BatterySnapshot;

const MAC: &str = "AA:BB:CC:DD:EE:01";

fn temp_cache(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kase-battery-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("case_battery.json")
}

fn start(hub: &Hub, cache: &Path) -> mpsc::UnboundedSender<BatteryInput> {
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(run(hub.clone(), rx, cache.to_path_buf()));
    tx
}

fn info(component: BatteryComponent, level: u8, status: BatteryStatus) -> BatteryInfo {
    BatteryInfo {
        component,
        level,
        status,
    }
}

/// Waits (up to 2 s) for the hub to show a battery state that passes `ok`.
async fn battery_where(hub: &Hub, ok: impl Fn(&BatterySnapshot) -> bool) -> BatterySnapshot {
    for _ in 0..200 {
        if let Some(d) = hub.snapshot().devices.get(MAC)
            && ok(&d.battery)
        {
            return d.battery.clone();
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("battery never reached the expected state");
}

#[tokio::test]
async fn aacp_levels_reach_the_hub() {
    let hub = Hub::new();
    let tx = start(&hub, &temp_cache("aacp"));
    let mac: Address = MAC.parse().unwrap();
    tx.send(BatteryInput::AacpLink(mac, true)).unwrap();
    tx.send(BatteryInput::Aacp(
        mac,
        vec![
            info(BatteryComponent::Left, 82, BatteryStatus::NotCharging),
            info(BatteryComponent::Right, 77, BatteryStatus::NotCharging),
        ],
    ))
    .unwrap();
    let b = battery_where(&hub, |b| b.left.level.is_some() && b.right.level.is_some()).await;
    assert_eq!(b.left.level, Some(82));
    assert_eq!(b.right.level, Some(77));
}

#[tokio::test]
async fn case_level_is_saved_and_restored_as_last_seen() {
    let cache = temp_cache("case");
    let mac: Address = MAC.parse().unwrap();
    {
        let hub = Hub::new();
        let tx = start(&hub, &cache);
        tx.send(BatteryInput::AacpLink(mac, true)).unwrap();
        tx.send(BatteryInput::Aacp(
            mac,
            vec![info(BatteryComponent::Case, 53, BatteryStatus::NotCharging)],
        ))
        .unwrap();
        battery_where(&hub, |b| b.case.level == Some(53)).await;
        for _ in 0..200 {
            if cache.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
    let saved: HashMap<String, CachedCase> =
        serde_json::from_str(&std::fs::read_to_string(&cache).unwrap()).unwrap();
    assert_eq!(saved.get(MAC).map(|c| c.level), Some(53));

    // A fresh start (no packets yet) shows the remembered level as stale.
    let hub = Hub::new();
    let tx = start(&hub, &cache);
    tx.send(BatteryInput::LinkLost(mac)).unwrap();
    let b = battery_where(&hub, |b| b.case.level.is_some()).await;
    assert_eq!(b.case.level, Some(53));
    assert!(b.case.stale);
}

#[tokio::test]
async fn advert_product_id_fills_in_an_unknown_model() {
    let hub = Hub::new();
    let tx = start(&hub, &temp_cache("model"));
    let mac: Address = MAC.parse().unwrap();
    tx.send(BatteryInput::Ble(
        mac,
        BleBatteryReading {
            left: Some((60, false)),
            right: Some((61, false)),
            product_id: Some(0x1920),
            ..Default::default()
        },
    ))
    .unwrap();
    battery_where(&hub, |b| b.left.level == Some(60)).await;
    let model = hub.snapshot().devices.get(MAC).map(|d| d.model);
    assert_eq!(model, Some(ModelKind::AirPods4));
}
