// TEMPORARY STUB: replaced by feat/battery-core on merge.
use crate::bluetooth::aacp::BatteryInfo;
use serde::{Deserialize, Serialize};
use std::time::{Instant, SystemTime};
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ComponentBattery { pub level: Option<u8>, pub charging: bool, pub present: bool, pub stale: bool, pub last_seen: Option<SystemTime> }
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum BatterySource { Aacp, Ble }
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct BatterySnapshot { pub left: ComponentBattery, pub right: ComponentBattery, pub case: ComponentBattery, pub headphone: ComponentBattery, pub source: Option<BatterySource> }
#[derive(Debug, Clone, Default)]
pub struct BleBatteryReading { pub left: Option<(u8,bool)>, pub right: Option<(u8,bool)>, pub case: Option<(u8,bool)>, pub headphone: Option<(u8,bool)>, pub left_in_case: bool, pub right_in_case: bool, pub lid_open: Option<bool>, pub product_id: Option<u16>, pub left_in_ear: Option<bool>, pub right_in_ear: Option<bool> }
#[derive(Debug, Default)]
pub struct BatteryTracker { s: BatterySnapshot }
impl BatteryTracker {
    pub fn new() -> Self { Self::default() }
    pub fn apply_aacp(&mut self, _i: &[BatteryInfo], _now: Instant) {}
    pub fn apply_ble(&mut self, _r: &BleBatteryReading, _now: Instant) {}
    pub fn set_aacp_connected(&mut self, _c: bool) {}
    pub fn snapshot(&self) -> BatterySnapshot { self.s.clone() }
}
