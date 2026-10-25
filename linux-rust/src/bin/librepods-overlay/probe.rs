//! Checks the Wayland registry before iced_layershell starts, because
//! layershellev unwraps the `zwlr_layer_shell_v1` bind and would panic on
//! compositors without it (GNOME/Mutter).

use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_registry;
use wayland_client::{Connection, Dispatch, QueueHandle};

/// Globals layershellev needs: layer shell (unwrapped), plus the ones it binds with `?`.
pub const REQUIRED: &[&str] = &[
    "zwlr_layer_shell_v1",
    "wl_compositor",
    "wl_shm",
    "wl_seat",
    "xdg_wm_base",
    "zxdg_output_manager_v1",
];

#[derive(Debug, PartialEq, Eq)]
pub enum Probe {
    Supported,
    NoDisplay(String),
    Missing(Vec<&'static str>),
}

struct State;

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
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

pub fn missing_globals(interfaces: &[String]) -> Vec<&'static str> {
    REQUIRED
        .iter()
        .copied()
        .filter(|req| !interfaces.iter().any(|i| i == req))
        .collect()
}

pub fn probe() -> Probe {
    if std::env::var_os("WAYLAND_DISPLAY").is_none() && std::env::var_os("WAYLAND_SOCKET").is_none()
    {
        return Probe::NoDisplay("WAYLAND_DISPLAY is not set".into());
    }
    let conn = match Connection::connect_to_env() {
        Ok(c) => c,
        Err(e) => return Probe::NoDisplay(e.to_string()),
    };
    // registry_queue_init does the initial roundtrip.
    let (globals, _queue) = match registry_queue_init::<State>(&conn) {
        Ok(g) => g,
        Err(e) => return Probe::NoDisplay(e.to_string()),
    };
    let interfaces: Vec<String> = globals
        .contents()
        .with_list(|l| l.iter().map(|g| g.interface.clone()).collect());
    let missing = missing_globals(&interfaces);
    if missing.is_empty() {
        Probe::Supported
    } else {
        Probe::Missing(missing)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_missing_layer_shell() {
        let mutter: Vec<String> = [
            "wl_compositor",
            "wl_shm",
            "wl_seat",
            "xdg_wm_base",
            "zxdg_output_manager_v1",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(missing_globals(&mutter), vec!["zwlr_layer_shell_v1"]);
        let mut kwin = mutter.clone();
        kwin.push("zwlr_layer_shell_v1".into());
        assert!(missing_globals(&kwin).is_empty());
    }
}
