//! Tells the overlay whether a fullscreen window is in front, so it can turn translucent
//! over games and videos and stay solid on the desktop.
//!
//! The answer comes from the compositor only: a tiny KWin script reports the active
//! window's fullscreen state back to us over D-Bus (see `SetFullscreen` in
//! `single_instance.rs`). Nothing here looks at, or talks to, any other process, so
//! there is no anti-cheat exposure. On compositors other than KWin this does nothing
//! and the overlay simply stays solid.

use crate::utils::get_devices_path;
use dbus::blocking::Connection;
use log::{debug, info};
use std::time::Duration;

const PLUGIN: &str = "kase-fullscreen";
/// The script's name before the app was renamed to Kase (0.5.0 and earlier).
const OLD_PLUGIN: &str = "bazzitepods-fullscreen";

/// KWin script for Plasma 6 and Plasma 5 (which names windows "clients": clientList,
/// activeClient, clientActivated, geometry). A window counts as fullscreen when it is in
/// fullscreen mode, or when it is borderless and covers its whole screen (many games do that).
const SCRIPT: &str = r#"
var six = typeof workspace.windowList === "function";
var last = null;
function geometry(w) { return w.frameGeometry || w.geometry; }
function isFull(w) {
    if (!w || w.desktopWindow || w.dock || w.specialWindow) return false;
    if (w.fullScreen) return true;
    var area = workspace.clientArea(KWin.FullScreenArea, w);
    var g = geometry(w);
    return w.noBorder && g.x <= area.x && g.y <= area.y
        && g.width >= area.width && g.height >= area.height;
}
function report() {
    var full = isFull(six ? workspace.activeWindow : workspace.activeClient);
    if (full === last) return;
    last = full;
    callDBus("io.github.dabarkle.Kase", "/io/github/dabarkle/Kase",
             "io.github.dabarkle.Kase", "SetFullscreen", full);
}
function hook(w) {
    w.fullScreenChanged.connect(report);
    (w.frameGeometryChanged || w.geometryChanged).connect(report);
}
(six ? workspace.windowList() : workspace.clientList()).forEach(hook);
(six ? workspace.windowAdded : workspace.clientAdded).connect(hook);
(six ? workspace.windowActivated : workspace.clientActivated).connect(report);
(six ? workspace.windowRemoved : workspace.clientRemoved).connect(report);
report();
"#;

/// Installs the KWin script in the background. Safe to call when KWin isn't running.
pub fn spawn() {
    std::thread::Builder::new()
        .name("fullscreen-watch".into())
        .spawn(|| {
            if let Err(e) = install() {
                info!("Fullscreen detection unavailable (not KWin?): {e}");
            }
        })
        .ok();
}

fn install() -> Result<(), Box<dyn std::error::Error>> {
    // The script file must be readable by KWin on the host. Our data dir is the same
    // path inside and outside the Flatpak sandbox (~/.var/app/<id>/data/...).
    let path = get_devices_path().with_file_name("fullscreen.js");
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, SCRIPT)?;

    let conn = Connection::new_session()?;
    let scripting = conn.with_proxy("org.kde.KWin", "/Scripting", Duration::from_secs(3));
    // A copy left behind by a previous run (crash, update) would report twice.
    for name in [PLUGIN, OLD_PLUGIN] {
        let unloaded: Result<(bool,), dbus::Error> =
            scripting.method_call("org.kde.kwin.Scripting", "unloadScript", (name,));
        debug!("previous fullscreen script {name} unloaded: {unloaded:?}");
    }
    let (id,): (i32,) = scripting.method_call(
        "org.kde.kwin.Scripting",
        "loadScript",
        (path.to_string_lossy().as_ref(), PLUGIN),
    )?;
    if id < 0 {
        return Err("KWin refused the script".into());
    }
    // Plasma 6 uses /Scripting/Script<N>; Plasma 5 used /<N>.
    let mut last_err = None;
    for obj in [format!("/Scripting/Script{id}"), format!("/{id}")] {
        let script = conn.with_proxy("org.kde.KWin", obj, Duration::from_secs(3));
        match script.method_call::<(), _, _, _>("org.kde.kwin.Script", "run", ()) {
            Ok(()) => {
                info!("Fullscreen detection active (KWin script {id})");
                return Ok(());
            }
            Err(e) => last_err = Some(e),
        }
    }
    Err(last_err.map(|e| e.to_string()).unwrap_or_default().into())
}
