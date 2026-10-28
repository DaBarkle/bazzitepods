//! Keeps A2DP audio alive for one AirPods device across range loss, reconnects,
//! sleep/resume and silent link stalls.
//!
//! Usage (from the device task):
//! ```ignore
//! let guard = AudioGuard::spawn(mac, session);
//! guard.link_up();                 // AACP/ACL connected
//! guard.wearing_changed(true);     // ear detection
//! guard.resume_hint();             // system resumed
//! guard.link_down();               // device disconnected
//! guard.shutdown();                // device task ends
//! ```

// The device supervisor (devices/airpods.rs) wires AudioGuard up in a separate
// change; until then most of this module is only reached from tests.

pub mod bluez;
pub mod guard;
pub mod pulse;

use bluez::{BluerBackend, BluezApi};
use guard::{Cmd, Guard, GuardConfig};
use log::debug;
use pulse::{LibPulse, PulseApi};
use std::sync::Arc;
use tokio::sync::{mpsc, watch};

/// What the UI should show about the audio path.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub enum AudioHealth {
    Disconnected,
    Negotiating,
    Ready {
        profile: String,
        codec: Option<String>,
    },
    Recovering {
        step: u8,
    },
    Failed {
        reason: String,
    },
}

pub struct AudioGuard;

impl AudioGuard {
    /// Spawns the guard task for `mac` on the current tokio runtime. It also runs
    /// a one-time cleanup: if the card is currently on `off`, it is set to A2DP.
    pub fn spawn(mac: bluer::Address, session: bluer::Session) -> AudioGuardHandle {
        Self::spawn_with(
            mac,
            LibPulse,
            BluerBackend::new(session),
            GuardConfig::default(),
        )
    }

    pub(crate) fn spawn_with<P: PulseApi, B: BluezApi>(
        mac: bluer::Address,
        pulse: P,
        bluez: B,
        cfg: GuardConfig,
    ) -> AudioGuardHandle {
        let (tx, rx) = mpsc::unbounded_channel();
        let (health_tx, health_rx) = watch::channel(AudioHealth::Disconnected);
        let guard = Guard::new(mac, Arc::new(pulse), Arc::new(bluez), cfg, health_tx, rx);
        tokio::spawn(guard.run());
        AudioGuardHandle { tx, health_rx }
    }
}

/// Cheap to clone; all methods are non-blocking and never fail (a stopped guard
/// just ignores them).
#[derive(Clone)]
pub struct AudioGuardHandle {
    tx: mpsc::UnboundedSender<Cmd>,
    health_rx: watch::Receiver<AudioHealth>,
}

impl AudioGuardHandle {
    fn send(&self, cmd: Cmd) {
        if self.tx.send(cmd).is_err() {
            debug!("audio guard already stopped, dropping {:?}", cmd);
        }
    }

    /// ACL/AACP link is up. The guard waits ~2 s before acting so it does not
    /// race WirePlumber's own restore.
    pub fn link_up(&self) {
        self.send(Cmd::LinkUp);
    }

    pub fn link_down(&self) {
        self.send(Cmd::LinkDown);
    }

    /// Never changes the profile on removal; insertion triggers a re-check.
    pub fn wearing_changed(&self, any_in_ear: bool) {
        self.send(Cmd::Wearing(any_in_ear));
    }

    /// Anything that may have broken audio: resume from sleep, or the audio route coming
    /// back from another device. Re-runs the full setup after the usual debounce.
    // Unused for now: after sleep the device task restarts, which builds a new guard.
    #[allow(dead_code)]
    pub fn resume_hint(&self) {
        self.send(Cmd::Resume);
    }

    /// The audio route just came back from another Apple device. Its A2DP transport was
    /// often broken while away, so re-check at once and repair a stall within a second.
    pub fn route_back(&self) {
        self.send(Cmd::RouteBack);
    }

    /// Like [`Self::route_back`], and rebuild the output right away: use it when the device
    /// that had the audio was streaming, which breaks this PC's transport.
    pub fn repair(&self) {
        self.send(Cmd::Repair);
    }

    /// Another device is playing on the AirPods: its output here goes dead, so stop
    /// treating that as a stall until [`Self::route_back`] or [`Self::repair`].
    pub fn route_away(&self) {
        self.send(Cmd::Away);
    }

    /// Rebuild the output once, right away (after the AirPods mic stream stops).
    pub fn rebuild(&self) {
        self.send(Cmd::Rebuild);
    }

    /// Mute the speakers now: the AirPods output is about to drop (the mic stream stops).
    pub fn hush(&self) {
        self.send(Cmd::Hush);
    }

    pub fn health(&self) -> watch::Receiver<AudioHealth> {
        self.health_rx.clone()
    }

    /// Stops the guard task. Idempotent.
    pub fn shutdown(&self) {
        self.send(Cmd::Shutdown);
    }

    /// Resolves once the guard task has ended (it drops its health sender when it stops).
    pub async fn stopped(&self) {
        let mut rx = self.health_rx.clone();
        while rx.changed().await.is_ok() {}
    }
}

#[cfg(test)]
mod tests;
