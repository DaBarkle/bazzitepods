use crate::bluetooth::aacp::AACPEvent;

#[derive(Debug, Clone)]
pub enum BluetoothUIMessage {
    OpenWindow,
    DeviceConnected(String),        // mac
    DeviceDisconnected(String),     // mac
    AACPUIEvent(String, AACPEvent), // mac, event
    #[allow(dead_code)] // Reserved for Nothing ANC notifications.
    ATTNotification(String, u16, Vec<u8>), // mac, handle, data
}
