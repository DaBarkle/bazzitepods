//! Decoding incoming AACP packets into state updates and events.

use super::*;

impl AACPManager {
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

        if opcode == crate::mic::packets::MIC_STREAM_OPCODE {
            self.receive_mic_packet(packet).await;
            return;
        }

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
                    debug!(
                        "Ignoring short or unknown Ear Detection packet: {}",
                        hex::encode(packet)
                    );
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
                let result = crate::store::update_devices(|devices| {
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
                let result = crate::store::update_devices(|devices| {
                    merge_le_keys(devices, &mac_str, irk.as_deref(), enc_key.as_deref());
                })
                .await;
                match result {
                    Ok(devices) => self.state.lock().await.devices = devices,
                    Err(e) => error!("Failed to save devices: {}", e),
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

    async fn receive_mic_packet(&self, packet: &[u8]) {
        if !crate::mic::packets::is_audio(packet) {
            info!("Received Mic Stream message: {}", hex::encode(packet));
            return;
        }
        let state = self.state.lock().await;
        if let Some(tx) = &state.mic_tx {
            // Never wait on the decoder here: battery and ear reports share this channel.
            if tx.try_send(packet.to_vec()).is_err() {
                debug!("Mic audio packet dropped (decoder busy or gone)");
            }
        }
    }
}
