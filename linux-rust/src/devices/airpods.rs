//! Per-AirPods device task.
//!
//! The supervisor in main.rs spawns `run` when BlueZ reports the AirPods connected and cancels it
//! when they disconnect. Inside, the AACP control channel is (re)opened with backoff for as long as
//! the Bluetooth link is up, so walking out of range and back never needs user action.

use crate::battery_service::{BatteryInput, BatteryService};
use crate::bluetooth::aacp::{
    AACPEvent, AACPManager, AirPodsLEKeys, ControlCommandIdentifiers, EarDetectionStatus,
    ProximityKeyType,
};
use crate::bluetooth::managers::DeviceManagers;
use crate::core::models;
use crate::devices::enums::{DeviceData, DeviceInformation};
use crate::hub::{Hub, LinkState};
use crate::media_controller::MediaController;
use crate::ui::messages::BluetoothUIMessage;
use crate::utils::{get_app_settings_path, get_devices_path};
use bluer::Address;
use log::{debug, info, warn};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock, mpsc};
use tokio::task::JoinSet;
use tokio::time::{Duration, sleep};
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
        d.last_seen = Some(std::time::SystemTime::now());
    });

    let media = Arc::new(Mutex::new(MediaController::new(
        mac_s.clone(),
        ctx.local_mac.clone(),
    )));

    let mut attempt: u32 = 0;
    loop {
        if cancel.is_cancelled() {
            break;
        }
        let aacp = connect_aacp(mac).await;
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

        session(mac, &name, &aacp, &ctx, &media, &cancel).await;

        aacp.shutdown().await;
        ctx.battery.send(BatteryInput::AacpLink(mac, false));
        ctx.device_managers.write().await.remove(&mac_s);
        *ctx.active_commands.lock().await = None;
        if cancel.is_cancelled() {
            break;
        }
        info!("Control channel to {} dropped while link is up; reconnecting", mac_s);
        ctx.hub.set_link(&mac_s, LinkState::Reconnecting { attempt: 1 });
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = sleep(Duration::from_secs(1)) => {}
        }
    }

    ctx.hub.update_device(&mac_s, &name, |d| {
        d.link = LinkState::Disconnected;
        d.in_ear = (None, None);
        d.audio = None;
        d.last_seen = Some(std::time::SystemTime::now());
    });
    let _ = ctx
        .ui_tx
        .send(BluetoothUIMessage::DeviceDisconnected(mac_s.clone()));
    info!("Device task finished for {}", mac_s);
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
    {
        let aacp = aacp.clone();
        tasks.spawn(async move {
            while let Some((id, value)) = command_rx.recv().await {
                if let Err(e) = aacp.send_control_command(id, &value).await {
                    warn!("Failed to send control command {:?}: {}", id, e);
                }
            }
        });
    }

    subscribe_controls(aacp, ctx, &mac_s, name, media, &mut tasks).await;

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

    let stem_control = setting_bool("stem_control");
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
            }
            event = event_rx.recv() => {
                let Some(event) = event else { break };
                handle_event(event, mac, name, aacp, ctx, media, &command_tx, stem_control).await;
            }
        }
    }
    tasks.abort_all();
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
    if setting_bool("stem_control") {
        // StemConfig bitmask: single=0x01, double=0x02, triple=0x04, long=0x08.
        if let Err(e) = aacp
            .send_control_command(ControlCommandIdentifiers::StemConfig, &[0x06])
            .await
        {
            warn!("Failed to enable stem press detection: {}", e);
        }
    }
    Ok(())
}

/// Mirror control-command state (listening mode, etc.) into the hub, and react to ownership changes.
async fn subscribe_controls(
    aacp: &AACPManager,
    ctx: &DeviceContext,
    mac_s: &str,
    name: &str,
    media: &Arc<Mutex<MediaController>>,
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
                    ControlCommandIdentifiers::AllowOffOption => d.allow_off_mode = Some(byte == 0x01),
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
    tasks.spawn(async move {
        let mut owned = false;
        while let Some(value) = rx.recv().await {
            let owns = value.first().copied().unwrap_or(0) != 0;
            if owned && !owns {
                info!("Another device took the AirPods; pausing local media");
                media.lock().await.pause_all_media().await;
            }
            owned = owns;
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
    command_tx: &CommandSender,
    stem_control: bool,
) {
    let mac_s = mac.to_string();
    match &event {
        AACPEvent::BatteryInfo(info) => {
            ctx.battery.send(BatteryInput::Aacp(mac, info.clone()));
        }
        AACPEvent::EarDetection(old, new) => {
            let in_ear = |i: usize| new.get(i).map(|s| *s == EarDetectionStatus::InEar);
            // AACP reports primary/secondary; the UI only needs "is anything in an ear", and
            // left/right placement is refined by BLE. Store as (primary, secondary).
            ctx.hub
                .update_device(&mac_s, name, |d| d.in_ear = (in_ear(0), in_ear(1)));
            media
                .lock()
                .await
                .handle_ear_detection(old.clone(), new.clone())
                .await;
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
            for device in new_devices
                .iter()
                .filter(|n| n.mac != local && old_devices.iter().all(|o| o.mac != n.mac))
            {
                info!("New device on AirPods: {}", device.mac);
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
                });
            }
        }
        AACPEvent::OwnershipToFalseRequest => {
            info!("AirPods asked us to release ownership; pausing local media");
            let _ = command_tx.send((ControlCommandIdentifiers::OwnsConnection, vec![0x00]));
            media.lock().await.pause_all_media().await;
        }
        AACPEvent::StemPress(press, _bud) => {
            use crate::bluetooth::aacp::StemPressType;
            if stem_control {
                let controller = media.lock().await;
                match press {
                    StemPressType::DoublePress => controller.next_track().await,
                    StemPressType::TriplePress => controller.previous_track().await,
                    _ => {}
                }
            }
        }
        _ => {}
    }
    // The legacy window still consumes raw events; it ignores the ones it doesn't know.
    let _ = ctx
        .ui_tx
        .send(BluetoothUIMessage::AACPUIEvent(mac_s, event));
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
    let kind = models::from_model_number(&info.model_number).map(|m| m.kind);
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
    devices.get(mac).map(|d| d.name.clone()).filter(|n| !n.is_empty())
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
