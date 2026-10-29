//! Watches logind for suspend/resume so device sessions can be restarted after waking, and
//! for the user coming back to the PC (screen unlocked, idle ended), which is the moment to
//! bring the AirPods back.
//!
//! Bluetooth links rarely survive suspend cleanly: the AirPods drop, the L2CAP socket may stay
//! half-open, and PipeWire recreates the card. Restarting sessions on resume avoids all of that.

use crate::Kick;
use dbus::arg::{PropMap, prop_cast};
use dbus::message::MatchRule;
use log::{debug, info};
use tokio::sync::mpsc::UnboundedSender;

pub fn spawn(kick_tx: UnboundedSender<Kick>) {
    // Not fatal while it's down: without logind (or without the Flatpak permission) we
    // rely on BlueZ disconnect events and the periodic sweep.
    tokio::spawn(crate::disconnect_watch::keep_watching(
        "Sleep monitor",
        kick_tx.clone(),
        run,
    ));
    // KDE never sets logind's IdleHint; its screen saver (and GNOME's) say so on the
    // session bus instead.
    tokio::spawn(crate::disconnect_watch::keep_watching(
        "Screen saver monitor",
        kick_tx,
        run_screensaver,
    ));
}

/// Screen savers that announce `ActiveChanged(false)` when the screen wakes or unlocks.
const SCREENSAVERS: [&str; 2] = ["org.freedesktop.ScreenSaver", "org.gnome.ScreenSaver"];

async fn run_screensaver(kick_tx: UnboundedSender<Kick>) -> Result<(), dbus::Error> {
    let (resource, conn) = dbus_tokio::connection::new_session_sync()?;
    let lost = tokio::spawn(resource);
    let mut matches = Vec::new();
    for iface in SCREENSAVERS {
        let tx = kick_tx.clone();
        let m = conn
            .add_match(MatchRule::new_signal(iface, "ActiveChanged"))
            .await?
            .cb(move |_, (active,): (bool,)| {
                if !active {
                    info!("Screen saver ended; checking the AirPods");
                    let _ = tx.send(Kick::UserActive);
                }
                true
            });
        matches.push(m);
    }
    let err = lost.await;
    debug!("session bus connection ended: {err:?}");
    Ok(())
}

async fn run(kick_tx: UnboundedSender<Kick>) -> Result<(), dbus::Error> {
    let (resource, conn) = dbus_tokio::connection::new_system_sync()?;
    // The resource future only ends when the connection is lost.
    let lost = tokio::spawn(resource);

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

    // KDE tells logind when the screen locks and unlocks, and when the session goes idle.
    // Either ending means someone sat down at the PC: try the AirPods straight away instead
    // of waiting for the next background attempt or for adverts this PC may not recognise.
    let mut rule = MatchRule::new_signal("org.freedesktop.DBus.Properties", "PropertiesChanged");
    rule.path = Some("/org/freedesktop/login1/session".into());
    rule.path_is_namespace = true;
    let tx = kick_tx.clone();
    let _session_match = conn.add_match(rule).await?.cb(
        move |_, (interface, changed, _): (String, PropMap, Vec<String>)| {
            if user_came_back(&interface, &changed) {
                info!("You are back at the PC; checking the AirPods");
                let _ = tx.send(Kick::UserActive);
            }
            true
        },
    );
    let err = lost.await;
    debug!("logind connection ended: {err:?}");
    Ok(())
}

/// A logind session property change that means the user is at the PC again.
fn user_came_back(interface: &str, changed: &PropMap) -> bool {
    interface == "org.freedesktop.login1.Session"
        && ["LockedHint", "IdleHint"]
            .iter()
            .any(|key| prop_cast::<bool>(changed, key) == Some(&false))
}

#[cfg(test)]
mod tests {
    use super::*;
    use dbus::arg::{RefArg, Variant};

    fn props(pairs: &[(&str, bool)]) -> PropMap {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), Variant(Box::new(*v) as Box<dyn RefArg>)))
            .collect()
    }

    #[test]
    fn unlocking_or_ending_idle_means_the_user_is_back() {
        let session = "org.freedesktop.login1.Session";
        assert!(user_came_back(session, &props(&[("LockedHint", false)])));
        assert!(user_came_back(session, &props(&[("IdleHint", false)])));
        assert!(!user_came_back(session, &props(&[("LockedHint", true)])));
        assert!(!user_came_back(session, &props(&[("IdleHint", true)])));
        assert!(!user_came_back(session, &props(&[("Active", false)])));
        assert!(!user_came_back(
            "org.freedesktop.login1.User",
            &props(&[("IdleHint", false)])
        ));
    }
}
