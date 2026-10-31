//! Hears why BlueZ dropped each link, so the supervisor can tell AirPods that walked out of
//! range (reconnect when they come back) from AirPods disconnected on this PC (leave them be).
//!
//! Recent BlueZ (5.87 on Bazzite) emits `org.bluez.Device1.Disconnected(reason, message)` on every disconnect.
//! Without it (older BlueZ) we only know the link went away, and treat that like a walk-out.

use crate::Kick;
use bluer::Address;
use dbus::message::MatchRule;
use log::{debug, info, warn};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use tokio::sync::mpsc::UnboundedSender;

/// Why a link dropped, as BlueZ reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropReason {
    /// Supervision timeout: the other end stopped answering (out of range, battery died).
    Timeout,
    /// This PC closed the link: someone pressed Disconnect here (KDE, bluetoothctl, ...).
    Local,
    /// The AirPods closed the link: put in the case, or handed over to another device.
    Remote,
    /// This PC went to sleep.
    Suspend,
    Authentication,
    Unknown,
}

impl DropReason {
    pub fn parse(reason: &str) -> Self {
        match reason.rsplit('.').next().unwrap_or_default() {
            "Timeout" => Self::Timeout,
            "Local" => Self::Local,
            "Remote" => Self::Remote,
            "Suspend" => Self::Suspend,
            "Authentication" => Self::Authentication,
            _ => Self::Unknown,
        }
    }
}

/// What to do about AirPods that dropped this PC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconnectPlan {
    /// Disconnected here on purpose: stay disconnected.
    Never,
    /// Walked off, went into the case, slept, or left for an iPhone: come back when they
    /// do, however long that is. The PC is home.
    UntilBack,
}

/// Decide from why the link dropped, if BlueZ said. Only a disconnect pressed on this PC
/// keeps them away. Dropping this PC for an iPhone used to be retried for 3 minutes only,
/// so AirPods taken to work with the iPhone never came back in the evening (2026-10-28).
pub fn plan(reason: Option<DropReason>) -> ReconnectPlan {
    match reason {
        Some(DropReason::Local) => ReconnectPlan::Never,
        _ => ReconnectPlan::UntilBack,
    }
}

/// AirPods disconnected on this PC, kept across restarts and reboots so Kase leaves them be.
pub fn kept_away_path() -> PathBuf {
    crate::utils::get_devices_path().with_file_name("kept_disconnected.json")
}

/// The remembered Disconnects. A missing or unreadable file means none: reconnecting is
/// the safe default.
pub fn load_kept_away(path: &Path) -> HashSet<Address> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok())
        .unwrap_or_default()
        .iter()
        .filter_map(|a| a.parse().ok())
        .collect()
}

pub fn save_kept_away(path: &Path, kept: &HashSet<Address>) {
    let mut list: Vec<String> = kept.iter().map(|a| a.to_string()).collect();
    list.sort();
    if let Err(e) = crate::store::write_atomic(path, &serde_json::json!(list).to_string()) {
        warn!("Could not save {}: {e}", path.display());
    }
}

/// `/org/bluez/hci0/dev_AA_BB_CC_DD_EE_FF` -> `AA:BB:CC:DD:EE:FF`.
pub fn address_from_path(path: &str) -> Option<Address> {
    let dev = path.rsplit('/').next()?.strip_prefix("dev_")?;
    dev.replace('_', ":").parse().ok()
}

pub fn spawn(kick_tx: UnboundedSender<Kick>) {
    // Without this watcher every drop counts as a walk-out, so a deliberate Disconnect
    // would be undone. Not fatal while it's down.
    tokio::spawn(keep_watching("Disconnect reasons", kick_tx, run));
}

const RETRY_MIN: std::time::Duration = if cfg!(test) {
    std::time::Duration::from_millis(5)
} else {
    std::time::Duration::from_secs(1)
};
const RETRY_MAX: std::time::Duration = std::time::Duration::from_secs(60);

/// Runs a D-Bus signal watcher for the life of the app: `run` returns when the bus
/// connection is lost (bluetoothd or D-Bus restarted), and is started again with a backoff.
pub(crate) async fn keep_watching<F, Fut>(what: &str, kick_tx: UnboundedSender<Kick>, run: F)
where
    F: Fn(UnboundedSender<Kick>) -> Fut,
    Fut: std::future::Future<Output = Result<(), dbus::Error>>,
{
    let mut delay = RETRY_MIN;
    while !kick_tx.is_closed() {
        let started = std::time::Instant::now();
        match run(kick_tx.clone()).await {
            Ok(()) => info!("{what}: bus connection lost; reconnecting"),
            Err(e) => info!("{what} unavailable: {e}"),
        }
        if started.elapsed() > RETRY_MAX {
            delay = RETRY_MIN;
        }
        tokio::time::sleep(delay).await;
        delay = (delay * 2).min(RETRY_MAX);
    }
}

async fn run(kick_tx: UnboundedSender<Kick>) -> Result<(), dbus::Error> {
    let (resource, conn) = dbus_tokio::connection::new_system_sync()?;
    // The resource future only ends when the connection is lost.
    let lost = tokio::spawn(resource);

    let rule = MatchRule::new_signal("org.bluez.Device1", "Disconnected");
    let _match = conn
        .add_match(rule)
        .await?
        .cb(move |msg, (reason, message): (String, String)| {
            let Some(addr) = msg.path().and_then(|p| address_from_path(&p)) else {
                return true;
            };
            debug!("{addr} disconnected: {reason} ({message})");
            let _ = kick_tx.send(Kick::Disconnected(addr, DropReason::parse(&reason)));
            true
        });
    let err = lost.await;
    debug!("BlueZ signal connection ended: {err:?}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn watcher_is_started_again_after_the_bus_drops() {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let rx = std::sync::Mutex::new(Some(rx));
        let runs = std::sync::atomic::AtomicU32::new(0);
        let run = |_tx: UnboundedSender<Kick>| {
            let n = runs.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
            if n == 3 {
                // The app is shutting down: the watcher stops for good.
                rx.lock().unwrap().take();
            }
            async { Ok(()) }
        };
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            keep_watching("test", tx, run),
        )
        .await
        .expect("watcher loop did not stop");
        assert_eq!(runs.load(std::sync::atomic::Ordering::SeqCst), 3);
    }

    #[test]
    fn parses_bluez_reasons() {
        assert_eq!(
            DropReason::parse("org.bluez.Reason.Timeout"),
            DropReason::Timeout
        );
        assert_eq!(
            DropReason::parse("org.bluez.Reason.Local"),
            DropReason::Local
        );
        assert_eq!(
            DropReason::parse("org.bluez.Reason.Remote"),
            DropReason::Remote
        );
        assert_eq!(
            DropReason::parse("org.bluez.Reason.Suspend"),
            DropReason::Suspend
        );
        assert_eq!(
            DropReason::parse("org.bluez.Reason.Authentication"),
            DropReason::Authentication
        );
        assert_eq!(
            DropReason::parse("org.bluez.Reason.Unknown"),
            DropReason::Unknown
        );
        assert_eq!(DropReason::parse(""), DropReason::Unknown);
    }

    #[test]
    fn every_drop_but_a_local_disconnect_reconnects_until_back() {
        for reason in [
            None,
            Some(DropReason::Timeout),
            Some(DropReason::Remote),
            Some(DropReason::Suspend),
            Some(DropReason::Authentication),
            Some(DropReason::Unknown),
        ] {
            assert_eq!(plan(reason), ReconnectPlan::UntilBack, "{reason:?}");
        }
    }

    #[test]
    fn disconnecting_here_stays_disconnected() {
        assert_eq!(plan(Some(DropReason::Local)), ReconnectPlan::Never);
    }

    #[test]
    fn kept_disconnects_survive_a_restart_and_a_bad_file_means_none() {
        let dir = std::env::temp_dir().join(format!("kase-kept-{}", std::process::id()));
        let path = dir.join("kept_disconnected.json");
        assert!(load_kept_away(&path).is_empty(), "missing file");
        let a: Address = "CC:22:FE:24:0C:70".parse().unwrap();
        save_kept_away(&path, &HashSet::from([a]));
        assert_eq!(load_kept_away(&path), HashSet::from([a]));
        std::fs::write(&path, "{not json").unwrap();
        assert!(load_kept_away(&path).is_empty(), "corrupt file");
        std::fs::write(&path, r#"["nonsense", "CC:22:FE:24:0C:70"]"#).unwrap();
        assert_eq!(load_kept_away(&path), HashSet::from([a]));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn address_from_object_path() {
        assert_eq!(
            address_from_path("/org/bluez/hci0/dev_CC_22_FE_24_0C_70"),
            Some("CC:22:FE:24:0C:70".parse().unwrap())
        );
        assert_eq!(address_from_path("/org/bluez/hci0"), None);
        assert_eq!(address_from_path("/org/bluez/hci0/dev_nonsense"), None);
    }
}
