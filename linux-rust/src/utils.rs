use aes::Aes128;
use aes::cipher::Array;
use aes::cipher::{BlockCipherEncrypt, KeyInit};
use std::path::PathBuf;

/// Folder name under the XDG data and config directories. LibrePods and Kase 0.6 and earlier
/// used `librepods`; [`migrate_legacy_dirs`] moves those over.
const DIR_NAME: &str = "kase";
const LEGACY_DIR_NAME: &str = "librepods";

fn home() -> String {
    std::env::var("HOME").unwrap_or_default()
}

fn data_home() -> PathBuf {
    std::env::var("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(home()).join(".local/share"))
}

pub(crate) fn state_home() -> PathBuf {
    std::env::var("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(home()).join(".local/state"))
}

fn config_home() -> PathBuf {
    std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(home()).join(".config"))
}

pub fn get_devices_path() -> PathBuf {
    data_home().join(DIR_NAME).join("devices.json")
}

pub fn get_app_settings_path() -> PathBuf {
    let new_path = config_home().join(DIR_NAME).join("app_settings.json");
    // Very old LibrePods kept settings loose in the data directory.
    let old_path = data_home().join("app_settings.json");
    if old_path.exists() && !new_path.exists() {
        if let Some(parent) = new_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if std::fs::copy(&old_path, &new_path).is_ok() {
            let _ = std::fs::remove_file(&old_path);
        }
    }
    new_path
}

/// Moves the data and config folders from their old `librepods` names to `kase`, once.
/// Call at startup, before anything reads or writes them.
pub fn migrate_legacy_dirs() {
    for base in [data_home(), config_home()] {
        match migrate_dir(&base.join(LEGACY_DIR_NAME), &base.join(DIR_NAME)) {
            Ok(true) => log::info!(
                "Moved {} to {}",
                LEGACY_DIR_NAME,
                base.join(DIR_NAME).display()
            ),
            Ok(false) => {}
            Err(e) => log::warn!("Could not move {}/{LEGACY_DIR_NAME}: {e}", base.display()),
        }
    }
}

/// Renames `old` to `new` when only `old` exists. Returns whether anything moved; never
/// touches an existing `new`, so a second run, or a newer folder, is left alone.
fn migrate_dir(old: &std::path::Path, new: &std::path::Path) -> std::io::Result<bool> {
    if !old.is_dir() || new.exists() {
        return Ok(false);
    }
    std::fs::rename(old, new)?;
    Ok(true)
}

fn e(key: &[u8; 16], data: &[u8; 16]) -> [u8; 16] {
    let mut swapped_key = *key;
    swapped_key.reverse();
    let mut swapped_data = *data;
    swapped_data.reverse();
    let cipher = Aes128::new(&Array::from(swapped_key));
    let mut block = Array::from(swapped_data);
    cipher.encrypt_block(&mut block);
    let mut result: [u8; 16] = block.into();
    result.reverse();
    result
}

pub fn ah(k: &[u8; 16], r: &[u8; 3]) -> [u8; 3] {
    let mut r_padded = [0u8; 16];
    r_padded[..3].copy_from_slice(r);
    let encrypted = e(k, &r_padded);
    let mut hash = [0u8; 3];
    hash.copy_from_slice(&encrypted[..3]);
    hash
}

/// Read a boolean from app_settings.json, if present.
pub fn app_setting_bool(key: &str) -> Option<bool> {
    std::fs::read_to_string(get_app_settings_path())
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v.get(key).and_then(|b| b.as_bool()))
}

#[cfg(test)]
#[path = "utils_tests.rs"]
mod hash_tests;
