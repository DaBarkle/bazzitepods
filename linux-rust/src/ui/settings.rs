//! Typed app settings, persisted to `app_settings.json`.
//!
//! Read once at startup ([`SettingsStore::load`]) and kept in memory; render paths never touch
//! the disk. Writes are atomic (temp file + rename) and run off the UI thread.
//!
//! Unknown keys (including legacy ones such as `theme` and `tray_text_mode`) are preserved, and a
//! malformed value for one key only resets that key to its default.

use crate::store::write_atomic;
use crate::utils::get_app_settings_path;
use log::warn;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::watch;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Appearance {
    #[default]
    System,
    Light,
    Dark,
}

pub use crate::overlay_protocol::{OverlayMode, OverlayPosition, OverlaySize, OverlayStyle};

/// Overlay corners/edges in picker order (top row, then bottom row).
pub const OVERLAY_POSITIONS: [OverlayPosition; 6] = [
    OverlayPosition::TopLeft,
    OverlayPosition::TopCenter,
    OverlayPosition::TopRight,
    OverlayPosition::BottomLeft,
    OverlayPosition::BottomCenter,
    OverlayPosition::BottomRight,
];

#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub appearance: Appearance,
    pub overlay_mode: OverlayMode,
    pub overlay_position: OverlayPosition,
    /// Overlay opacity while a fullscreen window is in front (solid otherwise).
    pub overlay_opacity: f32,
    /// Overlay material, 0 = smoked solid .. 1 = clear liquid glass.
    pub overlay_glass: f32,
    /// Classic glass island, or the purist flat black one.
    pub overlay_style: OverlayStyle,
    pub overlay_size: OverlaySize,
    /// Login autostart (applied through `crate::autostart`).
    pub start_at_login: bool,
    /// Stem double/triple press -> next/previous track. Read by the device task.
    pub stem_control: bool,
    /// Left stem hold swaps the desk mic and the AirPods mic; right hold changes noise
    /// control. Read by the device task.
    pub airpods_mic_switch: bool,
    /// Bring the AirPods back to this PC when the iPhone stops playing. Read by the device
    /// task (`return_to_pc`).
    pub return_to_pc: bool,
    /// Keys we don't manage, preserved verbatim.
    pub extra: Map<String, Value>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            appearance: Appearance::default(),
            overlay_mode: OverlayMode::Compact,
            overlay_position: OverlayPosition::default(),
            overlay_opacity: 0.6,
            overlay_glass: 0.7,
            overlay_style: OverlayStyle::default(),
            overlay_size: OverlaySize::default(),
            start_at_login: true,
            stem_control: false,
            airpods_mic_switch: false,
            return_to_pc: true,
            extra: Map::new(),
        }
    }
}

const KEYS: [&str; 11] = [
    "overlay_glass",
    "overlay_style",
    "overlay_size",
    "appearance",
    "overlay_mode",
    "overlay_position",
    "overlay_opacity",
    "start_at_login",
    "stem_control",
    "airpods_mic_switch",
    "return_to_pc",
];

fn field<T: serde::de::DeserializeOwned>(map: &Map<String, Value>, key: &str, default: T) -> T {
    match map.get(key) {
        Some(v) => serde_json::from_value(v.clone()).unwrap_or_else(|_| {
            warn!("Ignoring invalid value for setting {key}: {v}");
            default
        }),
        None => default,
    }
}

impl Settings {
    /// What the overlay process draws with. The only source of overlay settings, so the
    /// island and the Settings page can't disagree.
    pub fn overlay_config(&self) -> crate::overlay_protocol::OverlayConfig {
        crate::overlay_protocol::OverlayConfig {
            mode: self.overlay_mode,
            position: self.overlay_position,
            opacity: self.overlay_opacity,
            glass: self.overlay_glass,
            style: self.overlay_style,
            size: self.overlay_size,
            ..Default::default()
        }
    }

    /// Lenient parse: invalid JSON gives defaults; invalid fields fall back individually.
    pub fn from_json(json: &str) -> Self {
        let Ok(Value::Object(map)) = serde_json::from_str::<Value>(json) else {
            return Self::default();
        };
        let d = Self::default();
        let mut extra = map.clone();
        for k in KEYS {
            extra.remove(k);
        }
        Self {
            appearance: field(&map, "appearance", d.appearance),
            overlay_mode: field(&map, "overlay_mode", d.overlay_mode),
            overlay_position: field(&map, "overlay_position", d.overlay_position),
            overlay_opacity: field(&map, "overlay_opacity", d.overlay_opacity).clamp(0.2, 1.0),
            overlay_glass: field(&map, "overlay_glass", d.overlay_glass).clamp(0.0, 1.0),
            overlay_style: field(&map, "overlay_style", d.overlay_style),
            overlay_size: field(&map, "overlay_size", d.overlay_size),
            start_at_login: field(&map, "start_at_login", d.start_at_login),
            stem_control: field(&map, "stem_control", d.stem_control),
            airpods_mic_switch: field(&map, "airpods_mic_switch", d.airpods_mic_switch),
            return_to_pc: field(&map, "return_to_pc", d.return_to_pc),
            extra,
        }
    }

    pub fn to_json(&self) -> String {
        let mut map = self.extra.clone();
        let mut put = |k: &str, v: Value| {
            map.insert(k.to_string(), v);
        };
        put(
            "appearance",
            serde_json::to_value(self.appearance).unwrap_or(Value::Null),
        );
        put(
            "overlay_mode",
            serde_json::to_value(self.overlay_mode).unwrap_or(Value::Null),
        );
        put(
            "overlay_position",
            serde_json::to_value(self.overlay_position).unwrap_or(Value::Null),
        );
        put("overlay_opacity", Value::from(self.overlay_opacity));
        put("overlay_glass", Value::from(self.overlay_glass));
        put(
            "overlay_style",
            serde_json::to_value(self.overlay_style).unwrap_or(Value::Null),
        );
        put(
            "overlay_size",
            serde_json::to_value(self.overlay_size).unwrap_or(Value::Null),
        );
        put("start_at_login", Value::from(self.start_at_login));
        put("stem_control", Value::from(self.stem_control));
        put("airpods_mic_switch", Value::from(self.airpods_mic_switch));
        put("return_to_pc", Value::from(self.return_to_pc));
        serde_json::to_string_pretty(&Value::Object(map)).unwrap_or_else(|_| "{}".to_string())
    }
}

/// In-memory settings shared by the UI (and anything else that wants to follow changes, e.g.
/// the overlay client via [`SettingsStore::subscribe`]).
#[derive(Clone)]
pub struct SettingsStore {
    tx: Arc<watch::Sender<Settings>>,
    path: Option<PathBuf>,
    write_lock: Arc<std::sync::Mutex<()>>,
}

impl SettingsStore {
    /// Read app_settings.json once. Missing or broken files give defaults.
    pub fn load() -> Self {
        let path = get_app_settings_path();
        let settings = std::fs::read_to_string(&path)
            .map(|s| Settings::from_json(&s))
            .unwrap_or_default();
        Self::with(settings, Some(path))
    }

    /// A store that never touches the disk (demo mode, tests).
    pub fn ephemeral(settings: Settings) -> Self {
        Self::with(settings, None)
    }

    fn with(settings: Settings, path: Option<PathBuf>) -> Self {
        let (tx, _rx) = watch::channel(settings);
        Self {
            tx: Arc::new(tx),
            path,
            write_lock: Arc::new(std::sync::Mutex::new(())),
        }
    }

    pub fn get(&self) -> Settings {
        self.tx.borrow().clone()
    }

    pub fn subscribe(&self) -> watch::Receiver<Settings> {
        self.tx.subscribe()
    }

    /// Change settings in memory and notify subscribers. Call [`SettingsStore::persist`] after.
    pub fn update(&self, f: impl FnOnce(&mut Settings)) {
        self.tx.send_if_modified(|s| {
            let before = s.clone();
            f(s);
            *s != before
        });
    }

    /// Write the current settings to disk on a blocking thread. Never panics.
    pub async fn persist(self) {
        let Some(path) = self.path.clone() else {
            return;
        };
        let res = tokio::task::spawn_blocking(move || {
            // Serialize writers so an older snapshot never lands after a newer one.
            let _guard = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
            let json = self.get().to_json();
            write_atomic(&path, &json)
        })
        .await;
        match res {
            Ok(Ok(())) => {}
            Ok(Err(e)) => warn!("Could not save settings: {e}"),
            Err(e) => warn!("Settings writer failed: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_when_missing_or_broken() {
        assert_eq!(Settings::from_json(""), Settings::default());
        assert_eq!(Settings::from_json("not json"), Settings::default());
        assert_eq!(Settings::from_json("[1,2]"), Settings::default());
        let d = Settings::default();
        assert!(d.start_at_login);
        assert!(d.return_to_pc);
        assert_eq!(d.overlay_mode, OverlayMode::Compact);
        assert_eq!(d.appearance, Appearance::System);
    }

    #[test]
    fn legacy_keys_preserved_and_bad_fields_fall_back() {
        let s = Settings::from_json(
            r#"{"theme":"Dark","tray_text_mode":true,"stem_control":true,
                "overlay_mode":"bogus","overlay_position":"bottom-left","start_at_login":false}"#,
        );
        assert!(s.stem_control);
        assert!(!s.airpods_mic_switch);
        assert!(!s.start_at_login);
        assert_eq!(s.overlay_mode, OverlayMode::Compact);
        assert_eq!(s.overlay_position, OverlayPosition::BottomLeft);
        assert_eq!(s.extra.get("theme"), Some(&Value::from("Dark")));

        let round = Settings::from_json(&s.to_json());
        assert_eq!(round, s);
        let v: Value = serde_json::from_str(&s.to_json()).unwrap();
        assert_eq!(v["overlay_position"], "bottom-left");
        assert_eq!(v["tray_text_mode"], true);
    }
}

#[cfg(test)]
#[path = "settings_tests.rs"]
mod store_tests;
