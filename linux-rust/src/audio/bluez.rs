//! BlueZ side of the audio guard: A2DP profile (re)connection through
//! `org.bluez.Device1` and `org.bluez.MediaTransport1` state.

use bluer::{Address, Uuid};
use dbus::arg::RefArg;
use dbus::blocking::SyncConnection;
use dbus::blocking::stdintf::org_freedesktop_dbus::ObjectManager;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// A2DP Sink service class (the headphones' side), as used by `ConnectProfile`.
pub const A2DP_SINK_UUID: Uuid = Uuid::from_u128(0x0000110b_0000_1000_8000_00805f9b34fb);

const DBUS_TIMEOUT: Duration = Duration::from_secs(2);
const PROFILE_TIMEOUT: Duration = Duration::from_secs(15);

/// Device path fragment BlueZ uses for an address: `dev_AA_BB_CC_DD_EE_FF`.
pub fn device_path_fragment(mac: Address) -> String {
    format!("dev_{}", mac.to_string().to_uppercase().replace(':', "_"))
}

/// True when `path` is a child object (e.g. `/sep1/fd0`) of the device `mac`.
pub fn is_under_device(path: &str, mac: Address) -> bool {
    let frag = format!("/{}/", device_path_fragment(mac));
    path.contains(&frag)
}

pub trait BluezApi: Send + Sync + 'static {
    /// `State` of every `MediaTransport1` under the device (`idle`/`pending`/`active`).
    fn transport_states(
        &self,
        mac: Address,
    ) -> impl Future<Output = Result<Vec<String>, String>> + Send;
    fn connect_profile(
        &self,
        mac: Address,
        uuid: Uuid,
    ) -> impl Future<Output = Result<(), String>> + Send;
    fn disconnect_profile(
        &self,
        mac: Address,
        uuid: Uuid,
    ) -> impl Future<Output = Result<(), String>> + Send;
}

/// Real backend: bluer for `Device1`, a cached blocking system-bus connection for
/// the transport query (bluer does not expose `MediaTransport1`).
#[derive(Clone)]
pub struct BluerBackend {
    session: bluer::Session,
    system_bus: Arc<Mutex<Option<Arc<SyncConnection>>>>,
}

impl BluerBackend {
    pub fn new(session: bluer::Session) -> Self {
        BluerBackend {
            session,
            system_bus: Arc::new(Mutex::new(None)),
        }
    }

    async fn device(&self, mac: Address) -> Result<bluer::Device, String> {
        let names = self
            .session
            .adapter_names()
            .await
            .map_err(|e| format!("cannot list adapters: {e}"))?;
        for name in names {
            let Ok(adapter) = self.session.adapter(&name) else {
                continue;
            };
            if let Ok(addrs) = adapter.device_addresses().await
                && addrs.contains(&mac)
            {
                return adapter
                    .device(mac)
                    .map_err(|e| format!("cannot open device {mac}: {e}"));
            }
        }
        Err(format!("device {mac} not known to any adapter"))
    }

    fn bus(&self) -> Result<Arc<SyncConnection>, String> {
        let mut guard = self
            .system_bus
            .lock()
            .map_err(|_| "system bus lock poisoned".to_string())?;
        if let Some(c) = guard.as_ref() {
            return Ok(c.clone());
        }
        let conn = Arc::new(
            SyncConnection::new_system().map_err(|e| format!("cannot open system bus: {e}"))?,
        );
        *guard = Some(conn.clone());
        Ok(conn)
    }

    fn drop_bus(&self) {
        if let Ok(mut g) = self.system_bus.lock() {
            *g = None;
        }
    }
}

impl BluezApi for BluerBackend {
    async fn transport_states(&self, mac: Address) -> Result<Vec<String>, String> {
        let this = self.clone();
        let res = tokio::task::spawn_blocking(move || {
            let bus = this.bus()?;
            let proxy = bus.with_proxy("org.bluez", "/", DBUS_TIMEOUT);
            match proxy.get_managed_objects() {
                Ok(objects) => {
                    let mut states = Vec::new();
                    for (path, ifaces) in objects {
                        if !is_under_device(&path, mac) {
                            continue;
                        }
                        if let Some(props) = ifaces.get("org.bluez.MediaTransport1") {
                            let state = props
                                .get("State")
                                .and_then(|v| v.0.as_str())
                                .unwrap_or("unknown")
                                .to_string();
                            states.push(state);
                        }
                    }
                    Ok(states)
                }
                Err(e) => {
                    this.drop_bus();
                    Err(format!("GetManagedObjects failed: {e}"))
                }
            }
        })
        .await;
        res.map_err(|e| format!("blocking task failed: {e}"))?
    }

    async fn connect_profile(&self, mac: Address, uuid: Uuid) -> Result<(), String> {
        let device = self.device(mac).await?;
        match tokio::time::timeout(PROFILE_TIMEOUT, device.connect_profile(&uuid)).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(format!("ConnectProfile({uuid}) failed: {e}")),
            Err(_) => Err(format!("ConnectProfile({uuid}) timed out")),
        }
    }

    async fn disconnect_profile(&self, mac: Address, uuid: Uuid) -> Result<(), String> {
        let device = self.device(mac).await?;
        match tokio::time::timeout(PROFILE_TIMEOUT, device.disconnect_profile(&uuid)).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(format!("DisconnectProfile({uuid}) failed: {e}")),
            Err(_) => Err(format!("DisconnectProfile({uuid}) timed out")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transport_path_matching() {
        let mac: Address = "a0:b1:c2:d3:e4:f5".parse().unwrap();
        assert!(is_under_device(
            "/org/bluez/hci0/dev_A0_B1_C2_D3_E4_F5/sep1/fd0",
            mac
        ));
        assert!(!is_under_device(
            "/org/bluez/hci0/dev_A0_B1_C2_D3_E4_F5",
            mac
        ));
        assert!(!is_under_device(
            "/org/bluez/hci0/dev_A0_B1_C2_D3_E4_F6/sep1/fd0",
            mac
        ));
        assert_eq!(
            A2DP_SINK_UUID.to_string(),
            "0000110b-0000-1000-8000-00805f9b34fb"
        );
    }
}
