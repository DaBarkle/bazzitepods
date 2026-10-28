#[derive(Debug, Clone)]
pub enum BluetoothUIMessage {
    OpenWindow,
    /// AirPods finished connecting: the window reloads `devices.json` for serials and firmware.
    DeviceConnected,
}
