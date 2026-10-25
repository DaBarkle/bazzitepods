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
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Cmd {
    LinkUp,
    LinkDown,
    Wearing(bool),
    Resume,
    RouteBack,
    Shutdown,
}

// ---------------------------------------------------------------------------
// Pure decision helpers
// ---------------------------------------------------------------------------

/// How long after a take-back stalls are acted on quickly.
const FAST_WINDOW: Duration = Duration::from_secs(10);
/// Stall threshold inside that window.
const FAST_STALL: Duration = Duration::from_millis(800);

const PREFERRED_A2DP: [&str; 4] = [
    "a2dp-sink-aac",
    "a2dp-sink",
    "a2dp-sink-sbc_xq",
    "a2dp-sink-sbc",
];

pub fn is_a2dp(profile: &str) -> bool {
    profile.starts_with("a2dp")
}

pub fn is_headset(profile: &str) -> bool {
    profile.starts_with("headset") || profile.starts_with("handsfree")
}

pub fn is_off(profile: Option<&str>) -> bool {
    matches!(profile, None | Some("") | Some("off"))
}

/// A2DP sink profiles of a card, best first: AAC, auto, SBC-XQ, SBC, then the rest
/// by priority. Unavailable profiles are dropped unless nothing else is left.
pub fn a2dp_candidates(card: &CardInfo) -> Vec<String> {
    let rank = |avail_only: bool| {
        let mut list: Vec<&super::pulse::ProfileInfo> = card
            .profiles
            .iter()
            .filter(|p| p.name.starts_with("a2dp-sink") && (!avail_only || p.available))
            .collect();
        list.sort_by_key(|p| {
            let pref = PREFERRED_A2DP
                .iter()
                .position(|n| *n == p.name)
                .unwrap_or(PREFERRED_A2DP.len());
            (pref, std::cmp::Reverse(p.priority))
        });
        list.into_iter().map(|p| p.name.clone()).collect::<Vec<_>>()
    };
    let available = rank(true);
    if available.is_empty() {
        rank(false)
    } else {
        available
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProfileAction {
    /// Already on a usable profile.
    Keep,
    /// On HFP with the mic in use: do not touch.
    LeaveHeadset,
    /// Switch, trying these in order.
    Set(Vec<String>),
}

pub fn decide_profile(card: &CardInfo, recording: bool) -> ProfileAction {
    let active = card.active_profile.as_deref();
    match active {
        a if is_off(a) => ProfileAction::Set(a2dp_candidates(card)),
        Some(p) if is_a2dp(p) => ProfileAction::Keep,
        Some(p) if is_headset(p) => {
            if recording {
                ProfileAction::LeaveHeadset
            } else {
                ProfileAction::Set(a2dp_candidates(card))
            }
        }
        // Something else the user picked on purpose (pro-audio etc.): respect it.
        _ => ProfileAction::Keep,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SinkAction {
    Nothing,
    MakeDefault {
        sink_name: String,
        sink_index: u32,
        move_inputs: Vec<u32>,
    },
}

/// Restore our sink as default only when it was the default before the link
/// dropped (so the current default is just WirePlumber's fallback); never
/// override a default the user picked while we were away... or one we never had.
pub fn decide_sink(snap: &PulseSnapshot, ours: &SinkInfo, we_were_default: bool) -> SinkAction {
    if snap.default_sink.as_deref() == Some(ours.name.as_str()) || !we_were_default {
        return SinkAction::Nothing;
    }
    let fallback = snap
        .default_sink
        .as_deref()
        .and_then(|n| snap.sink_by_name(n))
        .map(|s| s.index);
    let move_inputs = snap
        .sink_inputs
        .iter()
        .filter(|i| Some(i.sink) == fallback)
        .map(|i| i.index)
        .collect();
    SinkAction::MakeDefault {
        sink_name: ours.name.clone(),
        sink_index: ours.index,
        move_inputs,
    }
}

/// Codec for display: PipeWire's `api.bluez5.codec`, else derived from the profile.
pub fn codec_for(profile: &str, sink: Option<&SinkInfo>) -> Option<String> {
    if let Some(c) = sink.and_then(|s| s.codec.clone()) {
        return Some(c);
    }
    for prefix in ["a2dp-sink-", "headset-head-unit-"] {
        if let Some(c) = profile.strip_prefix(prefix) {
            return Some(c.to_string());
        }
    }
    None
}

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
        }
    }

    pub(crate) async fn run(mut self) {
        info!("[audio {}] guard started", self.mac_str);
        self.startup_cleanup().await;
        while !self.shutdown {
            let wake = [self.ensure_at, self.next_monitor]
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
        }
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

    fn remember_default(&mut self, value: bool) {
        self.we_were_default = value;
        let mac = self.mac_str.clone();
        with_default_memory(|m| {
            if let Ok(mut m) = m.lock() {
                m.insert(mac, value);
            }
        });
    }

    /// Streams currently playing to this device's sink (to bring back after a profile toggle).
    async fn our_streams(&self) -> Vec<u32> {
        let Ok(snap) = self.pulse.snapshot().await else {
            return Vec::new();
        };
        let Some(sink) = snap
            .card_for(&self.mac_str)
            .and_then(|c| snap.sink_for_card(c))
        else {
            return Vec::new();
        };
        snap.sink_inputs
            .iter()
            .filter(|i| i.sink == sink.index)
            .map(|i| i.index)
            .collect()
    }

    /// A profile toggle recreates the sink and WirePlumber parks streams on the fallback
    /// (speakers). Move them back, and restore the default if it was ours.
    async fn restore_streams(&mut self, streams: &[u32]) {
        let deadline = Instant::now() + self.cfg.verify_timeout;
        let (snap, sink) = loop {
            let Ok(snap) = self.pulse.snapshot().await else {
                return;
            };
            if let Some(sink) = snap
                .card_for(&self.mac_str)
                .and_then(|c| snap.sink_for_card(c))
                .cloned()
            {
                break (snap, sink);
            }
            if Instant::now() >= deadline {
                return;
            }
            sleep(self.cfg.poll).await;
        };
        if self.we_were_default && snap.default_sink.as_deref() != Some(sink.name.as_str()) {
            if let Err(e) = self.pulse.set_default_sink(&sink.name).await {
                debug!("[audio {}] set default sink failed: {}", self.mac_str, e);
            }
        }
        for input in snap
            .sink_inputs
            .iter()
            .filter(|i| streams.contains(&i.index) && i.sink != sink.index)
        {
            if let Err(e) = self.pulse.move_sink_input(input.index, sink.index).await {
                debug!(
                    "[audio {}] move stream {} failed: {}",
                    self.mac_str, input.index, e
                );
            }
        }
    }

    /// Rule 6: whatever happened, never leave the card on `off`.
    async fn ensure_not_off(&mut self) {
        let Some(card) = self.current_card().await else {
            return;
        };
        if is_off(card.active_profile.as_deref()) {
            warn!("[audio {}] card is on 'off', restoring A2DP", self.mac_str);
            let candidates = a2dp_candidates(&card);
            if let Err(e) = self.apply_profile(&candidates).await {
                warn!("[audio {}] could not restore A2DP: {:?}", self.mac_str, e);
            }
        }
    }

    // ----- ready path ------------------------------------------------------

    async fn monitor(&mut self) {
        if !self.linked || !self.ready {
            return;
        }
        let now = Instant::now();
        let every = if self.fast_until.is_some_and(|t| now < t) {
            self.cfg.poll
        } else {
            self.cfg.monitor_interval
        };
        self.next_monitor = Some(now + every);
        let Ok(snap) = self.pulse.snapshot().await else {
            return;
        };
        let Some(card) = snap.card_for(&self.mac_str) else {
            info!(
                "[audio {}] audio card vanished while linked, re-checking",
                self.mac_str
            );
            self.lose_ready(now + self.cfg.link_debounce);
            return;
        };
        let active = card.active_profile.clone().unwrap_or_default();
        if is_off(Some(&active)) {
            info!(
                "[audio {}] card switched to 'off' externally, fixing",
                self.mac_str
            );
            self.lose_ready(now);
            return;
        }
        let Some(sink) = snap.sink_for_card(card).cloned() else {
            self.stall_since = None;
            return;
        };
        self.remember_default(snap.default_sink.as_deref() == Some(sink.name.as_str()));
        // Healthy (also clears an earlier stall failure once audio flows again).
        let healthy = AudioHealth::Ready {
            codec: codec_for(&active, Some(&sink)),
            profile: active.clone(),
        };

        if !is_a2dp(&active)
            || sink.state != SinkState::Running
            || snap.uncorked_inputs_on(sink.index) == 0
        {
            self.stall_since = None;
            self.set_health(healthy);
            return;
        }
        let states = match self.bluez.transport_states(self.mac).await {
            Ok(s) => s,
            Err(e) => {
                debug!("[audio {}] transport query failed: {}", self.mac_str, e);
                return;
            }
        };
        if states.iter().any(|s| s == "active") {
            self.stall_since = None;
            self.set_health(healthy);
            return;
        }
        let since = *self.stall_since.get_or_insert(now);
        debug!(
            "[audio {}] sink running but transport {:?} for {:?}",
            self.mac_str,
            states,
            now - since
        );
        let fast = self.fast_until.is_some_and(|t| now < t);
        let threshold = if fast {
            FAST_STALL.min(self.cfg.stall_threshold)
        } else {
            self.cfg.stall_threshold
        };
        if now - since >= threshold {
            self.maybe_run_ladder().await;
        } else {
            // Look again soon so detection is close to the threshold.
            self.next_monitor = Some(now + self.cfg.poll.min(self.cfg.monitor_interval));
        }
    }

    fn lose_ready(&mut self, ensure_at: Instant) {
        self.ready = false;
        self.next_monitor = None;
        self.stall_since = None;
        if self.ensure_attempts >= self.cfg.max_ensure_attempts {
            self.set_health(AudioHealth::Failed {
                reason: "audio device keeps disappearing".into(),
            });
            return;
        }
        self.set_health(AudioHealth::Negotiating);
        self.ensure_at = Some(ensure_at);
    }

    async fn maybe_run_ladder(&mut self) {
        let now = Instant::now();
        if let Some(last) = self.last_ladder
            && now - last < self.cfg.ladder_cooldown
        {
            debug!(
                "[audio {}] stall persists, recovery rate-limited",
                self.mac_str
            );
            return;
        }
        if self.ladders_this_link >= self.cfg.max_ladders_per_link {
            self.set_health(AudioHealth::Failed {
                reason: "audio keeps stalling; reconnect the AirPods".into(),
            });
            return;
        }
        self.last_ladder = Some(now);
        self.ladders_this_link += 1;
        self.stall_since = None;
        warn!(
            "[audio {}] audio stalled, starting recovery #{}",
            self.mac_str, self.ladders_this_link
        );
        let result = self.run_ladder().await;
        self.ensure_not_off().await;
        match result {
            Ok(Some(step)) => {
                info!("[audio {}] recovered at step {}", self.mac_str, step);
                if let Some(card) = self.current_card().await {
                    let profile = card.active_profile.unwrap_or_default();
                    self.set_health(AudioHealth::Ready {
                        codec: codec_for(&profile, None),
                        profile,
                    });
                }
            }
            Ok(None) => {
                warn!("[audio {}] recovery exhausted", self.mac_str);
                self.set_health(AudioHealth::Failed {
                    reason: "audio stalled and could not be recovered".into(),
                });
            }
            Err(Abort::Failed(reason)) => {
                self.set_health(AudioHealth::Failed { reason });
            }
            Err(Abort::Interrupted) => {}
        }
        // Monitoring continues (next_monitor is already set); rate limits bound it.
        if self.ready && self.next_monitor.is_none() {
            self.next_monitor = Some(Instant::now() + self.cfg.monitor_interval);
        }
    }

    /// Returns `Ok(Some(step))` when recovered, `Ok(None)` when every step failed.
    async fn run_ladder(&mut self) -> StepResult<Option<u8>> {
        let Some(card) = self.current_card().await else {
            return Err(Abort::Failed("audio device disappeared".into()));
        };
        let current = card
            .active_profile
            .clone()
            .filter(|p| is_a2dp(p))
            .unwrap_or_default();
        let candidates = a2dp_candidates(&card);
        let streams = self.our_streams().await;

        // Step 1: toggle off -> same A2DP profile. Done back-to-back without
        // interruption checks so we can never stop in between.
        self.set_health(AudioHealth::Recovering { step: 1 });
        let mut order = vec![current.clone()];
        order.extend(candidates.iter().filter(|c| **c != current).cloned());
        order.retain(|p| !p.is_empty());
        if order.is_empty() {
            // Never toggle to "off" without a profile to come back to.
            return Err(Abort::Failed("the device offers no A2DP profile".into()));
        }
        if let Err(e) = self.pulse.set_card_profile(card.index, "off").await {
            warn!("[audio {}] toggle to off failed: {}", self.mac_str, e);
        }
        sleep(self.cfg.toggle_gap).await;
        let step1 = self.apply_profile(&order).await;
        self.restore_streams(&streams).await;
        self.check_interrupt()?;
        if step1.is_ok() && self.stall_resolved().await {
            return Ok(Some(1));
        }
        self.check_interrupt()?;

        // Step 2: alternate codec.
        self.set_health(AudioHealth::Recovering { step: 2 });
        let active_now = self
            .current_card()
            .await
            .and_then(|c| c.active_profile)
            .unwrap_or_default();
        let alternates: Vec<String> = candidates
            .iter()
            .filter(|c| **c != active_now && **c != current)
            .cloned()
            .collect();
        if !alternates.is_empty() {
            let step2 = self.apply_profile(&alternates).await;
            self.restore_streams(&streams).await;
            self.check_interrupt()?;
            if step2.is_ok() && self.stall_resolved().await {
                return Ok(Some(2));
            }
            self.check_interrupt()?;
        }

        // Step 3: reconnect the A2DP profile.
        self.set_health(AudioHealth::Recovering { step: 3 });
        if let Err(e) = self
            .bluez
            .disconnect_profile(self.mac, A2DP_SINK_UUID)
            .await
        {
            warn!("[audio {}] {}", self.mac_str, e);
        }
        self.pause(self.cfg.reconnect_gap).await?;
        if let Err(e) = self.bluez.connect_profile(self.mac, A2DP_SINK_UUID).await {
            warn!("[audio {}] {}", self.mac_str, e);
        }
        self.check_interrupt()?;
        if let Some((_, card)) = self.wait_card(self.cfg.card_wait).await?
            && self.check_profile(&card).await.is_ok()
            && {
                self.restore_streams(&streams).await;
                self.stall_resolved().await
            }
        {
            return Ok(Some(3));
        }
        Ok(None)
    }

    /// True once audio flows again (transport active) or nothing is playing.
    async fn stall_resolved(&self) -> bool {
        let deadline = Instant::now() + self.cfg.verify_timeout;
        loop {
            if let Ok(snap) = self.pulse.snapshot().await
                && let Some(card) = snap.card_for(&self.mac_str)
                && let Some(sink) = snap.sink_for_card(card)
            {
                let playing =
                    sink.state == SinkState::Running && snap.uncorked_inputs_on(sink.index) > 0;
                if !playing {
                    return true;
                }
                if let Ok(states) = self.bluez.transport_states(self.mac).await
                    && states.iter().any(|s| s == "active")
                {
                    return true;
                }
            }
            if Instant::now() >= deadline {
                return false;
            }
            sleep(self.cfg.poll).await;
        }
    }
}
