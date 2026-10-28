#[derive(Debug, Clone)]
pub enum BluetoothUIMessage {
    OpenWindow,
    DeviceConnected(String),    // mac
    DeviceDisconnected(String), // mac
}
