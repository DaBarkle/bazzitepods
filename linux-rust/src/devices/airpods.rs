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
use crate::devices::enums::{DeviceData, DeviceInformation};
use crate::hub::{Hub, LinkState};
use crate::media_controller::MediaController;
use crate::mic::switch::{MicReport, MicRequest, MicSwitchHandle, SwitchConfig};
use crate::mic::{AacpMicLink, MicRoute, PulseMicHost};
use crate::ui::messages::BluetoothUIMessage;
use crate::utils::{get_app_settings_path, get_devices_path};
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

        session(mac, &name, &aacp, &ctx, &media, audio.as_ref(), &cancel).await;

        aacp.shutdown().await;
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
    if was_worn {
        controller.pause_all_media().await;
    }
    controller.stop_playback_listener().await;
    drop(controller);

    ctx.hub.update_device(&mac_s, &name, |d| {
        d.link = LinkState::Disconnected;
        d.in_case = dropped_in_case(&mac_s);
        d.in_ear = (None, None);
        d.audio = None;
        d.last_seen = Some(std::time::SystemTime::now());
    });
    let _ = ctx
        .ui_tx
        .send(BluetoothUIMessage::DeviceDisconnected(mac_s.clone()));
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
async fn session(
    mac: Address,
    name: &str,
    aacp: &AACPManager,
    ctx: &DeviceContext,
    media: &Arc<Mutex<MediaController>>,
    audio: Option<&AudioGuardHandle>,
    cancel: &CancellationToken,
) {
    let mac_s = mac.to_string();
    let mut tasks = JoinSet::new();

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

    subscribe_controls(aacp, ctx, &mac_s, name, media, audio, &route, &mut tasks).await;

    let mut stems = StemSettings::read();
    ctx.hub.update_device(&mac_s, name, |d| {
        d.mic = stems.mic_switch.then_some(MicRoute::Desk);
        d.mic_problem = None;
    });
    let mic = spawn_mic_switch(aacp, audio, ctx, &mac_s, name);
    let mut mic_requests = crate::mic::subscribe_requests();

    {
        let controller = media.lock().await;
        controller
            .start_playback_listener(aacp.clone(), command_tx.clone())
            .await;
    }

    ctx.battery.send(BatteryInput::AacpLink(mac, true));
    ctx.hub.set_link(&mac_s, LinkState::Connected);
    let _ = ctx
        .ui_tx
        .send(BluetoothUIMessage::DeviceConnected(mac_s.clone()));

    let mut route_watch = tokio::time::interval(ROUTE_WATCH_INTERVAL);
    let mut health = tokio::time::interval(HEALTH_INTERVAL);
    let mut model_known = false;
    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = health.tick() => {
                if !aacp.is_connected().await {
                    break;
                }
                if !model_known {
                    model_known = refresh_model(aacp, ctx, &mac_s, name).await;
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
                watch_route(aacp, ctx, &mac_s, &route, &command_tx).await;
                keep_mic_allowed(aacp, ctx, &mac_s, &route, &mic).await;
            }
            event = event_rx.recv() => {
                let Some(event) = event else { break };
                handle_event(event, mac, name, aacp, ctx, media, audio, &command_tx, &route, &stems, &mic).await;
                keep_mic_allowed(aacp, ctx, &mac_s, &route, &mic).await;
            }
            req = mic_requests.recv() => {
                if matches!(req, Err(tokio::sync::broadcast::error::RecvError::Closed)) {
                    continue;
                }
                if stems.mic_switch {
                    let allowed = mic_allowed(aacp, ctx, &mac_s, &route).await;
                    mic.request(MicRequest::Toggle { allowed });
                }
            }
        }
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

/// The audio came back from another device. While it was away the AirPods usually broke
/// the A2DP transport to this PC, so have the audio guard check at once and repair it.
/// The repair rebuilds the output, which makes players pause; resume the ones that were
/// playing when the audio came back, so one press of play is enough.
fn route_regained(audio: Option<&AudioGuardHandle>, from_other_device: bool) {
    info!("Audio route is back on this PC; re-checking the audio setup");
    let Some(audio) = audio.cloned() else {
        return;
    };
    // While another device had the audio, the AirPods broke this PC's transport and the
    // output node went into an error state that never stalls, so rebuild it now.
    if from_other_device {
        audio.repair();
    } else {
        audio.route_back();
    }
    resume_after_rebuild(audio);
}

/// The AirPods mic stream stopped: rebuild the music output once (the right pod goes
/// silent otherwise) and resume the players that the rebuild pauses.
pub fn rebuild_output(audio: Option<&AudioGuardHandle>) {
    let Some(audio) = audio.cloned() else {
        return;
    };
    audio.rebuild();
    resume_after_rebuild(audio);
}

/// Players that are playing now get pressed play again once the guard's rebuild is done.
fn resume_after_rebuild(audio: AudioGuardHandle) {
    tokio::spawn(async move {
        let playing = MediaController::playing_players().await;
        if playing.is_empty() {
            return;
        }
        let mut health = audio.health();
        let deadline = sleep(Duration::from_secs(15));
        tokio::pin!(deadline);
        let mut repaired = false;
        loop {
            tokio::select! {
                _ = &mut deadline => return,
                changed = health.changed() => {
                    if changed.is_err() {
                        return;
                    }
                    match *health.borrow() {
                        AudioHealth::Recovering { .. } => repaired = true,
                        AudioHealth::Ready { .. } if repaired => break,
                        _ => {}
                    }
                }
            }
        }
        // Let the new output settle before pressing play.
        sleep(Duration::from_millis(800)).await;
        MediaController::resume_if_paused(playing).await;
    });
}

/// Tell every other device on the AirPods that this PC is taking the audio route, so
/// they hand it over (the same exchange an iPhone does when you pick it in Control Centre).
/// Runs after `OwnsConnection = 1` was sent, whether from playback starting or from the
/// "Use on this PC" button.
async fn claim_audio_route(aacp: &AACPManager, local: &str) {
    let mut state = aacp.state.lock().await;
    // Whatever the other device said it was playing is over once we take the AirPods.
    state.peer_streaming = false;
    let others: Vec<String> = state
        .connected_devices
        .iter()
        .filter(|d| !d.mac.eq_ignore_ascii_case(local))
        .map(|d| d.mac.clone())
        .collect();
    // Sending takes the same lock.
    drop(state);
    for peer in others {
        if let Err(e) = aacp.send_media_information(local, &peer, true).await {
            warn!("Failed to send media information to {peer}: {e}");
        }
        if let Err(e) = aacp.send_smart_routing_show_ui(&peer).await {
            warn!("Failed to send smart routing UI to {peer}: {e}");
        }
        if let Err(e) = aacp.send_hijack_request(&peer).await {
            warn!("Failed to send hijack request to {peer}: {e}");
        }
    }
}

async fn handshake(aacp: &AACPManager) -> bluer::Result<()> {
    aacp.send_handshake().await?;
    sleep(Duration::from_millis(300)).await;
    aacp.send_set_feature_flags_packet().await?;
    sleep(Duration::from_millis(300)).await;
    aacp.send_notification_request().await?;
    if let Err(e) = aacp.send_some_packet().await {
        debug!("Optional init packet failed: {}", e);
    }
    if let Err(e) = aacp
        .send_proximity_keys_request(vec![ProximityKeyType::Irk, ProximityKeyType::EncKey])
        .await
    {
        warn!("Failed to request proximity keys: {}", e);
    }
    apply_stem_settings(aacp, &StemSettings::read()).await;
    Ok(())
}

/// The settings that decide which stem presses this PC handles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StemSettings {
    /// Double/triple press: next/previous track.
    track_control: bool,
    /// Left hold: swap mics; right hold: noise control.
    mic_switch: bool,
}

impl StemSettings {
    fn read() -> Self {
        Self {
            track_control: setting_bool("stem_control"),
            mic_switch: setting_bool("airpods_mic_switch"),
        }
    }
}

/// Claims the presses the settings ask for, and gives the rest back to the AirPods. The
/// AirPods keep the claim across reconnects, so it is always sent, 0 included.
async fn apply_stem_settings(aacp: &AACPManager, s: &StemSettings) {
    let mask = crate::mic::gestures::stem_claims(s.track_control, s.mic_switch);
    if let Err(e) = aacp
        .send_control_command(ControlCommandIdentifiers::StemConfig, &[mask])
        .await
    {
        warn!("Failed to set stem press handling: {}", e);
    }
}

fn spawn_mic_switch(
    aacp: &AACPManager,
    audio: Option<&AudioGuardHandle>,
    ctx: &DeviceContext,
    mac_s: &str,
    name: &str,
) -> MicSwitchHandle {
    let (hub, mac, name) = (ctx.hub.clone(), mac_s.to_string(), name.to_string());
    let report = move |r: MicReport| {
        hub.update_device(&mac, &name, |d| {
            if d.mic.is_some() || r.route == MicRoute::AirPods {
                d.mic = Some(r.route);
            }
            if r.problem.is_some() {
                d.mic_failures = d.mic_failures.wrapping_add(1);
            }
            d.mic_problem = r.problem.clone();
        });
    };
    crate::mic::switch::spawn(
        PulseMicHost,
        AacpMicLink {
            aacp: aacp.clone(),
            audio: audio.cloned(),
        },
        SwitchConfig::default(),
        Arc::new(report),
    )
}

/// Whether the AirPods mic may be used right now: this PC has the AirPods, at least one
/// is worn, and no call is on them.
async fn mic_allowed(
    aacp: &AACPManager,
    ctx: &DeviceContext,
    mac_s: &str,
    route: &RouteWatch,
) -> Result<(), &'static str> {
    if owns_audio(&ctx.hub, mac_s) == Some(false) {
        return Err("The AirPods are in use on another device");
    }
    if route.lock().call {
        return Err("A call is on the AirPods");
    }
    let ears = aacp.state.lock().await.ear_detection_status.clone();
    if !ears.is_empty() && !ears.contains(&EarDetectionStatus::InEar) {
        return Err("The AirPods aren't in your ears");
    }
    Ok(())
}

/// Back to the desk mic as soon as the AirPods mic stops being usable.
async fn keep_mic_allowed(
    aacp: &AACPManager,
    ctx: &DeviceContext,
    mac_s: &str,
    route: &RouteWatch,
    mic: &MicSwitchHandle,
) {
    let on_airpods = ctx
        .hub
        .snapshot()
        .devices
        .get(mac_s)
        .is_some_and(|d| d.mic == Some(MicRoute::AirPods));
    if on_airpods && let Err(why) = mic_allowed(aacp, ctx, mac_s, route).await {
        mic.request(MicRequest::Desk(why));
    }
}

/// Right stem hold with the mic switch on: the AirPods no longer change noise control
/// themselves, so step to the next of the modes picked for press and hold.
async fn cycle_listening_mode(aacp: &AACPManager, command_tx: &CommandSender) {
    let current = aacp
        .control_value(ControlCommandIdentifiers::ListeningMode)
        .await
        .and_then(|v| v.first().copied());
    let enabled = aacp
        .control_value(ControlCommandIdentifiers::ListeningModeConfigs)
        .await
        .and_then(|v| v.first().copied());
    let next = crate::mic::gestures::next_listening_mode(current, enabled);
    info!("Right stem hold: listening mode {current:?} -> {next:#04x}");
    let _ = command_tx.send((ControlCommandIdentifiers::ListeningMode, vec![next]));
}

/// Mirror control-command state (listening mode, etc.) into the hub, and react to ownership changes.
async fn subscribe_controls(
    aacp: &AACPManager,
    ctx: &DeviceContext,
    mac_s: &str,
    name: &str,
    media: &Arc<Mutex<MediaController>>,
    audio: Option<&AudioGuardHandle>,
    route: &RouteWatch,
    tasks: &mut JoinSet<()>,
) {
    let watch = [
        ControlCommandIdentifiers::ListeningMode,
        ControlCommandIdentifiers::AllowOffOption,
        ControlCommandIdentifiers::ConversationDetectConfig,
    ];
    for id in watch {
        let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
        aacp.subscribe_to_control_command(id, tx).await;
        let hub = ctx.hub.clone();
        let mac = mac_s.to_string();
        let name = name.to_string();
        tasks.spawn(async move {
            while let Some(value) = rx.recv().await {
                let Some(&byte) = value.first() else { continue };
                hub.update_device(&mac, &name, |d| match id {
                    ControlCommandIdentifiers::ListeningMode => d.listening_mode = Some(byte),
                    ControlCommandIdentifiers::AllowOffOption => {
                        d.allow_off_mode = Some(byte == 0x01)
                    }
                    ControlCommandIdentifiers::ConversationDetectConfig => {
                        d.conversation_awareness = Some(byte == 0x01)
                    }
                    _ => {}
                });
            }
        });
    }

    // Another Apple device took the AirPods. Pause what we were playing, but never touch the
    // audio profile: setting it "off" is what used to leave audio dead after reconnecting.
    let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
    aacp.subscribe_to_control_command(ControlCommandIdentifiers::OwnsConnection, tx)
        .await;
    let media = media.clone();
    let hub = ctx.hub.clone();
    let audio = audio.cloned();
    let route = route.clone();
    let aacp = aacp.clone();
    let (mac, name) = (mac_s.to_string(), name.to_string());
    tasks.spawn(async move {
        while let Some(value) = rx.recv().await {
            let owns = value.first().copied().unwrap_or(0) != 0;
            let had = owns_audio(&hub, &mac);
            hub.update_device(&mac, &name, |d| d.owns_audio = Some(owns));
            if had == Some(true) && !owns {
                info!("Another device took the AirPods; pausing local media");
                // Picked in the iPhone's Control Center, this is the only sign of the
                // switch, so record it like any other: the PC takes the AirPods back when
                // the iPhone lets go, and leaves the dead output alone until then.
                let streaming = aacp.state.lock().await.peer_streaming;
                route.lost(!streaming);
                if let Some(audio) = &audio {
                    audio.route_away();
                }
                media.lock().await.pause_all_media().await;
            } else if had == Some(false) && owns {
                route_regained(audio.as_ref(), true);
            }
        }
    });
}

#[allow(clippy::too_many_arguments)]
async fn handle_event(
    event: AACPEvent,
    mac: Address,
    name: &str,
    aacp: &AACPManager,
    ctx: &DeviceContext,
    media: &Arc<Mutex<MediaController>>,
    audio: Option<&AudioGuardHandle>,
    command_tx: &CommandSender,
    route: &RouteWatch,
    stems: &StemSettings,
    mic: &MicSwitchHandle,
) {
    let mac_s = mac.to_string();
    match &event {
        AACPEvent::BatteryInfo(info) => {
            ctx.battery.send(BatteryInput::Aacp(mac, info.clone()));
        }
        AACPEvent::EarDetection(old, new) => {
            note_ear_status(&mac_s, new);
            // A pod just went into the case. The first battery report then often says
            // "case unknown" (255) and the real level may never follow, so ask the AirPods
            // to resend their state a little later.
            let in_case = |v: &Vec<EarDetectionStatus>| v.contains(&EarDetectionStatus::InCase);
            if in_case(new) && !in_case(old) {
                let aacp = aacp.clone();
                tokio::spawn(async move {
                    for delay in [2, 4] {
                        sleep(Duration::from_secs(delay)).await;
                        if let Err(e) = aacp.send_notification_request().await {
                            debug!("case battery refresh failed: {e}");
                            return;
                        }
                    }
                });
            }
            let in_ear = |i: usize| new.get(i).map(|s| *s == EarDetectionStatus::InEar);
            // AACP reports primary/secondary; the UI only needs "is anything in an ear", and
            // left/right placement is refined by BLE. Store as (primary, secondary).
            ctx.hub
                .update_device(&mac_s, name, |d| d.in_ear = (in_ear(0), in_ear(1)));
            if let Some(audio) = audio {
                audio.wearing_changed(new.contains(&EarDetectionStatus::InEar));
            }
            // The AirPods repeat their status; only a change means something moved.
            if old != new {
                media
                    .lock()
                    .await
                    .handle_ear_detection(old.clone(), new.clone())
                    .await;
            }
        }
        AACPEvent::ControlCommand(status) => {
            if let Some(&value) = status.value.first() {
                let id = status.identifier as u8;
                ctx.hub.update_device(&mac_s, name, |d| {
                    d.controls.insert(id, value);
                });
            }
        }
        AACPEvent::ConversationalAwareness(status) => {
            media
                .lock()
                .await
                .handle_conversational_awareness(*status)
                .await;
        }
        AACPEvent::ConnectedDevices(old_devices, new_devices) => {
            let local = ctx.local_mac.clone();
            let others = new_devices
                .iter()
                .filter(|n| !n.mac.eq_ignore_ascii_case(&local))
                .count();
            ctx.hub
                .update_device(&mac_s, name, |d| d.other_devices = others);
            for device in new_devices
                .iter()
                .filter(|n| n.mac != local && old_devices.iter().all(|o| o.mac != n.mac))
            {
                info!("New device on AirPods: {}", device.mac);
                note_peer_joined(&mac_s);
                let aacp = aacp.clone();
                let local = local.clone();
                let peer = device.mac.clone();
                tokio::spawn(async move {
                    if let Err(e) = aacp.send_media_information_new_device(&local, &peer).await {
                        debug!("send_media_information_new_device failed: {}", e);
                    }
                    if let Err(e) = aacp.send_add_tipi_device(&local, &peer).await {
                        debug!("send_add_tipi_device failed: {}", e);
                    }
                    // Tell it we are playing, so it doesn't take the AirPods for a ding.
                    let ours = aacp
                        .state
                        .lock()
                        .await
                        .audio_source
                        .as_ref()
                        .and_then(|s| route_owner(s, &local));
                    if ours == Some(true)
                        && !MediaController::playing_players().await.is_empty()
                        && let Err(e) = aacp.send_media_information(&local, &peer, true).await
                    {
                        debug!("send_media_information failed: {}", e);
                    }
                });
            }
        }
        AACPEvent::AudioSource(source) => {
            // The AirPods say whose audio they are playing. An iPhone that takes them often
            // never sends OwnsConnection = 0, so this is the reliable signal.
            let had = owns_audio(&ctx.hub, &mac_s);
            match route_owner(source, &ctx.local_mac) {
                // Nobody is playing. If another device let go while something here is still
                // playing, take the AirPods back now instead of waiting for them to drift
                // back. Only after a notification borrowed them or the other device stopped
                // its media: the gap during a hand-over to an iPhone that starts playing
                // looks the same, and taking them back then makes the two fight.
                None if had == Some(false) && route.may_reclaim() => {
                    let worn = aacp
                        .state
                        .lock()
                        .await
                        .ear_detection_status
                        .contains(&EarDetectionStatus::InEar);
                    let playing = route.lock().local_playing
                        || !MediaController::playing_players().await.is_empty();
                    if worn && playing {
                        info!(
                            "The other device let go and media is playing here; taking the AirPods back"
                        );
                        let _ = command_tx
                            .send((ControlCommandIdentifiers::OwnsConnection, vec![0x01]));
                    }
                }
                None => {}
                Some(true) => {
                    ctx.hub
                        .update_device(&mac_s, name, |d| d.owns_audio = Some(true));
                    if had == Some(false) {
                        route_regained(audio, true);
                    }
                }
                // We just took the AirPods; this report is from before the switch.
                Some(false) if route.just_claimed() => {
                    debug!(
                        "Ignoring Audio Source {} right after taking the AirPods",
                        source.mac
                    );
                }
                Some(false) => {
                    let peer = aacp
                        .state
                        .lock()
                        .await
                        .peer_names
                        .get(&source.mac.to_ascii_uppercase())
                        .cloned();
                    ctx.hub.update_device(&mac_s, name, |d| {
                        d.owns_audio = Some(false);
                        d.elsewhere_name = peer;
                    });
                    if had != Some(false) {
                        route_lost(source, aacp, ctx, &mac_s, media, route, audio).await;
                    }
                }
            }
            route.lock().call = source.r#type == AudioSourceType::Call
                && route_owner(source, &ctx.local_mac) == Some(false);
        }
        AACPEvent::PeerStreaming(streaming) => {
            {
                let mut state = route.lock();
                state.peer_stopped = !streaming;
                // Playing media is not a borrow, whatever the switch first looked like.
                if *streaming {
                    state.lost_to_borrow = false;
                }
            }
            if *streaming && owns_audio(&ctx.hub, &mac_s) == Some(false) {
                info!("The other device started playing media; pausing local media");
                if let Some(audio) = audio {
                    audio.route_away();
                }
                media.lock().await.pause_all_media().await;
            }
        }
        AACPEvent::OwnershipToFalseRequest if route.just_claimed() => {
            debug!("Ignoring a release request right after taking the AirPods");
        }
        AACPEvent::OwnershipToFalseRequest => {
            info!("AirPods asked us to release ownership; pausing local media");
            route.lost(false);
            if let Some(audio) = audio {
                audio.route_away();
            }
            let _ = command_tx.send((ControlCommandIdentifiers::OwnsConnection, vec![0x00]));
            media.lock().await.pause_all_media().await;
        }
        AACPEvent::StemPress(press, bud) => {
            use crate::bluetooth::aacp::{StemPressBudType, StemPressType};
            match press {
                StemPressType::DoublePress | StemPressType::TriplePress if stems.track_control => {
                    let controller = media.lock().await;
                    if *press == StemPressType::DoublePress {
                        controller.next_track().await;
                    } else {
                        controller.previous_track().await;
                    }
                }
                StemPressType::LongPress if stems.mic_switch => match bud {
                    StemPressBudType::Left => {
                        let allowed = mic_allowed(aacp, ctx, &mac_s, route).await;
                        mic.request(MicRequest::Toggle { allowed });
                    }
                    StemPressBudType::Right => {
                        let has_anc = ctx
                            .hub
                            .snapshot()
                            .devices
                            .get(&mac_s)
                            .is_none_or(|d| d.model.info().has_anc);
                        if has_anc {
                            cycle_listening_mode(aacp, command_tx).await;
                        }
                    }
                },
                _ => {}
            }
        }
        _ => {}
    }
    // The legacy window still consumes raw events; it ignores the ones it doesn't know.
    let _ = ctx
        .ui_tx
        .send(BluetoothUIMessage::AACPUIEvent(mac_s, event));
}

/// Another device has the AirPods now. When it is playing media (YouTube on the iPhone),
/// pause here right away, like an iPhone does, so the next press of play is a fresh start
/// that takes them straight back. A notification only borrows them for a few seconds, so
/// then keep playing and pause only if they stay away.
async fn route_lost(
    source: &AudioSource,
    aacp: &AACPManager,
    ctx: &DeviceContext,
    mac_s: &str,
    media: &Arc<Mutex<MediaController>>,
    route: &RouteWatch,
    audio: Option<&AudioGuardHandle>,
) {
    let borrowed =
        source.r#type != AudioSourceType::Call && !aacp.state.lock().await.peer_streaming;
    route.lost(borrowed);
    if let Some(audio) = audio {
        audio.route_away();
    }
    if source.r#type == AudioSourceType::Call {
        info!(
            "{} has a call on the AirPods; pausing local media",
            source.mac
        );
        media.lock().await.pause_all_media().await;
        return;
    }
    if aacp.state.lock().await.peer_streaming {
        info!(
            "{} is playing media on the AirPods; pausing local media",
            source.mac
        );
        media.lock().await.pause_all_media().await;
        return;
    }
    info!(
        "{} borrowed the AirPods; keeping local media playing for now",
        source.mac
    );
    route.lock().borrowed_until = Some(Instant::now() + BORROW_GRACE);
    let (hub, mac, media) = (ctx.hub.clone(), mac_s.to_string(), media.clone());
    tokio::spawn(async move {
        sleep(BORROW_GRACE).await;
        if owns_audio(&hub, &mac) == Some(false) {
            info!("The AirPods stayed on the other device; pausing local media");
            media.lock().await.pause_all_media().await;
        }
    });
}

/// What the route watcher remembers between ticks.
#[derive(Default)]
struct RouteState {
    /// Until when another device is only borrowing the AirPods (a notification), if it is.
    borrowed_until: Option<Instant>,
    /// The streaming state last announced to the other devices.
    announced: Option<bool>,
    /// Audio streams seen playing, to catch players without MPRIS (the Plex app).
    streams: StreamTracker,
    /// Whether something plays here, as of the last tick.
    local_playing: bool,
    /// When this PC last took the AirPods.
    claimed_at: Option<Instant>,
    /// Another device has a phone call on the AirPods: never take them back mid-call.
    call: bool,
    /// When this PC last lost the AirPods, and whether only to a notification.
    lost_at: Option<Instant>,
    lost_to_borrow: bool,
    /// The other device said its media stopped.
    peer_stopped: bool,
}

/// Follows playing audio streams between ticks. A stream counts once it has played for
/// two ticks in a row, so a notification sound doesn't. Streams already playing when
/// tracking starts are background noise (idle voice chat and the like) until they stop.
#[derive(Default)]
struct StreamTracker {
    started: bool,
    /// Stream index -> ticks seen playing, capped at 2.
    seen: HashMap<u32, u8>,
}

impl StreamTracker {
    /// Feeds the streams playing now. Returns (a stream is playing, one just started).
    /// Streams already playing when tracking begins count as playing, but never as just
    /// started: media that was playing before the iPhone joined still brings the AirPods
    /// back when the iPhone stops, without counting as a fresh press of play.
    fn update(&mut self, playing: &[u32]) -> (bool, bool) {
        let baseline = !std::mem::replace(&mut self.started, true);
        let mut seen = HashMap::with_capacity(playing.len());
        let (mut active, mut started) = (false, false);
        for &index in playing {
            let count = match self.seen.get(&index) {
                _ if baseline => 2,
                Some(&count) => (count + 1).min(2),
                None => 1,
            };
            if count == 2 {
                active = true;
                started |= self.seen.get(&index) == Some(&1);
            }
            seen.insert(index, count);
        }
        self.seen = seen;
        (active, started)
    }

    fn reset(&mut self) {
        *self = Self::default();
    }
}

#[derive(Clone, Default)]
struct RouteWatch(Arc<std::sync::Mutex<RouteState>>);

impl RouteWatch {
    fn lock(&self) -> std::sync::MutexGuard<'_, RouteState> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Records that another device took the AirPods; `borrowed` when it plays no media
    /// (a notification).
    fn lost(&self, borrowed: bool) {
        let mut state = self.lock();
        state.lost_at = Some(Instant::now());
        state.lost_to_borrow = borrowed;
        state.peer_stopped = false;
    }

    /// Whether "the other device let go" may be taken as a cue to take the AirPods back:
    /// it only borrowed them or has stopped its media, and the hand-over gap is over.
    fn may_reclaim(&self) -> bool {
        let state = self.lock();
        let settled = state.lost_at.is_none_or(|t| t.elapsed() >= HANDOVER_GAP);
        settled && !state.call && (state.lost_to_borrow || state.peer_stopped)
    }

    /// True for a moment after this PC took the AirPods, while reports from before the
    /// switch may still arrive.
    fn just_claimed(&self) -> bool {
        self.lock()
            .claimed_at
            .is_some_and(|t| t.elapsed() < CLAIM_GRACE)
    }
}

/// How long after taking the AirPods to ignore reports that say another device has them.
const CLAIM_GRACE: Duration = Duration::from_secs(3);

/// A hand-over to another device can pass through "nobody" for a moment; ignore that.
const HANDOVER_GAP: Duration = Duration::from_secs(2);

/// How often to compare local playback with who has the AirPods.
const ROUTE_WATCH_INTERVAL: Duration = Duration::from_millis(500);

/// Runs only while another device shares the AirPods. It reads local playback directly
/// instead of relying on catching each play press, which a quick pause/play can hide.
/// - This PC has the AirPods: tell the other devices whenever playback starts or stops, so
///   an iPhone doesn't take them for a notification while we play.
/// - Another device has them and something here plays: the user pressed play here (we
///   paused when it took over), so take them back. While a notification only borrows
///   them, wait for it to let go instead.
async fn watch_route(
    aacp: &AACPManager,
    ctx: &DeviceContext,
    mac_s: &str,
    route: &RouteWatch,
    command_tx: &CommandSender,
) {
    let peers: Vec<String> = aacp
        .state
        .lock()
        .await
        .connected_devices
        .iter()
        .filter(|d| !d.mac.eq_ignore_ascii_case(&ctx.local_mac))
        .map(|d| d.mac.clone())
        .collect();
    if peers.is_empty() {
        let mut state = route.lock();
        state.announced = None;
        state.streams.reset();
        return;
    }
    let mpris = !MediaController::playing_players().await.is_empty();
    let streams = tokio::task::spawn_blocking(|| {
        crate::audio::pulse::snapshot_sync(crate::audio::pulse::DEFAULT_DEADLINE)
            .map(|s| s.playing_streams())
            .unwrap_or_default()
    })
    .await
    .unwrap_or_default();
    let (stream_playing, stream_started) = route.lock().streams.update(&streams);
    let playing = mpris || stream_playing;
    route.lock().local_playing = playing;
    match owns_audio(&ctx.hub, mac_s) {
        Some(true) => {
            if route.lock().announced == Some(playing) {
                return;
            }
            route.lock().announced = Some(playing);
            debug!("Telling the other devices this PC is streaming: {playing}");
            for peer in peers {
                if let Err(e) = aacp
                    .send_media_information(&ctx.local_mac, &peer, playing)
                    .await
                {
                    debug!("Could not tell {peer} the streaming state: {e}");
                }
            }
        }
        // MPRIS players were paused when the other device took over, so one playing now
        // was started here. A player without MPRIS can't be paused, so only a stream
        // that just started counts for those.
        // Not right after pausing for the other device: that playback is the tail end of
        // what was paused, not a fresh press of play.
        Some(false)
            if (mpris || stream_started) && !crate::media_controller::auto_paused_recently() =>
        {
            route.lock().announced = None;
            let borrowed = route
                .lock()
                .borrowed_until
                .is_some_and(|until| Instant::now() < until);
            let worn = aacp
                .state
                .lock()
                .await
                .ear_detection_status
                .contains(&EarDetectionStatus::InEar);
            let call = route.lock().call;
            if worn && !borrowed && !call {
                info!(
                    "Media is playing here while another device has the AirPods; taking them back"
                );
                let _ = command_tx.send((ControlCommandIdentifiers::OwnsConnection, vec![0x01]));
            }
        }
        _ => route.lock().announced = None,
    }
}

/// How long a device may borrow the AirPods without media (a notification) before local
/// playback is paused.
const BORROW_GRACE: Duration = Duration::from_secs(6);

/// Who has the audio route according to an Audio Source report: `Some(true)` for this PC,
/// `Some(false)` for another device, `None` when nothing is playing.
fn route_owner(source: &AudioSource, local_mac: &str) -> Option<bool> {
    if source.r#type == AudioSourceType::None || source.mac == "00:00:00:00:00:00" {
        return None;
    }
    Some(source.mac.eq_ignore_ascii_case(local_mac))
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
    let json = std::fs::read_to_string(get_devices_path()).ok()?;
    let devices: HashMap<String, DeviceData> = serde_json::from_str(&json).ok()?;
    devices
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

#[cfg(test)]
mod tests {
    use super::*;

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
