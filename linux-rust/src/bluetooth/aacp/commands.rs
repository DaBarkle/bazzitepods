//! Building and sending AACP requests: handshake, controls, mic and smart routing.

use super::*;

impl AACPManager {
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
        let data = [
            key_types.iter().fold(0u8, |acc, kt| acc | (*kt as u8)),
            0x00,
        ];
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

    /// The last value the AirPods reported for a control, if any.
    pub async fn control_value(&self, identifier: ControlCommandIdentifiers) -> Option<Vec<u8>> {
        self.state
            .lock()
            .await
            .control_command_status_list
            .iter()
            .find(|s| s.identifier == identifier)
            .map(|s| s.value.clone())
    }

    /// Asks the AirPods to start their high-quality mic stream (see `crate::mic`).
    pub async fn send_mic_start(&self) -> Result<()> {
        self.send_packet(&crate::mic::packets::MIC_START).await
    }

    pub async fn send_mic_stop(&self) -> Result<()> {
        self.send_packet(&crate::mic::packets::MIC_STOP).await
    }

    /// Where mic audio packets go; `None` drops them.
    pub async fn set_mic_sink(&self, tx: Option<mpsc::Sender<Vec<u8>>>) {
        self.state.lock().await.mic_tx = tx;
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

    #[allow(dead_code)]
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

    /// Asks the AirPods to keep these hosts, first one first. The packet EarPort's
    /// `aap_build_priority_list` builds (github.com/Anoryth/earport, GPL-3.0).
    pub async fn send_priority_list(&self, hosts: &[String]) -> Result<()> {
        let mut packet = vec![opcodes::PRIORITY_LIST, 0x00, hosts.len() as u8];
        for host in hosts {
            packet.extend_from_slice(&mac_to_reversed_bytes(host)?);
        }
        self.send_data_packet(&packet).await
    }

    pub async fn send_some_packet(&self) -> Result<()> {
        self.send_data_packet(&[0x29, 0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF])
            .await
    }
}
