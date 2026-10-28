//! Per-AirPods device task.
//!
//! The supervisor in main.rs spawns `run` when BlueZ reports the AirPods connected and cancels it
//! when they disconnect. Inside, the AACP control channel is (re)opened with backoff for as long as
//! the Bluetooth link is up, so walking out of range and back never needs user action.

use crate::audio::{AudioGuard, AudioGuardHandle, AudioHealth};
use crate::battery_service::{BatteryInput, BatteryService};
use crate::bluetooth::aacp::{
    AACPEvent, AACPManager, AirPodsLEKeys, AudioSource, AudioSourceType, ControlCommandIdentifiers,
    EarDetectionStatus, ProximityKeyType,
};
use crate::bluetooth::managers::DeviceManagers;
use crate::core::models::ModelInfo;
use crate::devices::enums::DeviceInformation;
use crate::hub::{Hub, LinkState};
use crate::media_controller::MediaController;
use crate::mic::switch::{MicReport, MicRequest, MicSwitchHandle, SwitchConfig};
use crate::mic::{AacpMicLink, MicRoute, PulseMicHost};
use crate::ui::messages::BluetoothUIMessage;
use crate::utils::get_app_settings_path;
use bluer::Address;
use log::{debug, info, warn};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock, mpsc};
use tokio::task::JoinSet;
use tokio::time::{Duration, Instant, sleep};
use tokio_util::sync::CancellationToken;

pub type CommandSender = mpsc::UnboundedSender<(ControlCommandIdentifiers, Vec<u8>)>;

/// Everything a device task needs from the rest of the app.
#[derive(Clone)]
pub struct DeviceContext {
    pub hub: Hub,
    pub battery: BatteryService,
    pub ui_tx: mpsc::UnboundedSender<BluetoothUIMessage>,
    pub device_managers: Arc<RwLock<HashMap<String, DeviceManagers>>>,
    /// The tray/overlay route control commands (listening mode etc.) through this.
    pub active_commands: Arc<Mutex<Option<CommandSender>>>,
    pub local_mac: String,
}

/// Backoff between control-channel attempts while the Bluetooth link stays up.
const RETRY_DELAYS: [u64; 6] = [1, 2, 3, 5, 8, 13];
/// How often we check that the control channel is still alive.
const HEALTH_INTERVAL: Duration = Duration::from_secs(2);
/// When the stem claim is sent a second time after connecting.
const CLAIM_RESEND_AFTER: Duration = Duration::from_secs(6);
/// A session step that takes longer than this is stuck: the control channel is dropped
/// and opened again, so battery and controls come back without restarting the app.
const SESSION_STUCK: Duration = Duration::from_secs(20);
/// How long a finished session waits for its audio guard to end a repair in progress.
const GUARD_STOP_WAIT: Duration = Duration::from_secs(30);

/// What the session is doing right now, for the stuck-session watchdog.
#[derive(Clone, Default)]
struct Progress(Arc<std::sync::Mutex<Option<(&'static str, Instant)>>>);

impl Progress {
    fn busy(&self, step: &'static str) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = Some((step, Instant::now()));
    }

    fn idle(&self) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    /// The step the session has been on for longer than `limit`, if any.
    fn stuck(&self, limit: Duration) -> Option<&'static str> {
        let current = *self.0.lock().unwrap_or_else(|e| e.into_inner());
        current.and_then(|(step, since)| (since.elapsed() >= limit).then_some(step))
    }

    /// Resolves with the step once one has taken longer than `limit`.
    async fn watch(&self, limit: Duration) -> &'static str {
        loop {
            sleep(limit / 4).await;
            if let Some(step) = self.stuck(limit) {
                return step;
            }
        }
    }
}

pub async fn run(mac: Address, name: String, ctx: DeviceContext, cancel: CancellationToken) {
    let mac_s = mac.to_string();
    info!("Device task started for {} ({})", name, mac_s);
    ctx.hub.update_device(&mac_s, &name, |d| {
        d.name = name.clone();
        d.link = LinkState::Connecting;
        d.in_case = false;
        d.last_seen = Some(std::time::SystemTime::now());
    });

    let media = Arc::new(Mutex::new(MediaController::new(
        mac_s.clone(),
        ctx.local_mac.clone(),
    )));
    {
        let media = media.lock().await.clone();
        tokio::spawn(async move { media.restore_pending_volume().await });
    }

    // The audio guard runs for the whole Bluetooth link, independent of the control channel:
    // audio must keep working even when AACP is down.
    let audio = match bluer::Session::new().await {
        Ok(session) => {
            let guard = AudioGuard::spawn(mac, session);
            guard.link_up();
            spawn_health_mirror(&guard, &ctx, &mac_s, &name);
            Some(guard)
        }
        Err(e) => {
            warn!("Audio guard unavailable for {}: {}", mac_s, e);
            None
        }
    };

    let mut attempt: u32 = 0;
    loop {
        if cancel.is_cancelled() {
            break;
        }
        let aacp = tokio::select! {
            _ = cancel.cancelled() => break,
            aacp = connect_aacp(mac) => aacp,
        };
        let Some(aacp) = aacp else {
            let delay = RETRY_DELAYS[(attempt as usize).min(RETRY_DELAYS.len() - 1)];
            attempt += 1;
            // After a few failures the link is probably audio-only (e.g. another host owns the
            // control channel). Keep retrying slowly but tell the UI we're in a limited state.
            let link = if attempt > 3 {
                LinkState::Limited
            } else {
                LinkState::Reconnecting { attempt }
            };
            ctx.hub.set_link(&mac_s, link);
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = sleep(Duration::from_secs(delay)) => continue,
            }
        };
        attempt = 0;

        let progress = Progress::default();
        tokio::select! {
            _ = session(mac, &name, &aacp, &ctx, &media, audio.as_ref(), &cancel, &progress) => {}
            step = progress.watch(SESSION_STUCK) => warn!(
                "Control channel to {} stopped responding (stuck in {} for over {:?}); reconnecting",
                mac_s, step, SESSION_STUCK
            ),
        }

        aacp.shutdown().await;
        media.lock().await.end_conversation().await;
        media.lock().await.stop_playback_listener().await;
        ctx.battery.send(BatteryInput::AacpLink(mac, false));
        release_shared_state(&ctx, &mac_s, &aacp).await;
        if cancel.is_cancelled() {
            break;
        }
        info!(
            "Control channel to {} dropped while link is up; reconnecting",
            mac_s
        );
        ctx.hub
            .set_link(&mac_s, LinkState::Reconnecting { attempt: 1 });
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = sleep(Duration::from_secs(1)) => {}
        }
    }

    ctx.battery.send(BatteryInput::LinkLost(mac));
    if let Some(audio) = &audio {
        audio.link_down();
        audio.shutdown();
    }
    // Link lost while being worn (walked away): pause like iPhone does, instead of letting
    // the music jump to the speakers.
    let was_worn = ctx
        .hub
        .snapshot()
        .devices
        .get(&mac_s)
        .is_some_and(|d| d.in_ear.0 == Some(true) || d.in_ear.1 == Some(true));
    let controller = media.lock().await;
    // Quitting the app is not walking away: leave the music playing.
    if was_worn && !crate::quitting() {
        controller.pause_all_media().await;
    }
    controller.end_conversation().await;
    controller.stop_playback_listener().await;
    drop(controller);

    ctx.hub.update_device(&mac_s, &name, |d| {
        d.link = LinkState::Disconnected;
        d.in_case = dropped_in_case(&mac_s);
        d.in_ear = (None, None);
        d.audio = None;
        d.last_seen = Some(std::time::SystemTime::now());
    });
    // Wait for the guard to finish a repair in progress: the supervisor starts the next
    // session for these AirPods only once this task ends, so two guards never change the
    // same card at once.
    if let Some(audio) = &audio
        && tokio::time::timeout(GUARD_STOP_WAIT, audio.stopped())
            .await
            .is_err()
    {
        warn!(
            "Audio guard for {} is still busy; not waiting longer",
            mac_s
        );
    }
    info!("Device task finished for {}", mac_s);
}

/// Clear the device-manager entry and tray command route, but only if they still belong to
/// this session (a newer session for the same AirPods may already have replaced them).
async fn release_shared_state(ctx: &DeviceContext, mac_s: &str, aacp: &AACPManager) {
    {
        let mut managers = ctx.device_managers.write().await;
        let ours = managers
            .get(mac_s)
            .and_then(|m| m.get_aacp())
            .is_some_and(|m| Arc::ptr_eq(&m.state, &aacp.state));
        if ours {
            managers.remove(mac_s);
        }
    }
    let mut active = ctx.active_commands.lock().await;
    if active.as_ref().is_some_and(|tx| tx.is_closed()) {
        *active = None;
    }
}

async fn connect_aacp(mac: Address) -> Option<AACPManager> {
    let mut aacp = AACPManager::new();
    aacp.connect(mac).await;
    if aacp.is_connected().await {
        Some(aacp)
    } else {
        aacp.shutdown().await;
        None
    }
}

/// One control-channel session. Returns when the channel drops or the task is cancelled.
#[allow(clippy::too_many_arguments)]
async fn session(
    mac: Address,
    name: &str,
    aacp: &AACPManager,
    ctx: &DeviceContext,
    media: &Arc<Mutex<MediaController>>,
    audio: Option<&AudioGuardHandle>,
    cancel: &CancellationToken,
    progress: &Progress,
) {
    let mac_s = mac.to_string();
    let mut tasks = JoinSet::new();
    progress.busy("handshake");

    let (event_tx, mut event_rx) = mpsc::unbounded_channel();
    aacp.set_event_channel(event_tx).await;

    if let Err(e) = handshake(aacp).await {
        warn!("Handshake with {} failed: {}", mac_s, e);
        return;
    }

    ctx.device_managers
        .write()
        .await
        .entry(mac_s.clone())
        .or_insert_with(|| DeviceManagers::with_aacp(aacp.clone()))
        .set_aacp(aacp.clone());

    let (command_tx, mut command_rx) = mpsc::unbounded_channel();
    *ctx.active_commands.lock().await = Some(command_tx.clone());
    let route = RouteWatch::default();
    {
        let route = route.clone();
        let aacp = aacp.clone();
        let hub = ctx.hub.clone();
        let local = ctx.local_mac.clone();
        let (mac, name) = (mac_s.clone(), name.to_string());
        let audio = audio.cloned();
        tasks.spawn(async move {
            while let Some((id, value)) = command_rx.recv().await {
                if let Err(e) = aacp.send_control_command(id, &value).await {
                    warn!("Failed to send control command {:?}: {}", id, e);
                    continue;
                }
                // Settings the AirPods reported: show the new value at once; their own echo
                // (if any) confirms it a moment later.
                if let Some(&byte) = value.first() {
                    hub.update_device(&mac, &name, |d| {
                        if let Some(v) = d.controls.get_mut(&(id as u8)) {
                            *v = byte;
                        }
                    });
                }
                if id == ControlCommandIdentifiers::OwnsConnection {
                    let owns = value.first().copied().unwrap_or(0) != 0;
                    let had = owns_audio(&hub, &mac);
                    hub.update_device(&mac, &name, |d| d.owns_audio = Some(owns));
                    if owns {
                        route.lock().claimed_at = Some(Instant::now());
                        claim_audio_route(&aacp, &local).await;
                        if had != Some(true) {
                            route_regained(audio.as_ref(), had == Some(false));
                        }
                    }
                }
            }
        });
    }

    progress.busy("subscribing to controls");
    subscribe_controls(aacp, ctx, &mac_s, name, media, audio, &route, &mut tasks).await;

    let mut stems = StemSettings::read();
    ctx.hub.update_device(&mac_s, name, |d| {
        d.mic = stems.mic_switch.then_some(MicRoute::Desk);
        d.mic_problem = None;
    });
    let mic = spawn_mic_switch(aacp, audio, ctx, &mac_s, name);
    let mut mic_requests = crate::mic::subscribe_requests();

    progress.busy("starting the playback listener");
    {
        let controller = media.lock().await;
        controller
            .start_playback_listener(aacp.clone(), command_tx.clone())
            .await;
    }
    progress.idle();

    ctx.battery.send(BatteryInput::AacpLink(mac, true));
    ctx.hub.set_link(&mac_s, LinkState::Connected);
    let _ = ctx.ui_tx.send(BluetoothUIMessage::DeviceConnected);

    let mut route_watch = tokio::time::interval(ROUTE_WATCH_INTERVAL);
    let mut health = tokio::time::interval(HEALTH_INTERVAL);
    let mut model_known = false;
    // AirPods 4 ANC ignore the stem claim sent during the handshake (seen 2026-10-28) but
    // take it once the connection has settled, so it is sent again a few seconds in.
    let connected_at = Instant::now();
    let mut claim_resent = false;
    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = health.tick() => {
                progress.busy("health check");
                if !aacp.is_connected().await {
                    break;
                }
                if !model_known {
                    model_known = refresh_model(aacp, ctx, &mac_s, name).await;
                }
                if !claim_resent && connected_at.elapsed() >= CLAIM_RESEND_AFTER {
                    claim_resent = true;
                    apply_stem_settings(aacp, &StemSettings::read()).await;
                }
                // Settings changed in the window: claim or release the stem presses now.
                let now = StemSettings::read();
                if now != stems {
                    apply_stem_settings(aacp, &now).await;
                    if !now.mic_switch {
                        mic.request(MicRequest::Desk("the mic switch was turned off"));
                    }
                    ctx.hub.update_device(&mac_s, name, |d| {
                        d.mic = now.mic_switch.then_some(d.mic.unwrap_or(MicRoute::Desk));
                        if !now.mic_switch {
                            d.mic_problem = None;
                        }
                    });
                    stems = now;
                }
            }
            _ = route_watch.tick() => {
                progress.busy("watching the audio route");
                watch_route(aacp, ctx, &mac_s, &route, &command_tx).await;
                keep_mic_allowed(aacp, ctx, &mac_s, &route, &mic).await;
            }
            event = event_rx.recv() => {
                let Some(event) = event else { break };
                progress.busy("handling an AirPods event");
                handle_event(event, mac, name, aacp, ctx, media, audio, &command_tx, &route, &stems, &mic).await;
                keep_mic_allowed(aacp, ctx, &mac_s, &route, &mic).await;
            }
            req = mic_requests.recv() => {
                // A lag report is not a request (the requests still buffered follow), and
                // the sender is a static that never closes.
                if req.is_err() {
                    continue;
                }
                if stems.mic_switch {
                    progress.busy("handling a mic request");
                    let allowed = mic_allowed(aacp, ctx, &mac_s, &route).await;
                    mic.request(MicRequest::Toggle { allowed });
                }
            }
        }
        progress.idle();
    }
    // Back to the desk mic before the control channel goes (STOP needs it).
    mic.shutdown().await;
    tasks.abort_all();
    // Wait for aborted tasks so their channels are really closed before cleanup checks them.
    while tasks.join_next().await.is_some() {}
}

/// When another device last joined each AirPods (by AirPods MAC).
static PEER_JOINED: std::sync::Mutex<Option<HashMap<String, Instant>>> =
    std::sync::Mutex::new(None);

fn note_peer_joined(mac: &str) {
    if let Ok(mut joined) = PEER_JOINED.lock() {
        joined
            .get_or_insert_with(HashMap::new)
            .insert(mac.to_ascii_uppercase(), Instant::now());
    }
}

/// Whether each AirPods' last ear report had them put away: in the case, none in an ear.
static PUT_AWAY: std::sync::Mutex<Option<HashMap<String, bool>>> = std::sync::Mutex::new(None);

fn note_ear_status(mac: &str, status: &[EarDetectionStatus]) {
    let away = status.contains(&EarDetectionStatus::InCase)
        && !status.contains(&EarDetectionStatus::InEar);
    if let Ok(mut put_away) = PUT_AWAY.lock() {
        put_away
            .get_or_insert_with(HashMap::new)
            .insert(mac.to_ascii_uppercase(), away);
    }
}

/// Whether these AirPods were put in the case when they dropped this PC.
pub fn dropped_in_case(mac: &str) -> bool {
    PUT_AWAY
        .lock()
        .ok()
        .and_then(|away| away.as_ref()?.get(&mac.to_ascii_uppercase()).copied())
        .unwrap_or(false)
}

/// Whether another device joined these AirPods just before they dropped this PC: they
/// keep a limited number of links and let go of this one to make room.
pub fn dropped_for_peer(mac: &str) -> bool {
    PEER_JOINED
        .lock()
        .ok()
        .and_then(|joined| joined.as_ref()?.get(&mac.to_ascii_uppercase()).copied())
        .is_some_and(|t| t.elapsed() < Duration::from_secs(10))
}

fn owns_audio(hub: &crate::hub::Hub, mac: &str) -> Option<bool> {
    hub.snapshot().devices.get(mac).and_then(|d| d.owns_audio)
}

/// Mirror the audio guard's health into the hub as a short human-readable status.
fn spawn_health_mirror(guard: &AudioGuardHandle, ctx: &DeviceContext, mac_s: &str, name: &str) {
    let mut rx = guard.health();
    let hub = ctx.hub.clone();
    let mac = mac_s.to_string();
    let name = name.to_string();
    tokio::spawn(async move {
        loop {
            let text = match &*rx.borrow_and_update() {
                AudioHealth::Disconnected => None,
                AudioHealth::Negotiating => Some("Connecting audio".to_string()),
                AudioHealth::Ready { codec, profile } => Some(codec.clone().unwrap_or_else(|| {
                    if profile.contains("aac") {
                        "AAC".into()
                    } else {
                        "Audio ready".into()
                    }
                })),
                AudioHealth::Recovering { .. } => Some("Restoring audio".to_string()),
                AudioHealth::Failed { reason } => Some(format!("Audio problem: {reason}")),
            };
            hub.update_device(&mac, &name, |d| d.audio = text);
            if rx.changed().await.is_err() {
                return;
            }
        }
    });
}

/// Fill the model once the AirPods have sent their INFORMATION packet. Returns true when known.
async fn refresh_model(aacp: &AACPManager, ctx: &DeviceContext, mac_s: &str, name: &str) -> bool {
    let info = {
        let state = aacp.state.lock().await;
        match state.devices.get(mac_s).and_then(|d| d.information.clone()) {
            Some(DeviceInformation::AirPods(info)) => Some(info),
            _ => None,
        }
    };
    let Some(info) = info else { return false };
    let kind = Some(ModelInfo::from_model_number(&info.model_number).kind)
        .filter(|k| *k != crate::core::models::ModelKind::Unknown);
    ctx.hub.update_device(mac_s, name, |d| {
        d.model_number = Some(info.model_number.clone()).filter(|s| !s.is_empty());
        if let Some(kind) = kind {
            d.model = kind;
        }
        if !info.name.is_empty() {
            d.name = info.name.clone();
        }
    });
    true
}

fn setting_bool(key: &str) -> bool {
    std::fs::read_to_string(get_app_settings_path())
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v.get(key).and_then(|b| b.as_bool()))
        .unwrap_or(false)
}

/// Name stored in devices.json for a MAC, if any.
pub fn saved_device_name(mac: &str) -> Option<String> {
    crate::store::load_devices()
        .get(mac)
        .map(|d| d.name.clone())
        .filter(|n| !n.is_empty())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AirPodsInformation {
    pub name: String,
    pub model_number: String,
    pub manufacturer: String,
    pub serial_number: String,
    pub version1: String,
    pub version2: String,
    pub hardware_revision: String,
    pub updater_identifier: String,
    pub left_serial_number: String,
    pub right_serial_number: String,
    pub version3: String,
    pub le_keys: AirPodsLEKeys,
}

mod controls;
mod route;

use controls::*;
use route::*;

pub use route::rebuild_output;

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_step_that_never_finishes_is_reported_as_stuck() {
        let progress = Progress::default();
        progress.busy("handshake");
        progress.idle();
        progress.busy("watching the audio route");
        let limit = Duration::from_millis(200);
        let step = tokio::time::timeout(Duration::from_secs(2), progress.watch(limit))
            .await
            .expect("stuck step reported");
        assert_eq!(step, "watching the audio route");
    }

    #[tokio::test]
    async fn an_idle_session_is_never_stuck() {
        let progress = Progress::default();
        progress.busy("health check");
        progress.idle();
        let limit = Duration::from_millis(100);
        let watch = tokio::time::timeout(Duration::from_millis(500), progress.watch(limit));
        assert!(watch.await.is_err());
    }

    fn source(mac: &str, r#type: AudioSourceType) -> AudioSource {
        AudioSource {
            mac: mac.to_string(),
            r#type,
        }
    }

    #[test]
    fn take_back_on_let_go_only_after_a_borrow_or_a_stop() {
        let route = RouteWatch::default();
        let settle = |route: &RouteWatch| {
            route.lock().lost_at = Instant::now().checked_sub(HANDOVER_GAP * 2);
        };
        // The iPhone took the AirPods to play media: its hand-over gap and its media
        // never count as letting go.
        route.lost(false);
        assert!(!route.may_reclaim());
        settle(&route);
        assert!(!route.may_reclaim());
        // It stopped playing: letting go now means the PC may have them back.
        route.lock().peer_stopped = true;
        assert!(route.may_reclaim());
        // A notification borrowed them: once the hand-over gap is over.
        route.lost(true);
        assert!(!route.may_reclaim());
        settle(&route);
        assert!(route.may_reclaim());
        // Never during a call.
        route.lock().call = true;
        assert!(!route.may_reclaim());
    }

    #[test]
    fn stream_tracker_counts_playback_and_ignores_short_sounds() {
        let mut t = StreamTracker::default();
        // Plex was already playing when tracking began: playing, but not a fresh start.
        assert_eq!(t.update(&[1]), (true, false));
        assert_eq!(t.update(&[1]), (true, false));
        // Paused (corked): nothing plays.
        assert_eq!(t.update(&[]), (false, false));
        // A notification sound for a single tick doesn't count.
        assert_eq!(t.update(&[3]), (false, false));
        assert_eq!(t.update(&[]), (false, false));
        // Plex resumes: counts on its second tick, as a start once.
        assert_eq!(t.update(&[1]), (false, false));
        assert_eq!(t.update(&[1]), (true, true));
        assert_eq!(t.update(&[1]), (true, false));
        // A second app starting while one plays is a fresh start too.
        assert_eq!(t.update(&[1, 4]), (true, false));
        assert_eq!(t.update(&[1, 4]), (true, true));
        // After a reset (the iPhone left and came back), playing streams are a baseline.
        t.reset();
        assert_eq!(t.update(&[1, 4]), (true, false));
    }

    #[test]
    fn audio_source_tells_who_has_the_route() {
        let local = "E8:65:38:93:3D:9E";
        let phone = "10:A2:D3:BF:3D:76";
        assert_eq!(
            route_owner(&source(phone, AudioSourceType::Media), local),
            Some(false)
        );
        assert_eq!(
            route_owner(&source(phone, AudioSourceType::Call), local),
            Some(false)
        );
        let ours = source("e8:65:38:93:3d:9e", AudioSourceType::Media);
        assert_eq!(route_owner(&ours, local), Some(true));
        let idle = source("00:00:00:00:00:00", AudioSourceType::Media);
        assert_eq!(route_owner(&idle, local), None);
        assert_eq!(
            route_owner(&source(phone, AudioSourceType::None), local),
            None
        );
    }
}
