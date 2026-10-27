//! Hears why BlueZ dropped each link, so the supervisor can tell AirPods that walked out of
//! range (reconnect when they come back) from AirPods disconnected on this PC (leave them be).
//!
//! Recent BlueZ (5.87 on Bazzite) emits `org.bluez.Device1.Disconnected(reason, message)` on every disconnect.
//! Without it (older BlueZ) we only know the link went away, and treat that like a walk-out.

use crate::Kick;
use bluer::Address;
use dbus::message::MatchRule;
use log::{debug, info};
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
    /// Let go of this PC to make room for an iPhone: try for a few minutes, until it lets go.
    Briefly,
    /// Walked off, went into the case, slept: come back when they do, however long that is,
    /// as on a Mac.
    UntilBack,
}

/// Decide from why the link dropped (if BlueZ said) and what the AirPods last reported.
pub fn plan(reason: Option<DropReason>, in_case: bool, for_peer: bool) -> ReconnectPlan {
    match reason {
        Some(DropReason::Local) => ReconnectPlan::Never,
        // A supervision timeout is distance, whatever else happened just before.
        Some(DropReason::Timeout | DropReason::Suspend) => ReconnectPlan::UntilBack,
        _ if in_case => ReconnectPlan::UntilBack,
        _ if for_peer => ReconnectPlan::Briefly,
        _ => ReconnectPlan::UntilBack,
    }
}

/// `/org/bluez/hci0/dev_AA_BB_CC_DD_EE_FF` -> `AA:BB:CC:DD:EE:FF`.
pub fn address_from_path(path: &str) -> Option<Address> {
    let dev = path.rsplit('/').next()?.strip_prefix("dev_")?;
    dev.replace('_', ":").parse().ok()
}

pub fn spawn(kick_tx: UnboundedSender<Kick>) {
    tokio::spawn(async move {
        if let Err(e) = run(kick_tx).await {
            // Not fatal: every drop is then treated as a walk-out.
            info!("Disconnect reasons unavailable: {e}");
        }
    });
}

async fn run(kick_tx: UnboundedSender<Kick>) -> Result<(), dbus::Error> {
    let (resource, conn) = dbus_tokio::connection::new_system_sync()?;
    // The resource future only ends when the connection is lost.
    let lost = tokio::spawn(async move { resource.await });

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
    fn walking_out_of_range_reconnects_until_back() {
        // The bug: worn out of the house, they were only reconnected from adverts for 4 h.
        assert_eq!(
            plan(Some(DropReason::Timeout), false, false),
            ReconnectPlan::UntilBack
        );
        // Older BlueZ without reasons: same.
        assert_eq!(plan(None, false, false), ReconnectPlan::UntilBack);
    }

    #[test]
    fn timeout_wins_over_a_peer_joining() {
        assert_eq!(
            plan(Some(DropReason::Timeout), false, true),
            ReconnectPlan::UntilBack
        );
    }

    #[test]
    fn disconnecting_here_stays_disconnected() {
        assert_eq!(
            plan(Some(DropReason::Local), false, false),
            ReconnectPlan::Never
        );
        assert_eq!(
            plan(Some(DropReason::Local), true, false),
            ReconnectPlan::Never
        );
        assert_eq!(
            plan(Some(DropReason::Local), false, true),
            ReconnectPlan::Never
        );
    }

    #[test]
    fn case_and_peer_drops_keep_their_behaviour() {
        assert_eq!(
            plan(Some(DropReason::Remote), true, false),
            ReconnectPlan::UntilBack
        );
        assert_eq!(plan(None, true, false), ReconnectPlan::UntilBack);
        assert_eq!(
            plan(Some(DropReason::Remote), false, true),
            ReconnectPlan::Briefly
        );
        assert_eq!(plan(None, false, true), ReconnectPlan::Briefly);
    }

    #[test]
    fn sleep_reconnects_until_back() {
        assert_eq!(
            plan(Some(DropReason::Suspend), false, true),
            ReconnectPlan::UntilBack
        );
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
