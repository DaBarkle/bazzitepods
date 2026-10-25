// TEMPORARY STUB: replaced by feat/battery-core on merge.
use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelKind { AirPods4, Unknown }
pub struct ModelInfo { pub name: &'static str, pub kind: ModelKind }
pub fn from_model_number(_s: &str) -> Option<&'static ModelInfo> { None }
pub fn from_ble_product_id(_p: u16) -> Option<&'static ModelInfo> { None }
