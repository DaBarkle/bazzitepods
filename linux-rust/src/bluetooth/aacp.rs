use crate::devices::airpods::AirPodsInformation;
use crate::devices::enums::{DeviceData, DeviceInformation, DeviceType};
use crate::utils::get_devices_path;
use bluer::{
    Address, AddressType, Error, Result,
    l2cap::{SeqPacket, Socket, SocketAddr},
};
use log::{debug, error, info};
use serde::{Deserialize, Serialize};
use serde_json;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, mpsc};
use tokio::task::JoinSet;
use tokio::time::{Instant, sleep};

const PSM: u16 = 0x1001;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const POLL_INTERVAL: Duration = Duration::from_millis(200);
const HEADER_BYTES: [u8; 4] = [0x04, 0x00, 0x04, 0x00];

pub mod opcodes {
    pub const SET_FEATURE_FLAGS: u8 = 0x4D;
    pub const REQUEST_NOTIFICATIONS: u8 = 0x0F;
    pub const BATTERY_INFO: u8 = 0x04;
    pub const CONTROL_COMMAND: u8 = 0x09;
    pub const EAR_DETECTION: u8 = 0x06;
    pub const CONVERSATION_AWARENESS: u8 = 0x4B;
    pub const INFORMATION: u8 = 0x1D;
    pub const RENAME: u8 = 0x1A;
    pub const PROXIMITY_KEYS_REQ: u8 = 0x30;
    pub const PROXIMITY_KEYS_RSP: u8 = 0x31;
    pub const STEM_PRESS: u8 = 0x19;
    pub const EQ_DATA: u8 = 0x53;
    pub const CONNECTED_DEVICES: u8 = 0x2E;
    pub const AUDIO_SOURCE: u8 = 0x0E;
    pub const SMART_ROUTING: u8 = 0x10;
    pub const SMART_ROUTING_RESP: u8 = 0x11;
    pub const SEND_CONNECTED_MAC: u8 = 0x14;
    pub const HEADTRACKING: u8 = 0x17;
    pub const TIPI_3: u8 = 0x0C;
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ControlCommandStatus {
    pub identifier: ControlCommandIdentifiers,
    pub value: Vec<u8>,
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ControlCommandIdentifiers {
    MicMode = 0x01,
    ButtonSendMode = 0x05,
    VoiceTrigger = 0x12,
    SingleClickMode = 0x14,
    DoubleClickMode = 0x15,
    ClickHoldMode = 0x16,
    DoubleClickInterval = 0x17,
    ClickHoldInterval = 0x18,
    ListeningModeConfigs = 0x1A,
    OneBudAncMode = 0x1B,
    CrownRotationDirection = 0x1C,
    ListeningMode = 0x0D,
    AutoAnswerMode = 0x1E,
    ChimeVolume = 0x1F,
    VolumeSwipeInterval = 0x23,
    CallManagementConfig = 0x24,
    VolumeSwipeMode = 0x25,
    AdaptiveVolumeConfig = 0x26,
    SoftwareMuteConfig = 0x27,
    ConversationDetectConfig = 0x28,
    Ssl = 0x29,
    HearingAid = 0x2C,
    AutoAncStrength = 0x2E,
    HpsGainSwipe = 0x2F,
    HrmState = 0x30,
    InCaseToneConfig = 0x31,
    SiriMultitoneConfig = 0x32,
    HearingAssistConfig = 0x33,
    AllowOffOption = 0x34,
    StemConfig = 0x39,
    SleepDetectionConfig = 0x35,
    AllowAutoConnect = 0x36,
    EarDetectionConfig = 0x0A,
    AutomaticConnectionConfig = 0x20,
    OwnsConnection = 0x06,
}

impl ControlCommandIdentifiers {
    fn from_u8(value: u8) -> Option<Self> {
        match value {
            0x01 => Some(Self::MicMode),
            0x05 => Some(Self::ButtonSendMode),
            0x12 => Some(Self::VoiceTrigger),
            0x14 => Some(Self::SingleClickMode),
            0x15 => Some(Self::DoubleClickMode),
            0x16 => Some(Self::ClickHoldMode),
            0x17 => Some(Self::DoubleClickInterval),
            0x18 => Some(Self::ClickHoldInterval),
            0x1A => Some(Self::ListeningModeConfigs),
            0x1B => Some(Self::OneBudAncMode),
            0x1C => Some(Self::CrownRotationDirection),
            0x0D => Some(Self::ListeningMode),
            0x1E => Some(Self::AutoAnswerMode),
            0x1F => Some(Self::ChimeVolume),
            0x23 => Some(Self::VolumeSwipeInterval),
            0x24 => Some(Self::CallManagementConfig),
            0x25 => Some(Self::VolumeSwipeMode),
            0x26 => Some(Self::AdaptiveVolumeConfig),
            0x27 => Some(Self::SoftwareMuteConfig),
            0x28 => Some(Self::ConversationDetectConfig),
            0x29 => Some(Self::Ssl),
            0x2C => Some(Self::HearingAid),
            0x2E => Some(Self::AutoAncStrength),
            0x2F => Some(Self::HpsGainSwipe),
            0x30 => Some(Self::HrmState),
            0x31 => Some(Self::InCaseToneConfig),
            0x32 => Some(Self::SiriMultitoneConfig),
            0x33 => Some(Self::HearingAssistConfig),
            0x34 => Some(Self::AllowOffOption),
            0x39 => Some(Self::StemConfig),
            0x35 => Some(Self::SleepDetectionConfig),
            0x36 => Some(Self::AllowAutoConnect),
            0x0A => Some(Self::EarDetectionConfig),
            0x20 => Some(Self::AutomaticConnectionConfig),
            0x06 => Some(Self::OwnsConnection),
            _ => None,
        }
    }
}

impl std::fmt::Display for ControlCommandIdentifiers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            ControlCommandIdentifiers::MicMode => "Mic Mode",
            ControlCommandIdentifiers::ButtonSendMode => "Button Send Mode",
            ControlCommandIdentifiers::VoiceTrigger => "Voice Trigger",
            ControlCommandIdentifiers::SingleClickMode => "Single Click Mode",
            ControlCommandIdentifiers::DoubleClickMode => "Double Click Mode",
            ControlCommandIdentifiers::ClickHoldMode => "Click Hold Mode",
            ControlCommandIdentifiers::DoubleClickInterval => "Double Click Interval",
            ControlCommandIdentifiers::ClickHoldInterval => "Click Hold Interval",
            ControlCommandIdentifiers::ListeningModeConfigs => "Listening Mode Configs",
            ControlCommandIdentifiers::OneBudAncMode => "One Bud ANC Mode",
            ControlCommandIdentifiers::CrownRotationDirection => "Crown Rotation Direction",
            ControlCommandIdentifiers::ListeningMode => "Listening Mode",
            ControlCommandIdentifiers::AutoAnswerMode => "Auto Answer Mode",
            ControlCommandIdentifiers::ChimeVolume => "Chime Volume",
            ControlCommandIdentifiers::VolumeSwipeInterval => "Volume Swipe Interval",
            ControlCommandIdentifiers::CallManagementConfig => "Call Management Config",
            ControlCommandIdentifiers::VolumeSwipeMode => "Volume Swipe Mode",
            ControlCommandIdentifiers::AdaptiveVolumeConfig => "Adaptive Volume Config",
            ControlCommandIdentifiers::SoftwareMuteConfig => "Software Mute Config",
            ControlCommandIdentifiers::ConversationDetectConfig => "Conversation Detect Config",
            ControlCommandIdentifiers::Ssl => "SSL",
            ControlCommandIdentifiers::HearingAid => "Hearing Aid",
            ControlCommandIdentifiers::AutoAncStrength => "Auto ANC Strength",
            ControlCommandIdentifiers::HpsGainSwipe => "HPS Gain Swipe",
            ControlCommandIdentifiers::HrmState => "HRM State",
            ControlCommandIdentifiers::InCaseToneConfig => "In Case Tone Config",
            ControlCommandIdentifiers::SiriMultitoneConfig => "Siri Multitone Config",
            ControlCommandIdentifiers::HearingAssistConfig => "Hearing Assist Config",
            ControlCommandIdentifiers::AllowOffOption => "Allow Off Option",
            ControlCommandIdentifiers::StemConfig => "Stem Config",
            ControlCommandIdentifiers::SleepDetectionConfig => "Sleep Detection Config",
            ControlCommandIdentifiers::AllowAutoConnect => "Allow Auto Connect",
            ControlCommandIdentifiers::EarDetectionConfig => "Ear Detection Config",
            ControlCommandIdentifiers::AutomaticConnectionConfig => "Automatic Connection Config",
            ControlCommandIdentifiers::OwnsConnection => "Owns Connection",
        };
        write!(f, "{}", name)
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Hash)]
pub enum ProximityKeyType {
    Irk = 0x01,
    EncKey = 0x04,
}

impl ProximityKeyType {
    fn from_u8(value: u8) -> Option<Self> {
        match value {
            0x01 => Some(Self::Irk),
            0x04 => Some(Self::EncKey),
            _ => None,
        }
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StemPressType {
    SinglePress = 0x05,
    DoublePress = 0x06,
    TriplePress = 0x07,
    LongPress = 0x08,
}

impl StemPressType {
    fn from_u8(value: u8) -> Option<Self> {
        match value {
            0x05 => Some(Self::SinglePress),
            0x06 => Some(Self::DoublePress),
            0x07 => Some(Self::TriplePress),
            0x08 => Some(Self::LongPress),
            _ => None,
        }
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StemPressBudType {
    Left = 0x01,
    Right = 0x02,
}

impl StemPressBudType {
    fn from_u8(value: u8) -> Option<Self> {
        match value {
            0x01 => Some(Self::Left),
            0x02 => Some(Self::Right),
            _ => None,
        }
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioSourceType {
    None = 0x00,
    Call = 0x01,
    Media = 0x02,
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatteryComponent {
    Headphone = 1,
    Left = 4,
    Right = 2,
    Case = 8,
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatteryStatus {
    /// Status byte 0x00: component known but state not reported.
    Unknown = 0,
    Charging = 1,
    NotCharging = 2,
    Disconnected = 4,
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EarDetectionStatus {
    InEar = 0x00,
    OutOfEar = 0x01,
    InCase = 0x02,
    Disconnected = 0x03,
}

impl AudioSourceType {
    fn from_u8(value: u8) -> Option<Self> {
        match value {
            0x00 => Some(Self::None),
            0x01 => Some(Self::Call),
            0x02 => Some(Self::Media),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioSource {
    pub mac: String,
    pub r#type: AudioSourceType,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatteryInfo {
    pub component: BatteryComponent,
    pub level: u8,
    pub status: BatteryStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectedDevice {
    pub mac: String,
    pub info1: u8,
    pub info2: u8,
    pub r#type: Option<String>,
}

#[derive(Debug, Clone)]
pub enum AACPEvent {
    BatteryInfo(Vec<BatteryInfo>),
    ControlCommand(ControlCommandStatus),
    EarDetection(Vec<EarDetectionStatus>, Vec<EarDetectionStatus>),
    ConversationalAwareness(u8),
    ProximityKeys(Vec<(u8, Vec<u8>)>),
    AudioSource(AudioSource),
    ConnectedDevices(Vec<ConnectedDevice>, Vec<ConnectedDevice>),
    OwnershipToFalseRequest,
    /// Another device on the AirPods says whether it is streaming media right now.
    PeerStreaming(bool),
    StemPress(StemPressType, StemPressBudType),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AirPodsLEKeys {
    pub irk: String,
    pub enc_key: String,
}

pub struct AACPManagerState {
    pub sender: Option<mpsc::Sender<Vec<u8>>>,
    pub control_command_status_list: Vec<ControlCommandStatus>,
    pub control_command_subscribers:
        HashMap<ControlCommandIdentifiers, Vec<mpsc::UnboundedSender<Vec<u8>>>>,
    pub owns: bool,
    pub old_connected_devices: Vec<ConnectedDevice>,
    pub connected_devices: Vec<ConnectedDevice>,
    pub audio_source: Option<AudioSource>,
    /// Whether another device last said it is streaming media (Smart Routing).
    pub peer_streaming: bool,
    /// Names other devices gave themselves in Smart Routing messages, by address.
    pub peer_names: HashMap<String, String>,
    pub battery_info: Vec<BatteryInfo>,
    pub conversational_awareness_status: u8,
    pub old_ear_detection_status: Vec<EarDetectionStatus>,
    pub ear_detection_status: Vec<EarDetectionStatus>,
    event_tx: Option<mpsc::UnboundedSender<AACPEvent>>,
    pub devices: HashMap<String, DeviceData>,
    pub airpods_mac: Option<Address>,
}

impl AACPManagerState {
    fn new() -> Self {
        let devices: HashMap<String, DeviceData> = std::fs::read_to_string(get_devices_path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        AACPManagerState {
            sender: None,
            control_command_status_list: Vec::new(),
            control_command_subscribers: HashMap::new(),
            owns: false,
            old_connected_devices: Vec::new(),
            connected_devices: Vec::new(),
            audio_source: None,
            peer_streaming: false,
            peer_names: HashMap::new(),
            battery_info: Vec::new(),
            conversational_awareness_status: 0,
            old_ear_detection_status: Vec::new(),
            ear_detection_status: Vec::new(),
            event_tx: None,
            devices,
            airpods_mac: None,
        }
    }
}

#[derive(Clone)]
pub struct AACPManager {
    pub state: Arc<Mutex<AACPManagerState>>,
    tasks: Arc<Mutex<JoinSet<()>>>,
}

impl AACPManager {
    pub fn new() -> Self {
        AACPManager {
            state: Arc::new(Mutex::new(AACPManagerState::new())),
            tasks: Arc::new(Mutex::new(JoinSet::new())),
        }
    }

    pub async fn connect(&mut self, addr: Address) {
        info!("AACPManager connecting to {} on PSM {:#06X}...", addr, PSM);
        let target_sa = SocketAddr::new(addr, AddressType::BrEdr, PSM);

        {
            let mut state = self.state.lock().await;
            state.airpods_mac = Some(addr);
        }

        let socket = match Socket::new_seq_packet() {
            Ok(s) => s,
            Err(e) => {
                error!("Failed to create L2CAP socket: {}", e);
                return;
            }
        };

        let seq_packet =
            match tokio::time::timeout(CONNECT_TIMEOUT, socket.connect(target_sa)).await {
                Ok(Ok(s)) => Arc::new(s),
                Ok(Err(e)) => {
                    error!("L2CAP connect failed: {}", e);
                    return;
                }
                Err(_) => {
                    error!("L2CAP connect timed out");
                    return;
                }
            };

        // Wait for connection to be fully established
        let start = Instant::now();
        loop {
            match seq_packet.peer_addr() {
                Ok(peer) if peer.cid != 0 => break,
                Ok(_) => { /* still waiting */ }
                Err(e) => {
                    if e.raw_os_error() == Some(107) {
                        // ENOTCONN
                        error!("Peer has disconnected during connection setup.");
                        return;
                    }
                    error!("Error getting peer address: {}", e);
                }
            }
            if start.elapsed() >= CONNECT_TIMEOUT {
                error!("Timed out waiting for L2CAP connection to be fully established.");
                return;
            }
            sleep(POLL_INTERVAL).await;
        }

        info!("L2CAP connection established with {}", addr);

        let (tx, rx) = mpsc::channel(128);

        let manager_clone = self.clone();
        {
            let mut state = self.state.lock().await;
            state.sender = Some(tx);
        }

        let mut tasks = self.tasks.lock().await;
        tasks.spawn(recv_thread(manager_clone, seq_packet.clone()));
        tasks.spawn(send_thread(rx, seq_packet));
    }

    async fn send_packet(&self, data: &[u8]) -> Result<()> {
        let state = self.state.lock().await;
        if let Some(sender) = &state.sender {
            sender.send(data.to_vec()).await.map_err(|e| {
                error!("Failed to send packet to channel: {}", e);
                Error::from(std::io::Error::new(
                    std::io::ErrorKind::NotConnected,
                    "L2CAP send channel closed",
                ))
            })
        } else {
            // Expected right after a disconnect; the caller gets the error and decides.
            debug!("Cannot send packet, sender is not available.");
            Err(Error::from(std::io::Error::new(
                std::io::ErrorKind::NotConnected,
                "L2CAP stream not connected",
            )))
        }
    }

    async fn send_data_packet(&self, data: &[u8]) -> Result<()> {
        let packet = [HEADER_BYTES.as_slice(), data].concat();
        self.send_packet(&packet).await
    }

    pub async fn set_event_channel(&self, tx: mpsc::UnboundedSender<AACPEvent>) {
        let mut state = self.state.lock().await;
        state.event_tx = Some(tx);
    }

    pub async fn subscribe_to_control_command(
        &self,
        identifier: ControlCommandIdentifiers,
        tx: mpsc::UnboundedSender<Vec<u8>>,
    ) {
        let mut state = self.state.lock().await;
        // send initial value if available
        if let Some(status) = state
            .control_command_status_list
            .iter()
            .find(|s| s.identifier == identifier)
        {
            let _ = tx.send(status.value.clone());
        }
        state
            .control_command_subscribers
            .entry(identifier)
            .or_default()
            .push(tx);
    }

    pub async fn receive_packet(&self, packet: &[u8]) {
        if !packet.starts_with(&HEADER_BYTES) {
            debug!(
                "Received packet does not start with expected header: {}",
                hex::encode(packet)
            );
            return;
        }
        if packet.len() < 5 {
            debug!("Received packet too short: {}", hex::encode(packet));
            return;
        }

        let opcode = packet[4];
        let payload = &packet[4..];

        match opcode {
            opcodes::BATTERY_INFO => {
                let Some(batteries) = parse_battery_info(payload) else {
                    error!("Invalid Battery Info packet: {}", hex::encode(payload));
                    return;
                };
                let mut state = self.state.lock().await;
                merge_battery_info(&mut state.battery_info, &batteries);
                let merged = state.battery_info.clone();
                if let Some(ref tx) = state.event_tx {
                    let _ = tx.send(AACPEvent::BatteryInfo(merged));
                }
                info!(
                    "Received Battery Info: {:?} (merged: {:?})",
                    batteries, state.battery_info
                );
            }
            opcodes::CONTROL_COMMAND => {
                if payload.len() < 7 {
                    error!("Control Command packet too short: {}", hex::encode(payload));
                    return;
                }
                let identifier_byte = payload[2];
                let value_bytes = &payload[3..7];

                let last_non_zero = value_bytes.iter().rposition(|&b| b != 0);
                let value = match last_non_zero {
                    Some(i) => value_bytes[..=i].to_vec(),
                    None => vec![0],
                };

                if let Some(identifier) = ControlCommandIdentifiers::from_u8(identifier_byte) {
                    let status = ControlCommandStatus {
                        identifier,
                        value: value.clone(),
                    };
                    let mut state = self.state.lock().await;
                    if let Some(existing) = state
                        .control_command_status_list
                        .iter_mut()
                        .find(|s| s.identifier == identifier)
                    {
                        existing.value = value.clone();
                    } else {
                        state.control_command_status_list.push(status.clone());
                    }
                    if identifier == ControlCommandIdentifiers::OwnsConnection {
                        state.owns = value_bytes[0] != 0;
                    }
                    if let Some(subscribers) = state.control_command_subscribers.get(&identifier) {
                        for sub in subscribers {
                            let _ = sub.send(value.clone());
                        }
                    }
                    if let Some(ref tx) = state.event_tx {
                        let _ = tx.send(AACPEvent::ControlCommand(status));
                    }
                    info!(
                        "Received Control Command: {:?}, value: {}",
                        identifier,
                        hex::encode(&value)
                    );
                } else {
                    // Newer firmware sends identifiers we don't know yet; harmless.
                    debug!(
                        "Unknown Control Command identifier: {:#04x}",
                        identifier_byte
                    );
                }
            }
            opcodes::EAR_DETECTION => {
                let Some((primary, secondary)) = parse_ear_detection(packet) else {
                    error!("Ear Detection packet too short: {}", hex::encode(packet));
                    return;
                };
                let statuses = vec![primary, secondary];
                let mut state = self.state.lock().await;
                state.old_ear_detection_status = state.ear_detection_status.clone();
                state.ear_detection_status = statuses.clone();

                if let Some(ref tx) = state.event_tx {
                    debug!(
                        "Sending Ear Detection event: old: {:?}, new: {:?}",
                        state.old_ear_detection_status, statuses
                    );
                    let _ = tx.send(AACPEvent::EarDetection(
                        state.old_ear_detection_status.clone(),
                        statuses,
                    ));
                }
                info!(
                    "Received Ear Detection Status: {:?}",
                    state.ear_detection_status
                );
            }
            opcodes::CONVERSATION_AWARENESS => {
                if packet.len() == 10 {
                    let status = packet[9];
                    let mut state = self.state.lock().await;
                    state.conversational_awareness_status = status;
                    if let Some(ref tx) = state.event_tx {
                        let _ = tx.send(AACPEvent::ConversationalAwareness(status));
                    }
                    info!("Received Conversation Awareness: {}", status);
                } else {
                    info!(
                        "Received Conversation Awareness packet with unexpected length: {}",
                        packet.len()
                    );
                }
            }
            opcodes::INFORMATION => {
                let Some(info) = parse_information(payload) else {
                    error!("Invalid Information packet: {}", hex::encode(payload));
                    return;
                };
                info!("Received Information: {:?}", info);
                let mac = self.state.lock().await.airpods_mac;
                let Some(mac) = mac else {
                    error!("Received Information before AirPods MAC was known; not persisting.");
                    return;
                };
                let mac_str = mac.to_string();
                let result = update_devices_file(|devices| {
                    merge_information(devices, &mac_str, info);
                })
                .await;
                match result {
                    Ok(devices) => self.state.lock().await.devices = devices,
                    Err(e) => error!("Failed to save devices: {}", e),
                }
            }

            opcodes::PROXIMITY_KEYS_RSP => {
                let Some(keys) = parse_proximity_keys(payload) else {
                    error!(
                        "Invalid Proximity Keys Response packet: {}",
                        hex::encode(payload)
                    );
                    return;
                };
                // Keys are secrets: log only their types and lengths.
                info!(
                    "Received Proximity Keys Response: {:?}",
                    keys.iter()
                        .map(|(kt, kd)| (kt, kd.len()))
                        .collect::<Vec<_>>()
                );
                let mut irk = None;
                let mut enc_key = None;
                for (key_type, key_data) in &keys {
                    match ProximityKeyType::from_u8(*key_type) {
                        Some(ProximityKeyType::Irk) => irk = Some(hex::encode(key_data)),
                        Some(ProximityKeyType::EncKey) => enc_key = Some(hex::encode(key_data)),
                        None => debug!("Ignoring unknown proximity key type {:#04x}", key_type),
                    }
                }
                let mac = self.state.lock().await.airpods_mac;
                let Some(mac) = mac else {
                    error!("Received Proximity Keys before AirPods MAC was known; not persisting.");
                    return;
                };
                if irk.is_none() && enc_key.is_none() {
                    return;
                }
                let mac_str = mac.to_string();
                let result = update_devices_file(|devices| {
                    merge_le_keys(devices, &mac_str, irk.as_deref(), enc_key.as_deref());
                })
                .await;
                match result {
                    Ok(devices) => self.state.lock().await.devices = devices,
                    Err(e) => error!("Failed to save devices: {}", e),
                }
                if let Some(ref tx) = self.state.lock().await.event_tx {
                    let _ = tx.send(AACPEvent::ProximityKeys(keys));
                }
            }
            opcodes::STEM_PRESS => {
                if payload.len() < 4 {
                    error!("Stem Press packet too short: {}", hex::encode(payload));
                    return;
                }
                let press_type = StemPressType::from_u8(payload[2]);
                let bud_type = StemPressBudType::from_u8(payload[3]);
                if let (Some(press), Some(bud)) = (press_type, bud_type) {
                    info!("Received Stem Press: {:?} on {:?}", press, bud);
                    let state = self.state.lock().await;
                    if let Some(ref tx) = state.event_tx {
                        let _ = tx.send(AACPEvent::StemPress(press, bud));
                    }
                } else {
                    error!(
                        "Invalid Stem Press packet - type: {:?}, bud: {:?}",
                        press_type, bud_type
                    );
                }
            }
            opcodes::AUDIO_SOURCE => {
                if payload.len() < 9 {
                    error!("Audio Source packet too short: {}", hex::encode(payload));
                    return;
                }
                let mac = format!(
                    "{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
                    payload[7], payload[6], payload[5], payload[4], payload[3], payload[2]
                );
                let typ = AudioSourceType::from_u8(payload[8]).unwrap_or(AudioSourceType::None);
                let audio_source = AudioSource { mac, r#type: typ };
                let mut state = self.state.lock().await;
                state.audio_source = Some(audio_source.clone());
                if let Some(ref tx) = state.event_tx {
                    let _ = tx.send(AACPEvent::AudioSource(audio_source));
                }
                info!("Received Audio Source: {:?}", state.audio_source);
            }
            opcodes::CONNECTED_DEVICES => {
                if payload.len() < 5 {
                    error!(
                        "Connected Devices packet too short: {}",
                        hex::encode(payload)
                    );
                    return;
                }
                // Layout matches the Android client: count at byte 4, 8-byte entries from byte 5.
                let count = payload[4] as usize;
                if payload.len() < 5 + count * 8 {
                    error!(
                        "Connected Devices packet length mismatch: {}",
                        hex::encode(payload)
                    );
                    return;
                }
                let mut devices = Vec::with_capacity(count);
                for i in 0..count {
                    let base = 5 + i * 8;
                    let mac = format!(
                        "{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
                        payload[base],
                        payload[base + 1],
                        payload[base + 2],
                        payload[base + 3],
                        payload[base + 4],
                        payload[base + 5]
                    );
                    let info1 = payload[base + 6];
                    let info2 = payload[base + 7];
                    devices.push(ConnectedDevice {
                        mac,
                        info1,
                        info2,
                        r#type: None,
                    });
                }
                let mut state = self.state.lock().await;
                state.old_connected_devices = state.connected_devices.clone();
                state.connected_devices = devices.clone();
                if let Some(ref tx) = state.event_tx {
                    let _ = tx.send(AACPEvent::ConnectedDevices(
                        state.old_connected_devices.clone(),
                        devices,
                    ));
                }
                info!("Received Connected Devices: {:?}", state.connected_devices);
            }
            opcodes::SMART_ROUTING_RESP => {
                let packet_string = String::from_utf8_lossy(payload.get(2..).unwrap_or(&[]));
                info!("Received Smart Routing Response: {}", packet_string);
                if let Some((mac, name)) = peer_identity(&packet_string) {
                    self.state.lock().await.peer_names.insert(mac, name);
                }
                if let Some(streaming) = peer_streaming_state(&packet_string) {
                    let mut state = self.state.lock().await;
                    state.peer_streaming = streaming;
                    if let Some(ref tx) = state.event_tx {
                        let _ = tx.send(AACPEvent::PeerStreaming(streaming));
                    }
                }
                if packet_string.contains("SetOwnershipToFalse") {
                    info!("Received OwnershipToFalse request");
                    if let Some(ref tx) = self.state.lock().await.event_tx {
                        let _ = tx.send(AACPEvent::OwnershipToFalseRequest);
                    }
                }
            }
            opcodes::EQ_DATA => {
                debug!("Received EQ Data");
            }
            _ => debug!("Received unknown packet with opcode {:#04x}", opcode),
        }
    }

    pub async fn send_notification_request(&self) -> Result<()> {
        let opcode = [opcodes::REQUEST_NOTIFICATIONS, 0x00];
        let data = [0xFF, 0xFF, 0xFF, 0xFF];
        let packet = [opcode.as_slice(), data.as_slice()].concat();
        self.send_data_packet(&packet).await
    }

    pub async fn send_set_feature_flags_packet(&self) -> Result<()> {
        let opcode = [opcodes::SET_FEATURE_FLAGS, 0x00];
        // let data = [0xD7, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
        let data = [0xFF, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]; // adaptive volume is actually useful, seeing if it works
        let packet = [opcode.as_slice(), data.as_slice()].concat();
        self.send_data_packet(&packet).await
    }

    pub async fn send_handshake(&self) -> Result<()> {
        let packet = [
            0x00, 0x00, 0x04, 0x00, 0x01, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00,
        ];
        self.send_packet(&packet).await
    }

    pub async fn send_proximity_keys_request(
        &self,
        key_types: Vec<ProximityKeyType>,
    ) -> Result<()> {
        let opcode = [opcodes::PROXIMITY_KEYS_REQ, 0x00];
        let mut data = Vec::with_capacity(2);
        data.push(key_types.iter().fold(0u8, |acc, kt| acc | (*kt as u8)));
        data.push(0x00);
        let packet = [opcode.as_slice(), data.as_slice()].concat();
        self.send_data_packet(&packet).await
    }

    pub async fn send_rename_packet(&self, name: &str) -> Result<()> {
        let name_bytes = name.as_bytes();
        let size = name_bytes.len();
        let mut packet = Vec::with_capacity(6 + size);
        packet.push(opcodes::RENAME);
        packet.push(0x00);
        packet.push(0x01);
        packet.push(size as u8);
        packet.push(0x00);
        packet.extend_from_slice(name_bytes);
        self.send_data_packet(&packet).await
    }

    pub async fn send_control_command(
        &self,
        identifier: ControlCommandIdentifiers,
        value: &[u8],
    ) -> Result<()> {
        let opcode = [opcodes::CONTROL_COMMAND, 0x00];
        let mut data = vec![identifier as u8];
        for i in 0..4 {
            data.push(value.get(i).copied().unwrap_or(0));
        }
        let packet = [opcode.as_slice(), data.as_slice()].concat();
        self.send_data_packet(&packet).await
    }

    pub async fn send_media_information_new_device(
        &self,
        self_mac_address: &str,
        target_mac_address: &str,
    ) -> Result<()> {
        let opcode = [opcodes::SMART_ROUTING, 0x00];
        let mut buffer = Vec::with_capacity(112);
        buffer.extend_from_slice(&mac_to_reversed_bytes(target_mac_address)?);

        buffer.extend_from_slice(&[0x68, 0x00]);
        buffer.extend_from_slice(&[0x01, 0xE5, 0x4A]);
        buffer.extend_from_slice(b"playingApp");
        buffer.push(0x42);
        buffer.extend_from_slice(b"NA");
        buffer.push(0x52);
        buffer.extend_from_slice(b"hostStreamingState");
        buffer.push(0x42);
        buffer.extend_from_slice(b"NO");
        buffer.push(0x49);
        buffer.extend_from_slice(b"btAddress");
        buffer.push(0x51);
        buffer.extend_from_slice(self_mac_address.as_bytes());
        buffer.push(0x46);
        buffer.extend_from_slice(b"btName");
        buffer.push(0x43);
        buffer.extend_from_slice(b"Mac");
        buffer.push(0x58);
        buffer.extend_from_slice(b"otherDevice");
        buffer.extend_from_slice(b"AudioCategory");
        buffer.extend_from_slice(&[0x30, 0x64]);

        let packet = [opcode.as_slice(), buffer.as_slice()].concat();
        self.send_data_packet(&packet).await
    }

    pub async fn send_hijack_request(&self, target_mac_address: &str) -> Result<()> {
        let opcode = [opcodes::SMART_ROUTING, 0x00];
        let mut buffer = Vec::with_capacity(106);
        buffer.extend_from_slice(&mac_to_reversed_bytes(target_mac_address)?);
        buffer.extend_from_slice(&[0x62, 0x00]);
        buffer.extend_from_slice(&[0x01, 0xE5]);
        buffer.push(0x4A);
        buffer.extend_from_slice(b"localscore");
        buffer.extend_from_slice(&[0x30, 0x64]);
        buffer.push(0x46);
        buffer.extend_from_slice(b"reason");
        buffer.push(0x48);
        buffer.extend_from_slice(b"Hijackv2");
        buffer.push(0x51);
        buffer.extend_from_slice(b"audioRoutingScore");
        buffer.extend_from_slice(&[0x31, 0x2D, 0x01, 0x5F]);
        buffer.extend_from_slice(b"audioRoutingSetOwnershipToFalse");
        buffer.push(0x01);
        buffer.push(0x4B);
        buffer.extend_from_slice(b"remotescore");
        buffer.push(0xA5);

        while buffer.len() < 106 {
            buffer.push(0x00);
        }

        let packet = [opcode.as_slice(), buffer.as_slice()].concat();
        self.send_data_packet(&packet).await
    }

    pub async fn send_media_information(
        &self,
        self_mac_address: &str,
        target_mac_address: &str,
        streaming_state: bool,
    ) -> Result<()> {
        let packet =
            media_information_packet(self_mac_address, target_mac_address, streaming_state)?;
        self.send_data_packet(&packet).await
    }

    pub async fn send_smart_routing_show_ui(&self, target_mac_address: &str) -> Result<()> {
        let opcode = [opcodes::SMART_ROUTING, 0x00];
        let mut buffer = Vec::with_capacity(134);
        buffer.extend_from_slice(&mac_to_reversed_bytes(target_mac_address)?);
        buffer.extend_from_slice(&[0x7E, 0x00]);
        buffer.extend_from_slice(&[0x01, 0xE6, 0x5B]);
        buffer.extend_from_slice(b"SmartRoutingKeyShowNearbyUI");
        buffer.push(0x01);
        buffer.push(0x4A);
        buffer.extend_from_slice(b"localscore");
        buffer.extend_from_slice(&[0x31, 0x2D]);
        buffer.push(0x01);
        buffer.push(0x46);
        buffer.extend_from_slice(b"reasonHhijackv2");
        buffer.push(0x51);
        buffer.extend_from_slice(b"audioRoutingScore");
        buffer.push(0xA2);
        buffer.push(0x5F);
        buffer.extend_from_slice(b"audioRoutingSetOwnershipToFalse");
        buffer.push(0x01);
        buffer.push(0x4B);
        buffer.extend_from_slice(b"remotescore");
        buffer.push(0xA2);

        while buffer.len() < 134 {
            buffer.push(0x00);
        }

        let packet = [opcode.as_slice(), buffer.as_slice()].concat();
        self.send_data_packet(&packet).await
    }

    pub async fn send_hijack_reversed(&self, target_mac_address: &str) -> Result<()> {
        let opcode = [opcodes::SMART_ROUTING, 0x00];
        let mut buffer = Vec::with_capacity(97);
        buffer.extend_from_slice(&mac_to_reversed_bytes(target_mac_address)?);
        buffer.extend_from_slice(&[0x59, 0x00]);
        buffer.extend_from_slice(&[0x01, 0xE3]);
        buffer.push(0x5F);
        buffer.extend_from_slice(b"audioRoutingSetOwnershipToFalse");
        buffer.push(0x01);
        buffer.push(0x59);
        buffer.extend_from_slice(b"audioRoutingShowReverseUI");
        buffer.push(0x01);
        buffer.push(0x46);
        buffer.extend_from_slice(b"reason");
        buffer.push(0x53);
        buffer.extend_from_slice(b"ReverseBannerTapped");

        while buffer.len() < 97 {
            buffer.push(0x00);
        }

        let packet = [opcode.as_slice(), buffer.as_slice()].concat();
        self.send_data_packet(&packet).await
    }

    pub async fn send_add_tipi_device(
        &self,
        self_mac_address: &str,
        target_mac_address: &str,
    ) -> Result<()> {
        let opcode = [opcodes::SMART_ROUTING, 0x00];
        let mut buffer = Vec::with_capacity(86);
        buffer.extend_from_slice(&mac_to_reversed_bytes(target_mac_address)?);
        buffer.extend_from_slice(&[0x4E, 0x00]);
        buffer.extend_from_slice(&[0x01, 0xE5]);
        buffer.extend_from_slice(&[0x48, 0x69]);
        buffer.extend_from_slice(b"idleTime");
        buffer.extend_from_slice(&[0x08, 0x47]);
        buffer.extend_from_slice(b"newTipi");
        buffer.extend_from_slice(&[0x01, 0x49]);
        buffer.extend_from_slice(b"btAddress");
        buffer.push(0x51);
        buffer.extend_from_slice(self_mac_address.as_bytes());
        buffer.push(0x46);
        buffer.extend_from_slice(b"btName");
        buffer.push(0x43);
        buffer.extend_from_slice(b"Mac");
        buffer.push(0x50);
        buffer.extend_from_slice(b"nearbyAudioScore");
        buffer.push(0x0E);

        let packet = [opcode.as_slice(), buffer.as_slice()].concat();
        self.send_data_packet(&packet).await
    }

    pub async fn send_some_packet(&self) -> Result<()> {
        self.send_data_packet(&[0x29, 0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF])
            .await
    }
}

// ---------------------------------------------------------------------------
// Pure, bounds-checked parsers. None of these may panic on any input.
// ---------------------------------------------------------------------------

const MAX_BATTERY_RECORDS: usize = 4;
const BATTERY_RECORD_LEN: usize = 5;

fn battery_component_from_u8(v: u8) -> Option<BatteryComponent> {
    match v {
        0x01 => Some(BatteryComponent::Headphone),
        0x02 => Some(BatteryComponent::Right),
        0x04 => Some(BatteryComponent::Left),
        0x08 => Some(BatteryComponent::Case),
        _ => None,
    }
}

fn battery_status_from_u8(v: u8) -> Option<BatteryStatus> {
    match v {
        0x00 => Some(BatteryStatus::Unknown),
        0x01 => Some(BatteryStatus::Charging),
        0x02 => Some(BatteryStatus::NotCharging),
        0x04 => Some(BatteryStatus::Disconnected),
        _ => None,
    }
}

/// Parse a BATTERY_INFO payload (starting at the opcode byte, i.e. packet minus the
/// 4-byte header): `04 00 <count> { <component> 01 <level> <status> 01 }*count`.
///
/// Returns `None` when the count is implausible, the packet is shorter than the
/// count implies, or a record's spacer bytes are wrong (the whole packet is then
/// untrustworthy). Records with an unknown component or status byte are skipped.
pub fn parse_battery_info(payload: &[u8]) -> Option<Vec<BatteryInfo>> {
    if payload.len() < 3 || payload[0] != opcodes::BATTERY_INFO {
        return None;
    }
    let count = payload[2] as usize;
    if count == 0 || count > MAX_BATTERY_RECORDS {
        return None;
    }
    let records = payload.get(3..3 + count * BATTERY_RECORD_LEN)?;
    let mut out = Vec::with_capacity(count);
    for rec in records.chunks_exact(BATTERY_RECORD_LEN) {
        let [comp, spacer, level, status, end] = rec else {
            return None;
        };
        if *spacer != 0x01 || *end != 0x01 {
            return None;
        }
        let Some(component) = battery_component_from_u8(*comp) else {
            debug!("Unknown battery component: {:#04x}", comp);
            continue;
        };
        let Some(status) = battery_status_from_u8(*status) else {
            debug!("Unknown battery status: {:#04x}", status);
            continue;
        };
        out.push(BatteryInfo {
            component,
            level: *level,
            status,
        });
    }
    Some(out)
}

/// Merge freshly parsed records into the accumulated per-component list.
/// Components absent from `new` are kept. A `Disconnected`/`Unknown` record with
/// level 0, or any record with a level above 100, keeps the previously known level.
pub fn merge_battery_info(existing: &mut Vec<BatteryInfo>, new: &[BatteryInfo]) {
    for rec in new {
        let no_data = matches!(
            rec.status,
            BatteryStatus::Disconnected | BatteryStatus::Unknown
        ) && rec.level == 0;
        let level_valid = rec.level <= 100 && !no_data;
        if let Some(old) = existing.iter_mut().find(|b| b.component == rec.component) {
            if level_valid {
                old.level = rec.level;
            }
            old.status = rec.status;
        } else {
            existing.push(BatteryInfo {
                level: if rec.level <= 100 { rec.level } else { 0 },
                ..rec.clone()
            });
        }
    }
}

fn ear_status_from_u8(v: u8) -> EarDetectionStatus {
    match v {
        0x00 => EarDetectionStatus::InEar,
        0x01 => EarDetectionStatus::OutOfEar,
        0x02 => EarDetectionStatus::InCase,
        0x03 => EarDetectionStatus::Disconnected,
        _ => {
            debug!("Unknown ear detection status: {:#04x}", v);
            EarDetectionStatus::OutOfEar
        }
    }
}

/// Parse an EAR_DETECTION packet (full packet including header). Returns
/// (primary, secondary).
pub fn parse_ear_detection(packet: &[u8]) -> Option<(EarDetectionStatus, EarDetectionStatus)> {
    let primary = *packet.get(6)?;
    let secondary = *packet.get(7)?;
    Some((ear_status_from_u8(primary), ear_status_from_u8(secondary)))
}

fn empty_le_keys() -> AirPodsLEKeys {
    AirPodsLEKeys {
        irk: String::new(),
        enc_key: String::new(),
    }
}

fn empty_airpods_information() -> AirPodsInformation {
    AirPodsInformation {
        name: String::new(),
        model_number: String::new(),
        manufacturer: String::new(),
        serial_number: String::new(),
        version1: String::new(),
        version2: String::new(),
        hardware_revision: String::new(),
        updater_identifier: String::new(),
        left_serial_number: String::new(),
        right_serial_number: String::new(),
        version3: String::new(),
        le_keys: empty_le_keys(),
    }
}

/// Parse an INFORMATION payload (starting at the opcode). The data after the
/// 4-byte sub-header is a binary prefix followed by NUL-separated tokens. Same
/// token selection as the original parser (and the Android app): skip the
/// prefix up to the first NUL, keep non-empty UTF-8 tokens, drop the first one
/// (a binary field), then the rest are name, model number, ... LE keys are empty.
pub fn parse_information(payload: &[u8]) -> Option<AirPodsInformation> {
    let data = payload.get(4..)?;
    if data.len() < 2 {
        return None;
    }
    let first_nul = data.iter().position(|&b| b == 0)?;
    let strings: Vec<String> = data[first_nul..]
        .split(|&b| b == 0)
        .filter(|s| !s.is_empty())
        .filter_map(|s| std::str::from_utf8(s).ok().map(str::to_owned))
        .skip(1)
        .collect();
    let get = |i: usize| strings.get(i).cloned().unwrap_or_default();
    Some(AirPodsInformation {
        name: get(0),
        model_number: get(1),
        manufacturer: get(2),
        serial_number: get(3),
        version1: get(4),
        version2: get(5),
        hardware_revision: get(6),
        updater_identifier: get(7),
        left_serial_number: get(8),
        right_serial_number: get(9),
        version3: get(10),
        le_keys: empty_le_keys(),
    })
}

/// Parse a PROXIMITY_KEYS_RSP payload (starting at the opcode):
/// `31 00 <count> { <type> 00 <len> 00 <key bytes...> }*count`.
pub fn parse_proximity_keys(payload: &[u8]) -> Option<Vec<(u8, Vec<u8>)>> {
    if payload.len() < 3 {
        return None;
    }
    let key_count = payload[2] as usize;
    let mut offset = 3usize;
    let mut keys = Vec::with_capacity(key_count);
    for _ in 0..key_count {
        let header = payload.get(offset..offset + 4)?;
        let key_type = header[0];
        let key_length = header[2] as usize;
        offset += 4;
        let key_data = payload.get(offset..offset + key_length)?;
        keys.push((key_type, key_data.to_vec()));
        offset += key_length;
    }
    Some(keys)
}

/// Parse "AA:BB:CC:DD:EE:FF" into bytes in over-the-air (reversed) order.
/// Smart Routing "media information": tells another device on the AirPods whether this
/// host is streaming media, which decides whether that device may take the audio route.
fn media_information_packet(
    self_mac_address: &str,
    target_mac_address: &str,
    streaming_state: bool,
) -> Result<Vec<u8>> {
    let opcode = [opcodes::SMART_ROUTING, 0x00];
    let mut buffer = Vec::with_capacity(138);
    buffer.extend_from_slice(&mac_to_reversed_bytes(target_mac_address)?);
    buffer.extend_from_slice(&[0x82, 0x00]);
    buffer.extend_from_slice(&[0x01, 0xE5, 0x4A]);
    // Keys are lowercase camelCase, as an iPhone sends them.
    buffer.extend_from_slice(b"playingApp");
    buffer.push(0x56);
    buffer.extend_from_slice(b"com.google.ios.youtube");
    buffer.push(0x52);
    buffer.extend_from_slice(b"hostStreamingState");
    // OPACK strings carry their length in the type byte (0x40 + length), so "YES" needs
    // 0x43; upstream sent 0x42 for both, which made "YES" unreadable.
    let state: &[u8] = if streaming_state { b"YES" } else { b"NO" };
    buffer.push(0x40 + state.len() as u8);
    buffer.extend_from_slice(state);
    buffer.push(0x49);
    buffer.extend_from_slice(b"btAddress");
    buffer.push(0x51);
    buffer.extend_from_slice(self_mac_address.as_bytes());
    buffer.push(0x46);
    buffer.extend_from_slice(b"btName");
    buffer.push(0x43);
    buffer.extend_from_slice(b"Mac");
    buffer.push(0x58);
    buffer.extend_from_slice(b"otherDevice");
    buffer.extend_from_slice(b"AudioCategory");
    buffer.extend_from_slice(&[0x31, 0x2D, 0x01]);

    while buffer.len() < 138 {
        buffer.push(0x00);
    }
    Ok([opcode.as_slice(), buffer.as_slice()].concat())
}

/// Reads `hostStreamingState` (any casing) from a Smart Routing message another device
/// sent: `Some(true)` when it streams media from an app, `Some(false)` for NO, `None` when
/// the message has no such key. A YES for `playingApp` "Unknown" is ignored: an iPhone sends
/// it in a quick YES/NO pair around system sounds and when its own playback stops.
pub fn peer_streaming_state(packet: &str) -> Option<bool> {
    let lower = packet.to_ascii_lowercase();
    let key = "hoststreamingstate";
    let rest = &lower[lower.find(key)? + key.len()..];
    // The value follows the key after one OPACK type byte.
    let value: String = rest.chars().skip(1).take(3).collect();
    if value.starts_with("yes") {
        // "playingApp" then a 7-character string tag ("G") and "Unknown".
        (!lower.contains("playingappgunknown")).then_some(true)
    } else if value.starts_with("no") {
        Some(false)
    } else {
        None
    }
}

/// Reads `btAddress` and `btName` from a Smart Routing message another device sent, e.g.
/// ("10:A2:D3:BF:3D:76", "iPhone").
pub fn peer_identity(packet: &str) -> Option<(String, String)> {
    let mac = opack_string_after(packet, "btAddress")?;
    let name = opack_string_after(packet, "btName")?;
    (mac.len() == 17 && !name.is_empty()).then(|| (mac.to_ascii_uppercase(), name))
}

/// The OPACK short string that follows `key`: one type byte (0x40 + length), then the text.
fn opack_string_after(packet: &str, key: &str) -> Option<String> {
    let rest = &packet[packet.find(key)? + key.len()..];
    let mut chars = rest.chars();
    let len = (chars.next()? as u32)
        .checked_sub(0x40)
        .filter(|&l| l > 0 && l < 0x20)?;
    let value: String = chars.take(len as usize).collect();
    (value.chars().count() == len as usize).then_some(value)
}

pub fn mac_to_reversed_bytes(mac: &str) -> Result<[u8; 6]> {
    let addr: Address = mac.trim().parse().map_err(|_| {
        Error::from(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("invalid MAC address: {mac:?}"),
        ))
    })?;
    let mut bytes = addr.0;
    bytes.reverse();
    Ok(bytes)
}

// ---------------------------------------------------------------------------
// devices.json persistence
// ---------------------------------------------------------------------------

fn airpods_entry<'a>(
    devices: &'a mut HashMap<String, DeviceData>,
    mac: &str,
) -> &'a mut DeviceData {
    let entry = devices
        .entry(mac.to_string())
        .or_insert_with(|| DeviceData {
            name: mac.to_string(),
            type_: DeviceType::AirPods,
            information: None,
        });
    if !matches!(entry.information, Some(DeviceInformation::AirPods(_))) {
        entry.type_ = DeviceType::AirPods;
        entry.information = Some(DeviceInformation::AirPods(empty_airpods_information()));
    }
    entry
}

/// Store device information, creating the entry if needed. Existing non-empty
/// LE keys are never replaced by empty ones.
pub fn merge_information(
    devices: &mut HashMap<String, DeviceData>,
    mac: &str,
    mut info: AirPodsInformation,
) {
    let entry = airpods_entry(devices, mac);
    if !info.name.is_empty() {
        entry.name = info.name.clone();
    }
    if let Some(DeviceInformation::AirPods(old)) = &entry.information {
        if info.le_keys.irk.is_empty() {
            info.le_keys.irk = old.le_keys.irk.clone();
        }
        if info.le_keys.enc_key.is_empty() {
            info.le_keys.enc_key = old.le_keys.enc_key.clone();
        }
    }
    entry.information = Some(DeviceInformation::AirPods(info));
}

/// Store LE keys (hex), creating the entry if needed. `None`/empty values leave
/// the stored key untouched.
pub fn merge_le_keys(
    devices: &mut HashMap<String, DeviceData>,
    mac: &str,
    irk: Option<&str>,
    enc_key: Option<&str>,
) {
    let entry = airpods_entry(devices, mac);
    if let Some(DeviceInformation::AirPods(info)) = entry.information.as_mut() {
        if let Some(irk) = irk.filter(|k| !k.is_empty()) {
            info.le_keys.irk = irk.to_string();
        }
        if let Some(enc) = enc_key.filter(|k| !k.is_empty()) {
            info.le_keys.enc_key = enc.to_string();
        }
    }
}

/// Serialises read-modify-write cycles of devices.json within this process.
static DEVICES_FILE_LOCK: Mutex<()> = Mutex::const_new(());

/// Write `devices` to `path` atomically: write a temp file next to it, then rename.
/// Creates the parent directory if needed.
pub async fn save_devices_atomic(
    path: &std::path::Path,
    devices: &HashMap<String, DeviceData>,
) -> std::io::Result<()> {
    let json = serde_json::to_string_pretty(devices)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(format!(".tmp.{}", std::process::id()));
    let tmp = std::path::PathBuf::from(tmp);
    tokio::fs::write(&tmp, json).await?;
    if let Err(e) = tokio::fs::rename(&tmp, path).await {
        let _ = tokio::fs::remove_file(&tmp).await;
        return Err(e);
    }
    Ok(())
}

/// Re-read devices.json at `path`, apply `f`, and write it back atomically.
/// Re-reading avoids clobbering entries written by other device tasks. A file
/// that exists but cannot be parsed is moved aside to `<path>.bak` rather than lost.
pub async fn update_devices_file_at<F>(
    path: &std::path::Path,
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
                let _ = tokio::fs::rename(path, std::path::PathBuf::from(bak)).await;
                HashMap::new()
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => HashMap::new(),
        Err(e) => return Err(e),
    };
    f(&mut devices);
    save_devices_atomic(path, &devices).await?;
    Ok(devices)
}

async fn update_devices_file<F>(f: F) -> std::io::Result<HashMap<String, DeviceData>>
where
    F: FnOnce(&mut HashMap<String, DeviceData>),
{
    update_devices_file_at(&get_devices_path(), f).await
}

async fn recv_thread(manager: AACPManager, sp: Arc<SeqPacket>) {
    let mut buf = vec![0u8; 1024];
    loop {
        match sp.recv(&mut buf).await {
            Ok(0) => {
                info!("Remote closed the connection.");
                break;
            }
            Ok(n) => {
                let data = &buf[..n];
                debug!("Received {} bytes: {}", n, hex::encode(data));
                manager.receive_packet(data).await;
            }
            Err(e) => {
                debug!("Read error: {}", e);
                info!(
                    "We have probably disconnected, clearing state variables (owns=false, connected_devices=empty, control_command_status_list=empty)."
                );
                let mut state = manager.state.lock().await;
                state.owns = false;
                state.connected_devices.clear();
                state.control_command_status_list.clear();
                break;
            }
        }
    }
    let mut state = manager.state.lock().await;
    state.sender = None;
}

async fn send_thread(mut rx: mpsc::Receiver<Vec<u8>>, sp: Arc<SeqPacket>) {
    while let Some(data) = rx.recv().await {
        let mut attempts = 0;
        loop {
            match sp.send(&data).await {
                Ok(_) => {
                    debug!("Sent {} bytes: {}", data.len(), hex::encode(&data));
                    break;
                }
                Err(e) if e.raw_os_error() == Some(107) && attempts < 10 => {
                    attempts += 1;
                    sleep(Duration::from_millis(100)).await;
                }
                Err(e) => {
                    error!("Failed to send data: {}", e);
                    return;
                }
            }
        }
    }
    info!("Send thread finished.");
}

// Lifecycle helpers used by the device supervisor.
impl AACPManager {
    /// True while the L2CAP control channel is up.
    pub async fn is_connected(&self) -> bool {
        self.state.lock().await.sender.is_some()
    }

    /// Tear down the control channel and its I/O tasks. Safe to call more than once.
    pub async fn shutdown(&self) {
        self.tasks.lock().await.abort_all();
        let mut state = self.state.lock().await;
        state.sender = None;
        state.event_tx = None;
        state.control_command_subscribers.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_peer_streaming_state() {
        let yes = "v=\u{fffd}JplayingAppVcom.google.ios.youtubeRhostStreamingStateCYESIbtAddressQ10:A2:D3:BF:3D:76FbtNameFiPhone";
        let no = "JplayingAppGUnknownRhostStreamingStateBNOIbtAddressQ10:A2:D3:BF:3D:76";
        let blip = "JplayingAppGUnknownRhostStreamingStateCYESIbtAddressQ10:A2:D3:BF:3D:76";
        assert_eq!(peer_streaming_state(yes), Some(true));
        assert_eq!(peer_streaming_state(no), Some(false));
        assert_eq!(peer_streaming_state(blip), None);
        assert_eq!(
            peer_identity(yes),
            Some(("10:A2:D3:BF:3D:76".into(), "iPhone".into()))
        );
        assert_eq!(peer_identity("btNameFiPh"), None);
        assert_eq!(
            peer_streaming_state("audioRoutingSetOwnershipToFalse"),
            None
        );
    }

    #[test]
    fn media_information_is_tagged_like_an_iphone_sends_it() {
        let find = |hay: &[u8], needle: &[u8]| hay.windows(needle.len()).any(|w| w == needle);
        let local = "E8:65:38:93:3D:9E";
        let phone = "10:A2:D3:BF:3D:76";
        let yes = media_information_packet(local, phone, true).unwrap();
        assert!(find(
            &yes,
            b"RhostStreamingStateCYESIbtAddressQE8:65:38:93:3D:9EFbtNameCMac"
        ));
        let no = media_information_packet(local, phone, false).unwrap();
        assert!(find(&no, b"RhostStreamingStateBNOIbtAddress"));
        assert_eq!(yes.len(), 140);
        // Our own message reads back as streaming, like the iPhone's do.
        assert_eq!(
            peer_streaming_state(&String::from_utf8_lossy(&yes)),
            Some(true)
        );
    }

    fn bi(component: BatteryComponent, level: u8, status: BatteryStatus) -> BatteryInfo {
        BatteryInfo {
            component,
            level,
            status,
        }
    }

    /// Full packet as received (header + payload) -> payload slice like receive_packet.
    fn payload(packet: &[u8]) -> &[u8] {
        &packet[4..]
    }

    #[test]
    fn battery_three_components() {
        let pkt = [
            0x04, 0x00, 0x04, 0x00, 0x04, 0x00, 0x03, //
            0x02, 0x01, 0x64, 0x02, 0x01, // right 100 not charging
            0x04, 0x01, 0x63, 0x01, 0x01, // left 99 charging
            0x08, 0x01, 0x11, 0x02, 0x01, // case 17
        ];
        let b = parse_battery_info(payload(&pkt)).unwrap();
        assert_eq!(
            b,
            vec![
                bi(BatteryComponent::Right, 100, BatteryStatus::NotCharging),
                bi(BatteryComponent::Left, 99, BatteryStatus::Charging),
                bi(BatteryComponent::Case, 17, BatteryStatus::NotCharging),
            ]
        );
    }

    #[test]
    fn battery_unknown_status_kept_and_unknown_component_skipped() {
        let pkt = [
            0x04, 0x00, 0x04, 0x00, 0x04, 0x00, 0x03, //
            0x08, 0x01, 0x32, 0x00, 0x01, // case status 0x00 (Unknown)
            0x10, 0x01, 0x32, 0x02, 0x01, // unknown component 0x10
            0x04, 0x01, 0x00, 0x04, 0x01, // left disconnected
        ];
        let b = parse_battery_info(payload(&pkt)).unwrap();
        assert_eq!(
            b,
            vec![
                bi(BatteryComponent::Case, 50, BatteryStatus::Unknown),
                bi(BatteryComponent::Left, 0, BatteryStatus::Disconnected),
            ]
        );
    }

    #[test]
    fn battery_headphone() {
        let pkt = [
            0x04, 0x00, 0x04, 0x00, 0x04, 0x00, 0x01, 0x01, 0x01, 0x40, 0x01, 0x01,
        ];
        let b = parse_battery_info(payload(&pkt)).unwrap();
        assert_eq!(
            b,
            vec![bi(BatteryComponent::Headphone, 64, BatteryStatus::Charging)]
        );
    }

    #[test]
    fn battery_rejects_bad_packets() {
        // count says 2 but only one record
        let short = [
            0x04, 0x00, 0x04, 0x00, 0x04, 0x00, 0x02, 0x02, 0x01, 0x64, 0x02, 0x01,
        ];
        assert!(parse_battery_info(payload(&short)).is_none());
        // bad spacer
        let spacer = [
            0x04, 0x00, 0x04, 0x00, 0x04, 0x00, 0x01, 0x02, 0x00, 0x64, 0x02, 0x01,
        ];
        assert!(parse_battery_info(payload(&spacer)).is_none());
        // implausible count
        let mut big = vec![0x04, 0x00, 0x04, 0x00, 0x04, 0x00, 0x09];
        big.extend(std::iter::repeat_n([0x02, 0x01, 0x10, 0x02, 0x01], 9).flatten());
        assert!(parse_battery_info(payload(&big)).is_none());
        // truncated / empty
        assert!(parse_battery_info(&[]).is_none());
        assert!(parse_battery_info(&[0x04, 0x00]).is_none());
        // wrong opcode
        assert!(parse_battery_info(&[0x05, 0x00, 0x00]).is_none());
    }

    #[test]
    fn merge_keeps_components_and_levels() {
        let mut acc = Vec::new();
        merge_battery_info(
            &mut acc,
            &[
                bi(BatteryComponent::Left, 80, BatteryStatus::NotCharging),
                bi(BatteryComponent::Right, 70, BatteryStatus::NotCharging),
                bi(BatteryComponent::Case, 50, BatteryStatus::Charging),
            ],
        );
        merge_battery_info(
            &mut acc,
            &[
                bi(BatteryComponent::Left, 0, BatteryStatus::Disconnected),
                bi(BatteryComponent::Right, 69, BatteryStatus::NotCharging),
            ],
        );
        assert_eq!(acc.len(), 3);
        let get = |c| acc.iter().find(|b| b.component == c).unwrap().clone();
        assert_eq!(
            get(BatteryComponent::Left),
            bi(BatteryComponent::Left, 80, BatteryStatus::Disconnected)
        );
        assert_eq!(get(BatteryComponent::Right).level, 69);
        assert_eq!(get(BatteryComponent::Case).level, 50);
    }

    #[test]
    fn ear_detection_bounds() {
        assert!(parse_ear_detection(&[0x04, 0x00, 0x04, 0x00, 0x06, 0x00]).is_none());
        assert!(parse_ear_detection(&[0x04, 0x00, 0x04, 0x00, 0x06, 0x00, 0x00]).is_none());
        let (p, s) =
            parse_ear_detection(&[0x04, 0x00, 0x04, 0x00, 0x06, 0x00, 0x00, 0x02]).unwrap();
        assert_eq!(p, EarDetectionStatus::InEar);
        assert_eq!(s, EarDetectionStatus::InCase);
    }

    fn info_payload(strings: &[&str]) -> Vec<u8> {
        // sub-header, binary prefix, NUL, a leading token that is discarded
        let mut v = vec![0x1D, 0x00, 0x00, 0x00, 0xAB, 0xCD, 0x00, 0x02, 0x00];
        for s in strings {
            v.extend_from_slice(s.as_bytes());
            v.push(0x00);
        }
        v
    }

    #[test]
    fn information_parse() {
        let p = info_payload(&["David's AirPods", "A3056", "Apple Inc.", "SERIAL1"]);
        let info = parse_information(&p).unwrap();
        assert_eq!(info.name, "David's AirPods");
        assert_eq!(info.model_number, "A3056");
        assert_eq!(info.manufacturer, "Apple Inc.");
        assert_eq!(info.serial_number, "SERIAL1");
        assert_eq!(info.version3, "");
        assert!(info.le_keys.irk.is_empty());
    }

    #[test]
    fn information_no_panic_on_empty() {
        // Previously `strings.remove(0)` panicked here.
        assert!(parse_information(&[0x1D, 0x00, 0x00, 0x00, 0x00, 0x00]).is_some());
        assert!(parse_information(&[0x1D, 0x00, 0x00, 0x00, 0xAA, 0xBB]).is_none());
        assert!(parse_information(&[0x1D]).is_none());
        let info = parse_information(&[0x1D, 0x00, 0x00, 0x00, 0x00, 0x00]).unwrap();
        assert_eq!(info.name, "");
    }

    #[test]
    fn proximity_keys_parse() {
        let mut p = vec![0x31, 0x00, 0x02];
        p.extend_from_slice(&[0x01, 0x00, 0x10, 0x00]);
        p.extend_from_slice(&[0x11; 16]);
        p.extend_from_slice(&[0x04, 0x00, 0x10, 0x00]);
        p.extend_from_slice(&[0x22; 16]);
        let keys = parse_proximity_keys(&p).unwrap();
        assert_eq!(keys, vec![(0x01, vec![0x11; 16]), (0x04, vec![0x22; 16])]);
        // truncated key data
        assert!(parse_proximity_keys(&p[..p.len() - 1]).is_none());
        assert!(parse_proximity_keys(&[0x31, 0x00, 0x01, 0x01, 0x00]).is_none());
    }

    #[test]
    fn mac_parsing() {
        assert_eq!(
            mac_to_reversed_bytes("AA:BB:CC:DD:EE:FF").unwrap(),
            [0xFF, 0xEE, 0xDD, 0xCC, 0xBB, 0xAA]
        );
        assert!(mac_to_reversed_bytes("").is_err());
        assert!(mac_to_reversed_bytes("ZZ:BB:CC:DD:EE:FF").is_err());
        assert!(mac_to_reversed_bytes("AA:BB").is_err());
    }

    fn le_keys_of(devices: &HashMap<String, DeviceData>, mac: &str) -> (String, String) {
        match &devices[mac].information {
            Some(DeviceInformation::AirPods(i)) => {
                (i.le_keys.irk.clone(), i.le_keys.enc_key.clone())
            }
            _ => panic!("no airpods info"),
        }
    }

    #[test]
    fn keys_then_information_keeps_keys() {
        let mac = "AA:BB:CC:DD:EE:FF";
        let mut devices = HashMap::new();
        merge_le_keys(&mut devices, mac, Some("aa"), Some("bb"));
        let info = parse_information(&info_payload(&["Pods", "A2698"])).unwrap();
        merge_information(&mut devices, mac, info);
        assert_eq!(le_keys_of(&devices, mac), ("aa".into(), "bb".into()));
        assert_eq!(devices[mac].name, "Pods");
        match &devices[mac].information {
            Some(DeviceInformation::AirPods(i)) => assert_eq!(i.model_number, "A2698"),
            _ => panic!(),
        }
    }

    #[test]
    fn information_then_keys_and_no_clobber() {
        let mac = "AA:BB:CC:DD:EE:FF";
        let mut devices = HashMap::new();
        let info = parse_information(&info_payload(&["Pods", "A2698"])).unwrap();
        merge_information(&mut devices, mac, info.clone());
        merge_le_keys(&mut devices, mac, Some("aa"), None);
        merge_le_keys(&mut devices, mac, None, Some("bb"));
        merge_le_keys(&mut devices, mac, Some(""), Some(""));
        assert_eq!(le_keys_of(&devices, mac), ("aa".into(), "bb".into()));
        // A later INFORMATION packet must not wipe keys.
        merge_information(&mut devices, mac, info);
        assert_eq!(le_keys_of(&devices, mac), ("aa".into(), "bb".into()));
    }

    #[tokio::test]
    async fn devices_file_roundtrip_atomic() {
        let dir = std::env::temp_dir().join(format!(
            "librepods-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let path = dir.join("nested").join("devices.json");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        let mac = "AA:BB:CC:DD:EE:FF";
        update_devices_file_at(&path, |d| merge_le_keys(d, mac, Some("aa"), Some("bb")))
            .await
            .unwrap();
        let info = parse_information(&info_payload(&["Pods", "A2698"])).unwrap();
        let devices = update_devices_file_at(&path, |d| merge_information(d, mac, info))
            .await
            .unwrap();
        assert_eq!(le_keys_of(&devices, mac), ("aa".into(), "bb".into()));
        let on_disk: HashMap<String, DeviceData> =
            serde_json::from_str(&tokio::fs::read_to_string(&path).await.unwrap()).unwrap();
        assert_eq!(le_keys_of(&on_disk, mac), ("aa".into(), "bb".into()));
        // no temp file left behind
        let mut entries = tokio::fs::read_dir(path.parent().unwrap()).await.unwrap();
        let mut names = Vec::new();
        while let Some(e) = entries.next_entry().await.unwrap() {
            names.push(e.file_name());
        }
        assert_eq!(names, vec![std::ffi::OsString::from("devices.json")]);
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn receive_packet_never_panics_on_short_input() {
        let m = AACPManager::new();
        for op in [
            opcodes::BATTERY_INFO,
            opcodes::CONTROL_COMMAND,
            opcodes::EAR_DETECTION,
            opcodes::CONVERSATION_AWARENESS,
            opcodes::STEM_PRESS,
            opcodes::AUDIO_SOURCE,
            opcodes::CONNECTED_DEVICES,
            opcodes::SMART_ROUTING_RESP,
            opcodes::EQ_DATA,
        ] {
            for extra in 0..12 {
                let mut p = vec![0x04, 0x00, 0x04, 0x00, op];
                p.extend(std::iter::repeat_n(0x05u8, extra));
                m.receive_packet(&p).await;
            }
        }
        // connected devices: count 1 but only 8 bytes after count (needs 10)
        let p = [
            0x04, 0x00, 0x04, 0x00, 0x2E, 0x00, 0x01, 1, 2, 3, 4, 5, 6, 7, 8,
        ];
        m.receive_packet(&p).await;
    }
}

#[cfg(test)]
#[path = "aacp_tests.rs"]
mod packet_tests;
