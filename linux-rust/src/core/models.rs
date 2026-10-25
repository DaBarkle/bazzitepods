//! AirPods model table: model numbers (from AACP INFORMATION) and BLE product IDs
//! (from proximity-pairing adverts) mapped to a [`ModelInfo`] describing capabilities.
//!
//! Sources:
//! * Model numbers: Apple "Identify your AirPods" (support.apple.com/109525), checked 2026-10.
//! * BLE product IDs: LibrePods Qt app (`linux/ble/blemanager.cpp`) and Android app
//!   (`BLEManager.kt`). The ID is read big-endian from advert bytes 3..5, i.e. it is
//!   Apple's USB/Bluetooth product ID (e.g. 0x2019) byte-swapped (0x1920).
//! * Capabilities: Android app `data/AirPods.kt`.
//!
//! Unknown models degrade gracefully: [`ModelInfo::UNKNOWN`] shows whatever battery
//! components are reported and hides every optional control.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelKind {
    AirPods1,
    AirPods2,
    AirPods3,
    AirPods4,
    AirPods4Anc,
    AirPods5,
    AirPodsPro,
    AirPodsPro2,
    AirPodsPro3,
    AirPodsMax,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelInfo {
    /// Human readable name, e.g. "AirPods Pro 2".
    pub name: &'static str,
    pub kind: ModelKind,
    /// Over-ear headphones (single "headphone" battery, no pods, no case battery).
    pub is_headphone: bool,
    /// Active noise cancellation / transparency listening modes.
    pub has_anc: bool,
    /// Adaptive listening mode.
    pub has_adaptive: bool,
    pub has_conversation_awareness: bool,
    pub has_case_battery: bool,
    pub has_ear_detection: bool,
    pub has_head_gestures: bool,
    pub has_adaptive_volume: bool,
    pub has_hearing_aid: bool,
}

const fn base(name: &'static str, kind: ModelKind) -> ModelInfo {
    ModelInfo {
        name,
        kind,
        is_headphone: false,
        has_anc: false,
        has_adaptive: false,
        has_conversation_awareness: false,
        has_case_battery: true,
        has_ear_detection: true,
        has_head_gestures: false,
        has_adaptive_volume: false,
        has_hearing_aid: false,
    }
}

impl ModelInfo {
    pub const UNKNOWN: ModelInfo = base("AirPods", ModelKind::Unknown);

    pub const AIRPODS_1: ModelInfo = base("AirPods (1st generation)", ModelKind::AirPods1);
    pub const AIRPODS_2: ModelInfo = base("AirPods (2nd generation)", ModelKind::AirPods2);
    pub const AIRPODS_3: ModelInfo = ModelInfo {
        has_head_gestures: true,
        ..base("AirPods (3rd generation)", ModelKind::AirPods3)
    };
    pub const AIRPODS_4: ModelInfo = ModelInfo {
        has_head_gestures: true,
        has_adaptive_volume: true,
        ..base("AirPods 4", ModelKind::AirPods4)
    };
    pub const AIRPODS_4_ANC: ModelInfo = ModelInfo {
        has_anc: true,
        has_adaptive: true,
        has_conversation_awareness: true,
        has_head_gestures: true,
        has_adaptive_volume: true,
        // Apple calls it "AirPods 4 with Active Noise Cancellation"; a name that
        // mentions ANC reads like noise cancellation is switched on, so keep it short.
        ..base("AirPods 4", ModelKind::AirPods4Anc)
    };
    /// Capabilities follow the upstream Android table; not independently verified.
    pub const AIRPODS_5: ModelInfo = ModelInfo {
        has_anc: true,
        has_adaptive: true,
        has_conversation_awareness: true,
        has_head_gestures: true,
        has_adaptive_volume: true,
        ..base("AirPods 5", ModelKind::AirPods5)
    };
    pub const AIRPODS_PRO: ModelInfo = ModelInfo {
        has_anc: true,
        ..base("AirPods Pro", ModelKind::AirPodsPro)
    };
    pub const AIRPODS_PRO_2: ModelInfo = ModelInfo {
        has_anc: true,
        has_adaptive: true,
        has_conversation_awareness: true,
        has_head_gestures: true,
        has_adaptive_volume: true,
        has_hearing_aid: true,
        ..base("AirPods Pro 2", ModelKind::AirPodsPro2)
    };
    pub const AIRPODS_PRO_3: ModelInfo = ModelInfo {
        name: "AirPods Pro 3",
        kind: ModelKind::AirPodsPro3,
        ..Self::AIRPODS_PRO_2
    };
    pub const AIRPODS_MAX: ModelInfo = ModelInfo {
        is_headphone: true,
        has_anc: true,
        has_case_battery: false,
        ..base("AirPods Max", ModelKind::AirPodsMax)
    };

    /// Look up by Apple model number as reported in the AACP INFORMATION packet
    /// (e.g. "A2698"). Unknown or empty strings return [`ModelInfo::UNKNOWN`].
    pub fn from_model_number(model: &str) -> &'static ModelInfo {
        match model.trim().to_ascii_uppercase().as_str() {
            "A1523" | "A1722" => &Self::AIRPODS_1,
            "A2032" | "A2031" => &Self::AIRPODS_2,
            "A2565" | "A2564" => &Self::AIRPODS_3,
            "A3053" | "A3050" | "A3054" => &Self::AIRPODS_4,
            "A3056" | "A3055" | "A3057" => &Self::AIRPODS_4_ANC,
            // AirPods 5 and AirPods 5 with Wireless Charging Case (2026).
            "A3531" | "A3532" | "A3533" | "A3439" | "A3440" | "A3441" => &Self::AIRPODS_5,
            "A2084" | "A2083" => &Self::AIRPODS_PRO,
            // Lightning (A2931/A2699/A2698) and USB-C (A3047/A3048/A3049).
            "A2931" | "A2699" | "A2698" | "A3047" | "A3048" | "A3049" => &Self::AIRPODS_PRO_2,
            "A3063" | "A3064" | "A3065" => &Self::AIRPODS_PRO_3,
            // Max Lightning, Max USB-C, Max 2.
            "A2096" | "A3184" | "A3454" => &Self::AIRPODS_MAX,
            _ => &Self::UNKNOWN,
        }
    }

    /// Look up by the 16-bit product ID from a BLE proximity-pairing advert
    /// (`(data[3] << 8) | data[4]` of the Apple manufacturer data).
    pub fn from_ble_product_id(id: u16) -> &'static ModelInfo {
        match id {
            0x0220 => &Self::AIRPODS_1,
            0x0F20 => &Self::AIRPODS_2,
            0x1320 => &Self::AIRPODS_3,
            0x1920 => &Self::AIRPODS_4,
            0x1B20 => &Self::AIRPODS_4_ANC,
            0x0E20 => &Self::AIRPODS_PRO,
            0x1420 | 0x2420 => &Self::AIRPODS_PRO_2,
            0x0A20 | 0x1F20 => &Self::AIRPODS_MAX,
            _ => &Self::UNKNOWN,
        }
    }

    pub fn is_known(&self) -> bool {
        self.kind != ModelKind::Unknown
    }
}

impl ModelKind {
    pub fn info(self) -> &'static ModelInfo {
        match self {
            ModelKind::AirPods1 => &ModelInfo::AIRPODS_1,
            ModelKind::AirPods2 => &ModelInfo::AIRPODS_2,
            ModelKind::AirPods3 => &ModelInfo::AIRPODS_3,
            ModelKind::AirPods4 => &ModelInfo::AIRPODS_4,
            ModelKind::AirPods4Anc => &ModelInfo::AIRPODS_4_ANC,
            ModelKind::AirPods5 => &ModelInfo::AIRPODS_5,
            ModelKind::AirPodsPro => &ModelInfo::AIRPODS_PRO,
            ModelKind::AirPodsPro2 => &ModelInfo::AIRPODS_PRO_2,
            ModelKind::AirPodsPro3 => &ModelInfo::AIRPODS_PRO_3,
            ModelKind::AirPodsMax => &ModelInfo::AIRPODS_MAX,
            ModelKind::Unknown => &ModelInfo::UNKNOWN,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn airpods4_model_numbers() {
        for m in ["A3050", "A3053", "A3054"] {
            let info = ModelInfo::from_model_number(m);
            assert_eq!(info.kind, ModelKind::AirPods4, "{m}");
            assert!(!info.has_anc);
        }
        for m in ["A3055", "A3056", "A3057"] {
            let info = ModelInfo::from_model_number(m);
            assert_eq!(info.kind, ModelKind::AirPods4Anc, "{m}");
            assert!(info.has_anc && info.has_adaptive && info.has_conversation_awareness);
        }
    }

    #[test]
    fn other_model_numbers() {
        assert_eq!(ModelInfo::from_model_number("A2698").kind, ModelKind::AirPodsPro2);
        assert_eq!(ModelInfo::from_model_number("A3048").kind, ModelKind::AirPodsPro2);
        assert_eq!(ModelInfo::from_model_number("a3064").kind, ModelKind::AirPodsPro3);
        assert_eq!(ModelInfo::from_model_number(" A2084 ").kind, ModelKind::AirPodsPro);
        assert_eq!(ModelInfo::from_model_number("A1523").kind, ModelKind::AirPods1);
        assert_eq!(ModelInfo::from_model_number("A3531").kind, ModelKind::AirPods5);
        let max = ModelInfo::from_model_number("A3184");
        assert_eq!(max.kind, ModelKind::AirPodsMax);
        assert!(max.is_headphone && !max.has_case_battery);
    }

    #[test]
    fn ble_product_ids() {
        assert_eq!(ModelInfo::from_ble_product_id(0x1920).kind, ModelKind::AirPods4);
        assert_eq!(ModelInfo::from_ble_product_id(0x1B20).kind, ModelKind::AirPods4Anc);
        assert_eq!(ModelInfo::from_ble_product_id(0x2420).kind, ModelKind::AirPodsPro2);
        assert_eq!(ModelInfo::from_ble_product_id(0x1F20).kind, ModelKind::AirPodsMax);
        assert_eq!(ModelInfo::from_ble_product_id(0x0220).kind, ModelKind::AirPods1);
    }

    #[test]
    fn unknown_degrades_gracefully() {
        for info in [
            ModelInfo::from_model_number(""),
            ModelInfo::from_model_number("A9999"),
            ModelInfo::from_ble_product_id(0xBEEF),
        ] {
            assert_eq!(info.kind, ModelKind::Unknown);
            assert!(!info.is_known());
            // Battery stays visible, optional controls hidden.
            assert!(info.has_case_battery);
            assert!(!info.has_anc && !info.has_adaptive && !info.has_conversation_awareness);
            assert!(!info.is_headphone);
        }
    }

    #[test]
    fn serde_names() {
        assert_eq!(serde_json::to_string(&ModelKind::AirPods4).unwrap(), "\"airpods4\"");
        assert_eq!(serde_json::to_string(&ModelKind::AirPods4Anc).unwrap(), "\"airpods4anc\"");
        assert_eq!(serde_json::to_string(&ModelKind::AirPodsPro2).unwrap(), "\"airpodspro2\"");
        assert_eq!(serde_json::to_string(&ModelKind::AirPodsMax).unwrap(), "\"airpodsmax\"");
        let k: ModelKind = serde_json::from_str("\"unknown\"").unwrap();
        assert_eq!(k, ModelKind::Unknown);
    }

    #[test]
    fn kind_info_roundtrip() {
        for k in [
            ModelKind::AirPods1,
            ModelKind::AirPods2,
            ModelKind::AirPods3,
            ModelKind::AirPods4,
            ModelKind::AirPods4Anc,
            ModelKind::AirPods5,
            ModelKind::AirPodsPro,
            ModelKind::AirPodsPro2,
            ModelKind::AirPodsPro3,
            ModelKind::AirPodsMax,
            ModelKind::Unknown,
        ] {
            assert_eq!(k.info().kind, k);
        }
    }
}
