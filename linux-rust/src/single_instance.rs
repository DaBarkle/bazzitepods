//! One Kase per session. A second launch (app menu click while the app sits in the tray)
//! asks the running copy to show its window and exits, instead of starting a duplicate that
//! would fight over the Bluetooth control channel.

use crate::ui::messages::BluetoothUIMessage;
use dbus::blocking::Connection;
use dbus::channel::MatchingReceiver;
use dbus::message::MatchRule;
use log::{debug, info};
use std::time::Duration;
use tokio::sync::mpsc::UnboundedSender;

const BUS_NAME: &str = "io.github.dabarkle.Kase";
const PATH: &str = "/io/github/dabarkle/Kase";
const IFACE: &str = "io.github.dabarkle.Kase";

pub enum Instance {
    /// We are the only instance; keep running.
    Primary,
    /// Another instance was asked to show its window; exit now.
    Secondary,
}

/// Claim the session-bus name. If another copy owns it, ask it to open its window.
/// Without a session bus we simply run (better a duplicate than no app).
pub fn claim(ui_tx: UnboundedSender<BluetoothUIMessage>, show_window: bool) -> Instance {
    let conn = match Connection::new_session() {
        Ok(c) => c,
        Err(e) => {
            debug!("No session bus for single-instance check: {e}");
            return Instance::Primary;
        }
    };
    use dbus::blocking::stdintf::org_freedesktop_dbus::RequestNameReply;
    match conn.request_name(BUS_NAME, false, false, true) {
        Ok(RequestNameReply::PrimaryOwner) | Ok(RequestNameReply::AlreadyOwner) => {}
        Ok(_) => {
            if show_window {
                let proxy = conn.with_proxy(BUS_NAME, PATH, Duration::from_secs(2));
                let r: Result<(), dbus::Error> = proxy.method_call(IFACE, "Activate", ());
                if let Err(e) = r {
                    debug!("Could not activate running instance: {e}");
                }
            }
            info!("Kase is already running");
            return Instance::Secondary;
        }
        Err(e) => {
            debug!("Could not claim {BUS_NAME}: {e}");
            return Instance::Primary;
        }
    }

    std::thread::Builder::new()
        .name("single-instance".into())
        .spawn(move || {
            let tx = ui_tx;
            conn.start_receive(
                MatchRule::new_method_call(),
                Box::new(move |msg, conn| {
                    if msg.interface().as_deref() == Some(IFACE) {
                        match msg.member().as_deref() {
                            Some("Activate") => {
                                let _ = tx.send(BluetoothUIMessage::OpenWindow);
                            }
                            // From our KWin script (see fullscreen_watch.rs).
                            Some("SetFullscreen") => {
                                if let Ok(active) = msg.read1::<bool>() {
                                    crate::overlay_feed::set_fullscreen(active);
                                }
                            }
                            _ => {}
                        }
                    }
                    if !msg.get_no_reply() {
                        use dbus::channel::Sender;
                        let _ = conn.send(msg.method_return());
                    }
                    true
                }),
            );
            loop {
                if conn.process(Duration::from_secs(60)).is_err() {
                    break;
                }
            }
        })
        .ok();
    Instance::Primary
}
