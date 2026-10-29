//! Settings parsing, saving and change notification.

use super::*;

#[test]
fn every_setting_survives_a_save_and_load() {
    let s = Settings {
        appearance: Appearance::Dark,
        overlay_mode: OverlayMode::Always,
        overlay_position: OverlayPosition::BottomRight,
        overlay_opacity: 0.45,
        overlay_glass: 0.1,
        overlay_style: OverlayStyle::Purist,
        overlay_size: OverlaySize::Large,
        start_at_login: false,
        stem_control: true,
        airpods_mic_switch: true,
        return_to_pc: false,
        extra: Map::new(),
    };
    let back = Settings::from_json(&s.to_json());
    assert_eq!(back, s);
}

#[test]
fn every_overlay_position_and_mode_has_a_stable_name() {
    for pos in OVERLAY_POSITIONS {
        let s = Settings {
            overlay_position: pos,
            ..Settings::default()
        };
        assert_eq!(Settings::from_json(&s.to_json()).overlay_position, pos);
    }
    for (mode, name) in [
        (OverlayMode::Auto, "auto"),
        (OverlayMode::Always, "always"),
        (OverlayMode::Compact, "compact"),
        (OverlayMode::Off, "off"),
    ] {
        let json = format!(r#"{{"overlay_mode":"{name}"}}"#);
        assert_eq!(Settings::from_json(&json).overlay_mode, mode);
    }
    for (style, name) in [
        (OverlayStyle::Classic, "classic"),
        (OverlayStyle::Purist, "purist"),
    ] {
        let json = format!(r#"{{"overlay_style":"{name}"}}"#);
        assert_eq!(Settings::from_json(&json).overlay_style, style);
    }
    for (size, name) in [
        (OverlaySize::Small, "small"),
        (OverlaySize::Medium, "medium"),
        (OverlaySize::Large, "large"),
        (OverlaySize::ExtraLarge, "extra_large"),
    ] {
        let json = format!(r#"{{"overlay_size":"{name}"}}"#);
        assert_eq!(Settings::from_json(&json).overlay_size, size);
    }
    let s = Settings::from_json(r#"{"overlay_style":"neon","overlay_size":9}"#);
    assert_eq!(s.overlay_style, OverlayStyle::Purist);
    assert_eq!(s.overlay_size, OverlaySize::Medium);
    for (a, name) in [
        (Appearance::System, "system"),
        (Appearance::Light, "light"),
        (Appearance::Dark, "dark"),
    ] {
        let json = format!(r#"{{"appearance":"{name}"}}"#);
        assert_eq!(Settings::from_json(&json).appearance, a);
    }
}

#[test]
fn sliders_are_kept_in_range() {
    let s = Settings::from_json(r#"{"overlay_opacity":0,"overlay_glass":7}"#);
    assert_eq!(s.overlay_opacity, 0.2, "never fully invisible");
    assert_eq!(s.overlay_glass, 1.0);
    let s = Settings::from_json(r#"{"overlay_opacity":5,"overlay_glass":-2}"#);
    assert_eq!(s.overlay_opacity, 1.0);
    assert_eq!(s.overlay_glass, 0.0);
    // Wrong types fall back to the default for that key only.
    let s = Settings::from_json(
        r#"{"overlay_opacity":"high","stem_control":"yes","start_at_login":false}"#,
    );
    assert_eq!(s.overlay_opacity, Settings::default().overlay_opacity);
    assert!(!s.stem_control);
    assert!(!s.start_at_login);
}

#[test]
fn managed_keys_are_not_duplicated_in_extra() {
    let s = Settings::from_json(r#"{"stem_control":true,"custom":1}"#);
    assert_eq!(s.extra.len(), 1);
    assert!(s.extra.contains_key("custom"));
}

#[test]
fn store_notifies_only_on_real_changes() {
    let store = SettingsStore::ephemeral(Settings::default());
    let mut rx = store.subscribe();
    store.update(|s| s.stem_control = false); // already false
    assert!(!rx.has_changed().unwrap());
    store.update(|s| s.stem_control = true);
    assert!(rx.has_changed().unwrap());
    assert!(rx.borrow_and_update().stem_control);
    assert!(store.get().stem_control);
}

#[tokio::test]
async fn ephemeral_store_never_writes() {
    let store = SettingsStore::ephemeral(Settings::default());
    store.clone().persist().await;
}

#[tokio::test]
async fn persist_writes_the_latest_settings() {
    let dir = std::env::temp_dir().join(format!("kase-settings-{}", std::process::id()));
    let path = dir.join("app_settings.json");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(&path, r#"{"legacy":"keep me","overlay_mode":"auto"}"#).unwrap();

    let loaded = Settings::from_json(&std::fs::read_to_string(&path).unwrap());
    let store = SettingsStore::with(loaded, Some(path.clone()));
    store.update(|s| s.overlay_position = OverlayPosition::TopLeft);
    store.clone().persist().await;
    store.update(|s| s.overlay_mode = OverlayMode::Off);
    store.clone().persist().await;

    let saved = Settings::from_json(&std::fs::read_to_string(&path).unwrap());
    assert_eq!(saved.overlay_position, OverlayPosition::TopLeft);
    assert_eq!(saved.overlay_mode, OverlayMode::Off);
    assert_eq!(saved.extra.get("legacy"), Some(&Value::from("keep me")));
    assert!(!dir.join("app_settings.json.tmp").exists());
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn persist_failure_is_logged_not_fatal() {
    // A path under a regular file cannot be created.
    let file = std::env::temp_dir().join(format!("kase-blocker-{}", std::process::id()));
    std::fs::write(&file, "x").unwrap();
    let store = SettingsStore::with(Settings::default(), Some(file.join("app_settings.json")));
    store.persist().await;
    let _ = std::fs::remove_file(file);
}

#[test]
fn overlay_config_follows_the_settings_defaults() {
    // A fresh install has no overlay keys: the island must use the same values the
    // Settings page shows (it used to draw at 100% while the slider said 60%).
    let cfg = Settings::from_json("{}").overlay_config();
    let d = Settings::default();
    assert_eq!(cfg.mode, OverlayMode::Compact);
    assert_eq!(cfg.opacity, d.overlay_opacity);
    assert_eq!(cfg.glass, d.overlay_glass);
    assert_eq!(cfg.position, d.overlay_position);
}

#[test]
fn overlay_config_reads_and_clamps_values() {
    let cfg = Settings::from_json(
        r#"{"overlay_mode":"always","overlay_position":"bottom-center",
            "overlay_opacity":0.05,"overlay_glass":3.5,
            "overlay_style":"classic","overlay_size":"extra_large"}"#,
    )
    .overlay_config();
    assert_eq!(cfg.mode, OverlayMode::Always);
    assert_eq!(cfg.position, OverlayPosition::BottomCenter);
    assert_eq!(cfg.opacity, 0.2);
    assert_eq!(cfg.glass, 1.0);
    assert_eq!(cfg.style, OverlayStyle::Classic);
    assert_eq!(cfg.size, OverlaySize::ExtraLarge);
}
