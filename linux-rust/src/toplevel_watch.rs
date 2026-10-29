//! Fullscreen detection on wlroots-style compositors (Sway, Hyprland, niri, ...), the
//! counterpart of the KWin script in `fullscreen_watch.rs`. The compositor lists its windows
//! and their state over `zwlr_foreign_toplevel_management_v1`; nothing here looks at another
//! process. Where the protocol is missing (KWin, GNOME) this ends quietly.

use log::{debug, info};
use std::collections::HashMap;
use wayland_client::backend::ObjectId;
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_registry;
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, event_created_child};
use wayland_protocols_wlr::foreign_toplevel::v1::client::zwlr_foreign_toplevel_handle_v1::{
    self as handle, ZwlrForeignToplevelHandleV1,
};
use wayland_protocols_wlr::foreign_toplevel::v1::client::zwlr_foreign_toplevel_manager_v1::{
    self as manager, ZwlrForeignToplevelManagerV1,
};

/// One window's state: (activated, fullscreen).
type Flags = (bool, bool);

#[derive(Default)]
struct Watch {
    /// State sent since the window's last `done`.
    pending: HashMap<ObjectId, Flags>,
    windows: HashMap<ObjectId, Flags>,
}

/// Reads the protocol's state array (native-endian u32s): 2 = activated, 3 = fullscreen.
pub fn parse_state(bytes: &[u8]) -> Flags {
    let states: Vec<u32> = bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| u32::from_ne_bytes(*c))
        .collect();
    (states.contains(&2), states.contains(&3))
}

/// Whether the active window is fullscreen.
pub fn fullscreen_in_front<'a>(windows: impl IntoIterator<Item = &'a Flags>) -> bool {
    windows.into_iter().any(|&(active, full)| active && full)
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for Watch {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwlrForeignToplevelManagerV1, ()> for Watch {
    fn event(
        _: &mut Self,
        _: &ZwlrForeignToplevelManagerV1,
        _: manager::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }

    event_created_child!(Watch, ZwlrForeignToplevelManagerV1, [
        manager::EVT_TOPLEVEL_OPCODE => (ZwlrForeignToplevelHandleV1, ()),
    ]);
}

impl Dispatch<ZwlrForeignToplevelHandleV1, ()> for Watch {
    fn event(
        watch: &mut Self,
        window: &ZwlrForeignToplevelHandleV1,
        event: handle::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            handle::Event::State { state } => {
                watch.pending.insert(window.id(), parse_state(&state));
            }
            handle::Event::Done => {
                if let Some(flags) = watch.pending.remove(&window.id()) {
                    watch.windows.insert(window.id(), flags);
                }
            }
            handle::Event::Closed => {
                watch.pending.remove(&window.id());
                watch.windows.remove(&window.id());
                window.destroy();
            }
            _ => {}
        }
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let conn = Connection::connect_to_env()?;
    let (globals, mut queue) = registry_queue_init::<Watch>(&conn)?;
    let qh = queue.handle();
    let _manager: ZwlrForeignToplevelManagerV1 = globals
        .bind(&qh, 1..=3, ())
        .map_err(|_| "the compositor has no foreign-toplevel management")?;
    info!("Fullscreen detection active (wlr foreign toplevel)");
    let mut watch = Watch::default();
    let mut last = None;
    loop {
        queue.blocking_dispatch(&mut watch)?;
        let now = fullscreen_in_front(watch.windows.values());
        if last != Some(now) {
            last = Some(now);
            crate::overlay_feed::set_fullscreen(now);
        }
    }
}

/// Watches in the background. Safe to call anywhere: ends quietly without the protocol.
pub fn spawn() {
    std::thread::Builder::new()
        .name("toplevel-watch".into())
        .spawn(|| {
            if let Err(e) = run() {
                debug!("wlr fullscreen detection unavailable: {e}");
            }
        })
        .ok();
}

#[cfg(test)]
#[path = "toplevel_watch_tests.rs"]
mod tests;
