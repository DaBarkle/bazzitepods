//! The app's JSON files on disk: one atomic, durable write for all of them, and one place
//! that reads and updates devices.json (which holds the AirPods' LE keys).

use crate::devices::enums::DeviceData;
use crate::utils::get_devices_path;
use log::error;
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use tokio::sync::Mutex;

/// Write `contents` to `path` atomically and durably: a temp file in the same directory is
/// written and flushed to disk before it replaces `path`. Without the flush, a power cut
/// right after the rename can leave an empty file, and the lenient readers then silently
/// fall back to defaults (forgetting settings or the AirPods' keys).
pub fn write_atomic(path: &Path, contents: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    let result = (|| {
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(contents.as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// Saved devices, or none when devices.json is missing or unreadable.
pub fn load_devices() -> HashMap<String, DeviceData> {
    std::fs::read_to_string(get_devices_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Serialises read-modify-write cycles of devices.json within this process.
static DEVICES_FILE_LOCK: Mutex<()> = Mutex::const_new(());

/// Re-read devices.json, apply `f`, and write it back atomically.
pub async fn update_devices<F>(f: F) -> std::io::Result<HashMap<String, DeviceData>>
where
    F: FnOnce(&mut HashMap<String, DeviceData>),
{
    update_devices_at(&get_devices_path(), f).await
}

/// [`update_devices`] for the file at `path`. Re-reading avoids clobbering entries written
/// by other device tasks. A file that exists but cannot be parsed is moved aside to
/// `<path>.bak` rather than lost.
pub async fn update_devices_at<F>(
    path: &Path,
    f: F,
) -> std::io::Result<HashMap<String, DeviceData>>
where
    F: FnOnce(&mut HashMap<String, DeviceData>),
{
    let _guard = DEVICES_FILE_LOCK.lock().await;
    let mut devices: HashMap<String, DeviceData> = match tokio::fs::read_to_string(path).await {
        Ok(s) if s.trim().is_empty() => HashMap::new(),
        Ok(s) => match serde_json::from_str(&s) {
            Ok(d) => d,
            Err(e) => {
                error!("devices.json is corrupt ({}); moving it aside", e);
                let mut bak = path.as_os_str().to_owned();
                bak.push(".bak");
                let _ = tokio::fs::rename(path, PathBuf::from(bak)).await;
                HashMap::new()
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => HashMap::new(),
        Err(e) => return Err(e),
    };
    f(&mut devices);
    let json = serde_json::to_string_pretty(&devices)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || write_atomic(&path, &json))
        .await
        .map_err(std::io::Error::other)??;
    Ok(devices)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_atomic_replaces_and_leaves_no_temp_file() {
        let dir = std::env::temp_dir().join(format!("kase-store-{}", std::process::id()));
        let path = dir.join("sub").join("x.json");
        write_atomic(&path, "{\"a\":1}").unwrap();
        write_atomic(&path, "{\"a\":2}").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"a\":2}");
        assert!(!dir.join("sub").join("x.json.tmp").exists());
        let _ = std::fs::remove_dir_all(dir);
    }
}
