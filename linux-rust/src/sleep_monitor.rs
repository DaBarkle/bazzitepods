//! Watches logind for suspend/resume so device sessions can be restarted after waking.
//!
//! Bluetooth links rarely survive suspend cleanly: the AirPods drop, the L2CAP socket may stay
//! half-open, and PipeWire recreates the card. Restarting sessions on resume avoids all of that.

use crate::Kick;
use dbus::message::MatchRule;
use log::{debug, info};
use tokio::sync::mpsc::UnboundedSender;

pub fn spawn(kick_tx: UnboundedSender<Kick>) {
    tokio::spawn(async move {
        if let Err(e) = run(kick_tx).await {
            // Not fatal: without logind (or without the Flatpak permission) we simply rely on
            // BlueZ disconnect events and the periodic sweep.
            info!("Sleep monitor unavailable: {e}");
        }
    });
}

async fn run(kick_tx: UnboundedSender<Kick>) -> Result<(), dbus::Error> {
    let (resource, conn) = dbus_tokio::connection::new_system_sync()?;
    // The resource future only ends when the connection is lost.
    let lost = tokio::spawn(async move { resource.await });

    let rule = MatchRule::new_signal("org.freedesktop.login1.Manager", "PrepareForSleep");
    let tx = kick_tx.clone();
    let _match = conn
        .add_match(rule)
        .await?
        .cb(move |_, (going_to_sleep,): (bool,)| {
            if going_to_sleep {
                debug!("System is going to sleep");
            } else {
                let _ = tx.send(Kick::Resumed);
            }
            true
        });
    let err = lost.await;
    debug!("logind connection ended: {err:?}");
    Ok(())
}
