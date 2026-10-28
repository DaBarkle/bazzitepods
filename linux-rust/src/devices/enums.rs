use crate::devices::airpods::AirPodsInformation;
use serde::{Deserialize, Serialize};
use std::fmt::Display;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum DeviceType {
    AirPods,
    /// A Nothing Ear entry saved by LibrePods. Kase no longer supports these devices, but
    /// the variant stays so a `devices.json` holding one still loads (with its AirPods keys).
    Nothing,
}

impl Display for DeviceType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DeviceType::AirPods => write!(f, "AirPods"),
            DeviceType::Nothing => write!(f, "Nothing"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data")]
// Nearly every entry is AirPods; boxing it to shrink the rare legacy variant buys nothing.
#[allow(clippy::large_enum_variant)]
pub enum DeviceInformation {
    AirPods(AirPodsInformation),
    /// Kept as-is from a LibrePods `devices.json`; see [`DeviceType::Nothing`].
    Nothing(serde_json::Value),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceData {
    pub name: String,
    pub type_: DeviceType,
    pub information: Option<DeviceInformation>,
}
