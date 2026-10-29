//! Picks the Bluetooth adapter the AirPods are paired on. A PC with a built-in chip and a
//! USB dongle has two adapters, and BlueZ's "default" one may not be where they live.

use bluer::{Adapter, Session};

/// BlueZ service UUID advertised by AirPods (and Beats) for the AACP control channel.
pub const AIRPODS_UUID: uuid::Uuid = uuid::uuid!("74ec2172-0bad-4d01-8f77-997b2be0722a");

/// What we know about one adapter when choosing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub name: String,
    pub powered: bool,
    pub has_airpods: bool,
}

/// The adapter to use: a powered one with paired AirPods, then any with paired AirPods,
/// then the default one, then the first one listed.
pub fn pick<'a>(candidates: &'a [Candidate], default: Option<&str>) -> Option<&'a str> {
    candidates
        .iter()
        .find(|c| c.has_airpods && c.powered)
        .or_else(|| candidates.iter().find(|c| c.has_airpods))
        .or_else(|| candidates.iter().find(|c| Some(c.name.as_str()) == default))
        .or_else(|| candidates.first())
        .map(|c| c.name.as_str())
}

async fn has_paired_airpods(adapter: &Adapter) -> bool {
    let Ok(addrs) = adapter.device_addresses().await else {
        return false;
    };
    for addr in addrs {
        let Ok(device) = adapter.device(addr) else {
            continue;
        };
        if device.is_paired().await.unwrap_or(false)
            && device
                .uuids()
                .await
                .ok()
                .flatten()
                .is_some_and(|u| u.contains(&AIRPODS_UUID))
        {
            return true;
        }
    }
    false
}

/// Every adapter BlueZ knows, with what `pick` needs.
pub async fn candidates(session: &Session) -> bluer::Result<Vec<Candidate>> {
    let mut out = Vec::new();
    for name in session.adapter_names().await? {
        let Ok(adapter) = session.adapter(&name) else {
            continue;
        };
        out.push(Candidate {
            powered: adapter.is_powered().await.unwrap_or(false),
            has_airpods: has_paired_airpods(&adapter).await,
            name,
        });
    }
    Ok(out)
}

/// The adapter the AirPods are paired on, falling back to the default adapter.
pub async fn airpods_adapter(session: &Session) -> bluer::Result<Adapter> {
    let all = candidates(session).await?;
    let default = session
        .default_adapter()
        .await
        .ok()
        .map(|a| a.name().to_string());
    match pick(&all, default.as_deref()) {
        Some(name) => session.adapter(name),
        None => session.default_adapter().await,
    }
}

#[cfg(test)]
#[path = "adapter_tests.rs"]
mod tests;
