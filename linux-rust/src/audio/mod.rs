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
use guard::{Cmd, Guard, GuardConfig, ProfileAction, decide_profile};
use log::{debug, warn};
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

    /// The system resumed from sleep (or anything else that may have broken audio).
    pub fn resume_hint(&self) {
        self.send(Cmd::Resume);
    }

    pub fn health(&self) -> watch::Receiver<AudioHealth> {
        self.health_rx.clone()
    }

    /// Stops the guard task. Idempotent.
    pub fn shutdown(&self) {
        self.send(Cmd::Shutdown);
    }
}

/// One-shot fix used by `MediaController`: if the card for `mac` is on `off`, or
/// on HFP without the microphone in use, switch it to the best A2DP profile.
/// Returns the profile that is active afterwards.
pub async fn ensure_a2dp_profile(mac: &str) -> Result<String, String> {
    ensure_a2dp_profile_with(&LibPulse, mac).await
}

pub(crate) async fn ensure_a2dp_profile_with<P: PulseApi>(
    pulse: &P,
    mac: &str,
) -> Result<String, String> {
    let snap = pulse.snapshot().await.map_err(|e| e.to_string())?;
    let card = snap
        .card_for(mac)
        .ok_or_else(|| format!("no audio card for {mac}"))?;
    let active = card.active_profile.clone().unwrap_or_default();
    match decide_profile(card, snap.is_recording_from_card(card.index)) {
        ProfileAction::Keep | ProfileAction::LeaveHeadset => Ok(active),
        ProfileAction::Set(candidates) => {
            for profile in &candidates {
                match pulse.set_card_profile(card.index, profile).await {
                    Ok(()) => return Ok(profile.clone()),
                    Err(e) => warn!("setting {profile} on {} failed: {e}", card.name),
                }
            }
            Err(format!(
                "no A2DP profile could be activated (tried {candidates:?})"
            ))
        }
    }
}

#[cfg(test)]
mod tests;
