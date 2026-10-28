//! Per-device audio state machine (see `audio/mod.rs` for the public handle).
//!
//! ```text
//! DISCONNECTED --link_up--> (debounce) --> LINKED: wait for bluez card by MAC
//!     card missing + no MediaTransport1 -> ConnectProfile(A2DP), <= N retries
//!   -> PROFILE_CHECK: "off"/missing -> best A2DP; headset w/o recording -> best A2DP;
//!                     headset while recording -> leave; any a2dp-* -> keep
//!   -> SINK_CHECK: restore default sink + move streams if we were default before
//!   -> READY (monitor): stall = sink RUNNING + uncorked inputs, no active transport
//!        for > stall_threshold -> ladder: toggle off->a2dp, alternate codec,
//!        DisconnectProfile+ConnectProfile -> FAILED. Rate limited, bounded.
//! ```
//! Invariant: the card is never left on `off` by this module.

use super::AudioHealth;
use super::bluez::{A2DP_SINK_UUID, BluezApi};
use super::pulse::{CardInfo, PulseApi, PulseSnapshot, SinkInfo, SinkState};
use bluer::Address;
use log::{debug, info, warn};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, watch};
use tokio::time::{Instant, sleep, sleep_until};

/// Tunables. `Default` holds the production values.
#[derive(Debug, Clone)]
pub struct GuardConfig {
    /// Wait after link up before acting, so WirePlumber can restore the device first.
    pub link_debounce: Duration,
    /// How long to wait for the bluez card to appear.
    pub card_wait: Duration,
    /// Poll interval while waiting for pulse state changes.
    pub poll: Duration,
    /// `ConnectProfile(A2DP)` attempts when the card never appears.
    pub connect_profile_retries: u8,
    /// Health check interval while ready.
    pub monitor_interval: Duration,
    /// Sink running with no active transport for this long = stalled.
    pub stall_threshold: Duration,
    /// Gap between `off` and `a2dp` in the profile toggle.
    pub toggle_gap: Duration,
    /// Gap between DisconnectProfile and ConnectProfile.
    pub reconnect_gap: Duration,
    /// How long to wait for a recovery step / profile change to take effect.
    pub verify_timeout: Duration,
    /// Minimum time between two recovery ladders.
    pub ladder_cooldown: Duration,
    /// Maximum recovery ladders per link session.
    pub max_ladders_per_link: u8,
    /// Delay before retrying a failed setup.
    pub retry_after_failure: Duration,
    /// Maximum setup attempts per link session (wearing/resume resets it).
    pub max_ensure_attempts: u8,
    /// After the link comes up, the output was just built: a take-back this soon doesn't
    /// rebuild it again (a stall is still repaired).
    pub fresh_link: Duration,
    /// Where the names of outputs muted by [`Guard::hush`] are kept until they are unmuted,
    /// so a crash or kill mid-rebuild can be undone at the next start. `None` in tests.
    pub hush_file: Option<std::path::PathBuf>,
}

impl Default for GuardConfig {
    fn default() -> Self {
        GuardConfig {
            link_debounce: Duration::from_secs(2),
            card_wait: Duration::from_secs(5),
            poll: Duration::from_millis(400),
            connect_profile_retries: 2,
            monitor_interval: Duration::from_secs(2),
            stall_threshold: Duration::from_secs(4),
            toggle_gap: Duration::from_millis(400),
            reconnect_gap: Duration::from_secs(1),
            verify_timeout: Duration::from_secs(3),
            ladder_cooldown: Duration::from_secs(30),
            max_ladders_per_link: 3,
            retry_after_failure: Duration::from_secs(30),
            max_ensure_attempts: 3,
            fresh_link: Duration::from_secs(10),
            hush_file: Some(hush_file()),
        }
    }
}

/// Outputs a guard muted for a rebuild and has not unmuted yet.
pub fn hush_file() -> std::path::PathBuf {
    crate::utils::get_devices_path().with_file_name("hushed_outputs.json")
}

/// Unmutes outputs left muted by a guard that never finished its rebuild (the app crashed
/// or was killed during it). Run once at startup, before any guard exists.
pub async fn unmute_left_hushed<P: PulseApi>(pulse: &P, file: &std::path::Path) {
    let Ok(json) = tokio::fs::read_to_string(file).await else {
        return;
    };
    let names: Vec<String> = serde_json::from_str(&json).unwrap_or_default();
    if let Ok(snap) = pulse.snapshot().await {
        for sink in snap.sinks.iter().filter(|s| names.contains(&s.name)) {
            info!(
                "Unmuting {} (left muted by an interrupted rebuild)",
                sink.name
            );
            if let Err(e) = pulse.set_sink_mute(sink.index, false).await {
                warn!("Could not unmute {}: {}", sink.name, e);
            }
        }
    }
    let _ = tokio::fs::remove_file(file).await;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Cmd {
    LinkUp,
    LinkDown,
    Wearing(bool),
    Resume,
    RouteBack,
    /// The audio came back from a device that streamed while it had the route: the
    /// AirPods broke this PC's transport, so rebuild the output now instead of waiting
    /// for playback to stall on it.
    Repair,
    /// Another device is playing on the AirPods. This PC's output is dead until they come
    /// back, and that is expected: rebuilding it then would take the AirPods back.
    Away,
    /// Rebuild the output once, now, whatever its state: the AirPods mic stream just
    /// stopped, after which AirPods 4 ANC keep playing on one pod only although the
    /// transport looks healthy. Not rate limited and not counted as a recovery.
    Rebuild,
    /// The AirPods mic stream is about to stop, and AirPods 4 ANC drop the music
    /// transport with it: mute the speakers now, so the moment the AirPods output
    /// vanishes nothing plays there. The rebuild that follows unmutes them, or
    /// [`HUSH_LIMIT`] does if none comes.
    Hush,
    Shutdown,
}

mod decide;
mod hush;
mod monitor;
mod rebuild;

pub use decide::*;

// ---------------------------------------------------------------------------
// State machine
// ---------------------------------------------------------------------------

/// "Was this device the default output?" survives the guard itself: every reconnect creates a
/// new guard, and the answer from before the link dropped is exactly what we need afterwards.
type DefaultMemory = std::sync::Mutex<std::collections::HashMap<String, bool>>;

#[cfg(not(test))]
fn with_default_memory<R>(f: impl FnOnce(&DefaultMemory) -> R) -> R {
    static MEMORY: std::sync::LazyLock<DefaultMemory> = std::sync::LazyLock::new(Default::default);
    f(&MEMORY)
}

// Tests run in parallel with the same fake MAC; keep their memories apart.
#[cfg(test)]
fn with_default_memory<R>(f: impl FnOnce(&DefaultMemory) -> R) -> R {
    thread_local!(static MEMORY: DefaultMemory = Default::default());
    MEMORY.with(f)
}

fn was_default(mac: &str) -> bool {
    with_default_memory(|m| {
        m.lock()
            .map(|m| m.get(mac).copied().unwrap_or(false))
            .unwrap_or(false)
    })
}

#[derive(Debug)]
enum Abort {
    /// Link went down or shutdown was requested while working.
    Interrupted,
    Failed(String),
}

type StepResult<T> = Result<T, Abort>;

pub(crate) struct Guard<P: PulseApi, B: BluezApi> {
    mac: Address,
    mac_str: String,
    pulse: Arc<P>,
    bluez: Arc<B>,
    cfg: GuardConfig,
    health: watch::Sender<AudioHealth>,
    rx: mpsc::UnboundedReceiver<Cmd>,

    linked: bool,
    shutdown: bool,
    /// A link_up/resume arrived while working: abandon the current setup/ladder.
    restart: bool,
    /// A setup or recovery is in progress.
    busy: bool,
    ready: bool,
    ensure_at: Option<Instant>,
    ensure_attempts: u8,
    next_monitor: Option<Instant>,
    stall_since: Option<Instant>,
    last_ladder: Option<Instant>,
    ladders_this_link: u8,
    we_were_default: bool,
    wearing: bool,
    /// Until then, a stall is acted on almost at once: the audio route just came back
    /// from another device, and its transport was probably broken while it was away.
    fast_until: Option<Instant>,
    /// Rebuild the output at the next monitor pass, stall or not.
    repair_pending: bool,
    /// One plain rebuild (`Cmd::Rebuild`) at the next monitor pass.
    rebuild_pending: bool,
    last_repair: Option<Instant>,
    linked_at: Option<Instant>,
    /// Another device has the AirPods: leave the dead output alone until they come back.
    away: bool,
    /// Other outputs muted while ours is rebuilt, so nothing plays on the speakers.
    hushed: Vec<u32>,
    /// Names of `hushed`, as saved to `cfg.hush_file`.
    hushed_names: Vec<String>,
    /// A [`Cmd::Hush`] waiting to be applied.
    hush_pending: bool,
    /// When outputs muted by [`Cmd::Hush`] get unmuted if no rebuild did it first.
    hush_until: Option<Instant>,
    /// A recovery left the output on a lesser codec: since when, to switch back.
    lesser_codec_since: Option<Instant>,
}

impl<P: PulseApi, B: BluezApi> Guard<P, B> {
    pub(crate) fn new(
        mac: Address,
        pulse: Arc<P>,
        bluez: Arc<B>,
        cfg: GuardConfig,
        health: watch::Sender<AudioHealth>,
        rx: mpsc::UnboundedReceiver<Cmd>,
    ) -> Self {
        Guard {
            mac,
            mac_str: mac.to_string().to_uppercase(),
            pulse,
            bluez,
            cfg,
            health,
            rx,
            linked: false,
            shutdown: false,
            restart: false,
            busy: false,
            ready: false,
            ensure_at: None,
            ensure_attempts: 0,
            next_monitor: None,
            stall_since: None,
            last_ladder: None,
            ladders_this_link: 0,
            we_were_default: was_default(&mac.to_string().to_uppercase()),
            wearing: false,
            fast_until: None,
            hush_pending: false,
            hush_until: None,
            lesser_codec_since: None,
            repair_pending: false,
            rebuild_pending: false,
            last_repair: None,
            linked_at: None,
            away: false,
            hushed: Vec::new(),
            hushed_names: Vec::new(),
        }
    }

    pub(crate) async fn run(mut self) {
        info!("[audio {}] guard started", self.mac_str);
        self.startup_cleanup().await;
        while !self.shutdown {
            let wake = [self.ensure_at, self.next_monitor, self.hush_until]
                .into_iter()
                .flatten()
                .min();
            let cmd = match wake {
                Some(t) => tokio::select! {
                    c = self.rx.recv() => Some(c),
                    _ = sleep_until(t) => None,
                },
                None => Some(self.rx.recv().await),
            };
            match cmd {
                Some(Some(c)) => self.on_cmd(c),
                Some(None) => self.shutdown = true,
                None => self.on_timer().await,
            }
            if std::mem::take(&mut self.hush_pending) {
                self.hush().await;
                self.hush_until = Some(Instant::now() + HUSH_LIMIT);
            }
        }
        // Stopped mid-rebuild (sleep, shutdown): never leave the speakers muted.
        self.unmute_hushed().await;
        info!("[audio {}] guard stopped", self.mac_str);
    }

    fn set_health(&self, h: AudioHealth) {
        self.health.send_if_modified(|cur| {
            if *cur != h {
                debug!("[audio {}] health: {:?}", self.mac_str, h);
                *cur = h;
                true
            } else {
                false
            }
        });
    }

    fn on_cmd(&mut self, cmd: Cmd) {
        let now = Instant::now();
        debug!("[audio {}] command {:?}", self.mac_str, cmd);
        match cmd {
            Cmd::LinkUp => {
                self.away = false;
                self.linked_at = Some(now);
                self.linked = true;
                self.restart = self.busy;
                self.ready = false;
                self.ensure_attempts = 0;
                self.ladders_this_link = 0;
                self.stall_since = None;
                self.next_monitor = None;
                self.ensure_at = Some(now + self.cfg.link_debounce);
                self.set_health(AudioHealth::Negotiating);
            }
            Cmd::LinkDown => {
                self.away = false;
                self.linked = false;
                self.ready = false;
                self.ensure_at = None;
                self.next_monitor = None;
                self.stall_since = None;
                self.set_health(AudioHealth::Disconnected);
            }
            Cmd::Wearing(in_ear) => {
                // Never touches the profile on removal; only re-verifies on insertion.
                self.wearing = in_ear;
                if in_ear && self.linked && !self.busy {
                    if self.ready {
                        self.next_monitor = Some(now);
                    } else if self.ensure_at.is_none() {
                        self.ensure_attempts = 0;
                        self.ensure_at = Some(now + self.cfg.poll);
                    }
                }
            }
            Cmd::Resume => {
                if self.linked {
                    self.restart = self.busy;
                    self.ready = false;
                    self.ensure_attempts = 0;
                    self.next_monitor = None;
                    self.stall_since = None;
                    self.ensure_at = Some(now + self.cfg.link_debounce);
                    self.set_health(AudioHealth::Negotiating);
                }
            }
            Cmd::RouteBack => {
                self.away = false;
                if self.linked {
                    // An explicit take-back: check right away, act on a stall within about
                    // a second, and don't let earlier recoveries rate-limit this one.
                    self.fast_until = Some(now + FAST_WINDOW);
                    self.last_ladder = None;
                    self.ladders_this_link = 0;
                    self.stall_since = None;
                    if self.ready {
                        self.next_monitor = Some(now);
                    } else if self.ensure_at.is_none() && !self.busy {
                        self.ensure_attempts = 0;
                        self.ensure_at = Some(now + self.cfg.poll);
                    }
                }
            }
            Cmd::Repair => {
                if self.linked {
                    self.on_cmd(Cmd::RouteBack);
                    let fresh = self
                        .linked_at
                        .is_some_and(|t| now - t < self.cfg.fresh_link);
                    if !fresh && self.last_repair.is_none_or(|t| now - t >= REPAIR_COOLDOWN) {
                        self.last_repair = Some(now);
                        self.repair_pending = true;
                    }
                }
            }
            Cmd::Away => {
                if !self.away {
                    info!(
                        "[audio {}] another device is playing on the AirPods; pausing stall checks",
                        self.mac_str
                    );
                }
                self.away = true;
                self.stall_since = None;
                self.repair_pending = false;
            }
            Cmd::Rebuild => {
                if self.linked && !self.away {
                    // Like a take-back: if the AirPods still don't play after the
                    // rebuild, act on it within about a second, not after four.
                    self.fast_until = Some(now + FAST_WINDOW);
                    self.last_ladder = None;
                    self.ladders_this_link = 0;
                    self.stall_since = None;
                    self.rebuild_pending = true;
                    if self.ready {
                        self.next_monitor = Some(now);
                    }
                }
            }
            Cmd::Hush => {
                if self.linked && !self.away {
                    self.hush_pending = true;
                }
            }
            Cmd::Shutdown => self.shutdown = true,
        }
    }

    /// Applies queued commands; returns `Interrupted` if the work in progress must stop.
    fn check_interrupt(&mut self) -> StepResult<()> {
        while let Ok(cmd) = self.rx.try_recv() {
            self.on_cmd(cmd);
        }
        if self.shutdown || !self.linked || self.restart {
            // Link down, shutdown, or a fresh link_up/resume rescheduled the setup.
            return Err(Abort::Interrupted);
        }
        Ok(())
    }

    async fn pause(&mut self, d: Duration) -> StepResult<()> {
        sleep(d).await;
        self.check_interrupt()
    }

    async fn on_timer(&mut self) {
        let now = Instant::now();
        self.busy = true;
        self.restart = false;
        if self.ensure_at.is_some_and(|t| t <= now) {
            self.ensure_at = None;
            self.ensure().await;
        } else if self.next_monitor.is_some_and(|t| t <= now) {
            self.next_monitor = None;
            self.monitor().await;
        } else if self.hush_until.is_some_and(|t| t <= now) {
            debug!(
                "[audio {}] no rebuild came; unmuting the speakers",
                self.mac_str
            );
            self.unmute_hushed().await;
        }
        self.busy = false;
        self.restart = false;
    }

    /// Old versions set the card to `off` when the buds left the ear and WirePlumber
    /// saved it. If the card is present and `off`, fix it once at startup.
    async fn startup_cleanup(&mut self) {
        let Ok(snap) = self.pulse.snapshot().await else {
            return;
        };
        let Some(card) = snap.card_for(&self.mac_str) else {
            return;
        };
        if is_off(card.active_profile.as_deref()) {
            info!(
                "[audio {}] card {} is on 'off' (saved by an older version?), restoring A2DP",
                self.mac_str, card.name
            );
            let candidates = a2dp_candidates(card);
            if let Err(e) = self.apply_profile(&candidates).await {
                warn!("[audio {}] startup cleanup failed: {:?}", self.mac_str, e);
            }
        }
    }

    // ----- setup path ------------------------------------------------------

    async fn ensure(&mut self) {
        if !self.linked {
            return;
        }
        self.ensure_attempts = self.ensure_attempts.saturating_add(1);
        self.set_health(AudioHealth::Negotiating);
        match self.ensure_audio().await {
            Ok(health) => {
                info!("[audio {}] ready: {:?}", self.mac_str, health);
                // The limit is for setups that keep failing, not for a card that comes
                // back every time it goes.
                self.ensure_attempts = 0;
                // WirePlumber remembers a fallback codec as the device's profile: go
                // back to the best one (once nothing plays) rather than keep it forever.
                self.note_codec().await;
                self.ready = true;
                self.stall_since = None;
                self.set_health(health);
                let now = Instant::now();
                self.next_monitor = Some(if self.fast_until.is_some_and(|t| now < t) {
                    now + self.cfg.poll
                } else {
                    now + self.cfg.monitor_interval
                });
            }
            Err(Abort::Interrupted) => {
                debug!("[audio {}] setup interrupted", self.mac_str);
            }
            Err(Abort::Failed(reason)) => {
                warn!("[audio {}] setup failed: {}", self.mac_str, reason);
                self.ready = false;
                self.set_health(AudioHealth::Failed { reason });
                if self.ensure_attempts < self.cfg.max_ensure_attempts {
                    self.ensure_at = Some(Instant::now() + self.cfg.retry_after_failure);
                }
            }
        }
        self.ensure_not_off().await;
    }

    async fn ensure_audio(&mut self) -> StepResult<AudioHealth> {
        let (_snap, card) = self.wait_for_card_or_connect().await?;
        let profile = self.check_profile(&card).await?;
        self.check_sink(&profile).await
    }

    /// Polls until the card for our MAC exists, up to `wait`.
    async fn wait_card(&mut self, wait: Duration) -> StepResult<Option<(PulseSnapshot, CardInfo)>> {
        let deadline = Instant::now() + wait;
        let mut last_err = None;
        let mut any_ok = false;
        loop {
            match self.pulse.snapshot().await {
                Ok(snap) => {
                    any_ok = true;
                    if let Some(card) = snap.card_for(&self.mac_str).cloned() {
                        return Ok(Some((snap, card)));
                    }
                }
                Err(e) => last_err = Some(e),
            }
            if Instant::now() >= deadline {
                break;
            }
            self.pause(self.cfg.poll).await?;
        }
        match (any_ok, last_err) {
            (false, Some(e)) => Err(Abort::Failed(e.to_string())),
            _ => Ok(None),
        }
    }

    async fn wait_for_card_or_connect(&mut self) -> StepResult<(PulseSnapshot, CardInfo)> {
        let mut connects = 0u8;
        loop {
            if let Some(found) = self.wait_card(self.cfg.card_wait).await? {
                return Ok(found);
            }
            let transports = match self.bluez.transport_states(self.mac).await {
                Ok(t) => t,
                Err(e) => {
                    debug!("[audio {}] transport query failed: {}", self.mac_str, e);
                    Vec::new()
                }
            };
            if connects >= self.cfg.connect_profile_retries {
                return Err(Abort::Failed(if transports.is_empty() {
                    "A2DP did not connect (no audio device after ConnectProfile)".into()
                } else {
                    "Bluetooth audio transport exists but no sound card appeared".into()
                }));
            }
            connects += 1;
            if transports.is_empty() {
                info!(
                    "[audio {}] no bluez card and no MediaTransport1: ConnectProfile(A2DP) attempt {}",
                    self.mac_str, connects
                );
                if let Err(e) = self.bluez.connect_profile(self.mac, A2DP_SINK_UUID).await {
                    warn!("[audio {}] {}", self.mac_str, e);
                }
                self.check_interrupt()?;
            } else {
                debug!(
                    "[audio {}] transport present ({:?}) but no card yet, waiting",
                    self.mac_str, transports
                );
            }
        }
    }

    /// Makes sure the card is on a usable profile; returns the active profile.
    async fn check_profile(&mut self, card: &CardInfo) -> StepResult<String> {
        // Recording state needs a fresh snapshot including sources.
        let snap = self
            .pulse
            .snapshot()
            .await
            .map_err(|e| Abort::Failed(e.to_string()))?;
        let card = snap
            .card_for(&self.mac_str)
            .cloned()
            .unwrap_or_else(|| card.clone());
        let recording = snap.is_recording_from_card(card.index);
        match decide_profile(&card, recording) {
            ProfileAction::Keep => Ok(card.active_profile.clone().unwrap_or_default()),
            ProfileAction::LeaveHeadset => {
                info!(
                    "[audio {}] on {:?} with the microphone in use, leaving it",
                    self.mac_str, card.active_profile
                );
                Ok(card.active_profile.clone().unwrap_or_default())
            }
            ProfileAction::Set(candidates) => {
                info!(
                    "[audio {}] profile {:?} is not usable, switching (candidates {:?})",
                    self.mac_str, card.active_profile, candidates
                );
                self.apply_profile(&candidates).await
            }
        }
    }

    /// Sets the first candidate that sticks; verifies by reading it back.
    async fn apply_profile(&mut self, candidates: &[String]) -> StepResult<String> {
        if candidates.is_empty() {
            return Err(Abort::Failed("the device offers no A2DP profile".into()));
        }
        for profile in candidates {
            let Some(card) = self.current_card().await else {
                return Err(Abort::Failed("audio device disappeared".into()));
            };
            if let Err(e) = self.pulse.set_card_profile(card.index, profile).await {
                warn!(
                    "[audio {}] set profile {} failed: {}",
                    self.mac_str, profile, e
                );
                continue;
            }
            if self.verify_profile(profile).await {
                info!("[audio {}] profile set to {}", self.mac_str, profile);
                return Ok(profile.clone());
            }
            warn!("[audio {}] profile {} did not stick", self.mac_str, profile);
        }
        Err(Abort::Failed("could not activate any A2DP profile".into()))
    }

    async fn current_card(&self) -> Option<CardInfo> {
        let snap = self.pulse.snapshot().await.ok()?;
        snap.card_for(&self.mac_str).cloned()
    }

    async fn verify_profile(&self, profile: &str) -> bool {
        let deadline = Instant::now() + self.cfg.verify_timeout;
        loop {
            if let Some(card) = self.current_card().await
                && card.active_profile.as_deref() == Some(profile)
            {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            sleep(self.cfg.poll).await;
        }
    }

    /// Waits for the sink, restores the default if appropriate, returns Ready.
    async fn check_sink(&mut self, profile: &str) -> StepResult<AudioHealth> {
        let deadline = Instant::now() + self.cfg.verify_timeout;
        let (snap, sink) = loop {
            let snap = self
                .pulse
                .snapshot()
                .await
                .map_err(|e| Abort::Failed(e.to_string()))?;
            let sink = snap
                .card_for(&self.mac_str)
                .and_then(|c| snap.sink_for_card(c))
                .cloned();
            if let Some(sink) = sink {
                break (snap, sink);
            }
            if Instant::now() >= deadline {
                return Err(Abort::Failed(format!(
                    "profile {profile} is active but no audio output appeared"
                )));
            }
            self.pause(self.cfg.poll).await?;
        };
        let action = decide_sink(&snap, &sink, self.we_were_default);
        self.remember_default(snap.default_sink.as_deref() == Some(sink.name.as_str()));
        if let SinkAction::MakeDefault {
            sink_name,
            sink_index,
            move_inputs,
        } = action
        {
            info!(
                "[audio {}] restoring {} as default output (was {:?}), moving {} stream(s)",
                self.mac_str,
                sink_name,
                snap.default_sink,
                move_inputs.len()
            );
            match self.pulse.set_default_sink(&sink_name).await {
                Ok(()) => self.remember_default(true),
                Err(e) => warn!("[audio {}] set default sink failed: {}", self.mac_str, e),
            }
            for input in move_inputs {
                if let Err(e) = self.pulse.move_sink_input(input, sink_index).await {
                    debug!(
                        "[audio {}] move stream {} failed: {}",
                        self.mac_str, input, e
                    );
                }
            }
        }
        Ok(AudioHealth::Ready {
            profile: profile.to_string(),
            codec: codec_for(profile, Some(&sink)),
        })
    }
}
