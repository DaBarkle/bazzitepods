//! Start Kase with the user's session.
//!
//! Inside Flatpak we ask the XDG Background portal
//! (`org.freedesktop.portal.Background.RequestBackground`), which writes or removes
//! `~/.config/autostart/<app-id>.desktop` on the host for us; no filesystem permission is needed.
//! Outside Flatpak (a plain `cargo run` / AppImage / distro package) we manage that
//! file ourselves.
//!
//! Either way the app is launched as `kase --start-minimized`, i.e. tray and background
//! work only, with the main window hidden.
//!
//! Nothing in here panics: every failure is reported as `Err(String)` for the caller to log.

// Wired up by the integrator (call sites live in main.rs / ui); until then nothing uses it.

use dbus::Path as DbusPath;
use dbus::arg::{PropMap, RefArg, Variant};
use dbus::blocking::Connection;
use dbus::message::MatchRule;
use log::{debug, info, warn};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const APP_ID: &str = "io.github.dabarkle.Kase";
const START_MINIMIZED_FLAG: &str = "--start-minimized";
const PORTAL_BUS_NAME: &str = "org.freedesktop.portal.Desktop";
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
const BACKGROUND_IFACE: &str = "org.freedesktop.portal.Background";
const REQUEST_IFACE: &str = "org.freedesktop.portal.Request";
/// The method call itself should return immediately.
const METHOD_TIMEOUT: Duration = Duration::from_secs(10);
/// GNOME may show the user a permission dialog before the Response arrives.
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(60);

/// Enable or disable starting Kase (minimized) at login.
///
/// Safe to call on every start: the portal and the fallback are both idempotent.
pub async fn ensure_autostart(enable: bool) -> Result<(), String> {
    let joined = tokio::task::spawn_blocking(move || {
        if running_in_flatpak() {
            request_background_portal(enable)
        } else {
            let dir = autostart_dir(
                std::env::var_os("XDG_CONFIG_HOME"),
                std::env::var_os("HOME"),
            )?;
            // AppImages run from a temporary mount; $APPIMAGE is the stable path.
            let exe = match std::env::var_os("APPIMAGE") {
                Some(p) => std::path::PathBuf::from(p),
                None => std::env::current_exe()
                    .map_err(|e| format!("cannot determine the current executable: {e}"))?,
            };
            set_autostart_file(&dir, &exe, enable)
        }
    })
    .await;

    match joined {
        Ok(result) => result,
        Err(e) => Err(format!("autostart task failed: {e}")),
    }
}

/// True when running inside a Flatpak sandbox.
pub fn running_in_flatpak() -> bool {
    Path::new("/.flatpak-info").exists()
}

// ---------------------------------------------------------------------------
// XDG Background portal
// ---------------------------------------------------------------------------

fn request_background_portal(enable: bool) -> Result<(), String> {
    let conn =
        Connection::new_session().map_err(|e| format!("cannot connect to the session bus: {e}"))?;

    let token = handle_token();
    let sender = conn.unique_name().to_string();
    let expected_path = request_path(&sender, &token);

    // Subscribe to the Response signal *before* calling the method, so it cannot be missed.
    let response: Arc<Mutex<Option<(u32, PropMap)>>> = Arc::new(Mutex::new(None));
    add_response_match(&conn, &expected_path, Arc::clone(&response))?;

    let mut options = PropMap::new();
    options.insert("handle_token".into(), variant(token.clone()));
    options.insert(
        "reason".into(),
        variant(String::from(
            "Show AirPods battery and keep them working in the background",
        )),
    );
    options.insert("autostart".into(), variant(enable));
    options.insert(
        "commandline".into(),
        variant(vec![
            String::from("kase"),
            String::from(START_MINIMIZED_FLAG),
        ]),
    );
    options.insert("dbus-activatable".into(), variant(false));

    let proxy = conn.with_proxy(PORTAL_BUS_NAME, PORTAL_PATH, METHOD_TIMEOUT);
    let (handle,): (DbusPath<'static>,) = proxy
        .method_call(BACKGROUND_IFACE, "RequestBackground", ("", options))
        .map_err(|e| format!("Background portal call failed: {e}"))?;

    // Very old portals ignore handle_token and return a different path.
    if &*handle != expected_path.as_str() {
        debug!("Background portal returned unexpected request path {handle}");
        add_response_match(&conn, &handle, Arc::clone(&response))?;
    }

    let deadline = Instant::now() + RESPONSE_TIMEOUT;
    loop {
        let got = match response.lock() {
            Ok(mut guard) => guard.take(),
            Err(poisoned) => poisoned.into_inner().take(),
        };
        if let Some((code, results)) = got {
            return interpret_response(enable, code, &results);
        }
        if Instant::now() >= deadline {
            return Err("timed out waiting for the Background portal response".into());
        }
        conn.process(Duration::from_millis(250))
            .map_err(|e| format!("D-Bus error while waiting for the portal: {e}"))?;
    }
}

fn add_response_match(
    conn: &Connection,
    path: &str,
    slot: Arc<Mutex<Option<(u32, PropMap)>>>,
) -> Result<(), String> {
    let path =
        DbusPath::new(path.to_string()).map_err(|e| format!("invalid portal request path: {e}"))?;
    let rule = MatchRule::new_signal(REQUEST_IFACE, "Response").with_path(path);
    conn.add_match(rule, move |(code, results): (u32, PropMap), _, _| {
        match slot.lock() {
            Ok(mut guard) => *guard = Some((code, results)),
            Err(poisoned) => *poisoned.into_inner() = Some((code, results)),
        }
        false // one response per request: drop the match
    })
    .map(|_| ())
    .map_err(|e| format!("cannot subscribe to the portal response: {e}"))
}

fn interpret_response(enable: bool, code: u32, results: &PropMap) -> Result<(), String> {
    match code {
        0 => {
            let autostart = results
                .get("autostart")
                .and_then(|v| v.0.as_u64())
                .map(|v| v != 0);
            let background = results
                .get("background")
                .and_then(|v| v.0.as_u64())
                .map(|v| v != 0);
            debug!("Background portal: background={background:?} autostart={autostart:?}");
            if enable && autostart == Some(false) {
                warn!("Background portal granted the request but did not enable autostart");
                return Err("autostart was not enabled by the Background portal".into());
            }
            info!(
                "Autostart {} via the Background portal",
                if enable { "enabled" } else { "disabled" }
            );
            Ok(())
        }
        1 => Err("the user denied the background/autostart request".into()),
        _ => Err(format!(
            "the Background portal request failed (response {code})"
        )),
    }
}

fn variant<T: RefArg + 'static>(value: T) -> Variant<Box<dyn RefArg>> {
    Variant(Box::new(value))
}

/// Unique token for this request; only `[A-Za-z0-9_]` is allowed.
fn handle_token() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    format!("kase_{}_{}", std::process::id(), nanos)
}

/// `/org/freedesktop/portal/desktop/request/SENDER/TOKEN`, where SENDER is our unique bus name
/// without the leading ':' and with '.' replaced by '_'.
fn request_path(unique_name: &str, token: &str) -> String {
    let sender = unique_name.trim_start_matches(':').replace('.', "_");
    format!("{PORTAL_PATH}/request/{sender}/{token}")
}

// ---------------------------------------------------------------------------
// Non-Flatpak fallback: ~/.config/autostart/<app-id>.desktop
// ---------------------------------------------------------------------------

fn autostart_dir(
    xdg_config_home: Option<OsString>,
    home: Option<OsString>,
) -> Result<PathBuf, String> {
    // Per the XDG base dir spec, a relative XDG_CONFIG_HOME is invalid and must be ignored.
    let config = match xdg_config_home.map(PathBuf::from) {
        Some(p) if p.is_absolute() => p,
        _ => match home.map(PathBuf::from) {
            Some(h) if h.is_absolute() => h.join(".config"),
            _ => return Err("neither XDG_CONFIG_HOME nor HOME is set".into()),
        },
    };
    Ok(config.join("autostart"))
}

fn autostart_file(dir: &Path) -> PathBuf {
    dir.join(format!("{APP_ID}.desktop"))
}

fn set_autostart_file(dir: &Path, exe: &Path, enable: bool) -> Result<(), String> {
    let file = autostart_file(dir);
    if !enable {
        return match std::fs::remove_file(&file) {
            Ok(()) => {
                info!("Autostart disabled: removed {}", file.display());
                Ok(())
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("cannot remove {}: {e}", file.display())),
        };
    }

    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let contents = desktop_entry(exe);
    if std::fs::read_to_string(&file).is_ok_and(|existing| existing == contents) {
        return Ok(());
    }
    // Write to a temporary file and rename, so a crash never leaves a half-written entry.
    let tmp = dir.join(format!(".{APP_ID}.desktop.tmp"));
    std::fs::write(&tmp, contents.as_bytes())
        .map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &file).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("cannot create {}: {e}", file.display())
    })?;
    info!("Autostart enabled: wrote {}", file.display());
    Ok(())
}

fn desktop_entry(exe: &Path) -> String {
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=Kase\n\
         Comment=AirPods battery, controls and island overlay for KDE Plasma\n\
         Exec={} {START_MINIMIZED_FLAG}\n\
         Icon={APP_ID}\n\
         Terminal=false\n\
         NoDisplay=true\n\
         StartupNotify=false\n\
         X-GNOME-Autostart-enabled=true\n\
         X-KDE-autostart-phase=2\n",
        quote_exec_arg(&exe.to_string_lossy())
    )
}

/// Quote an argument for a desktop entry `Exec=` key (Desktop Entry spec, "The Exec key").
fn quote_exec_arg(arg: &str) -> String {
    const RESERVED: &[char] = &[
        ' ', '\t', '\n', '"', '\'', '\\', '>', '<', '~', '|', '&', ';', '$', '*', '?', '#', '(',
        ')', '`', '%',
    ];
    let escaped = arg.replace('%', "%%");
    if !arg.contains(RESERVED) {
        return escaped;
    }
    let mut out = String::with_capacity(escaped.len() + 2);
    out.push('"');
    for c in escaped.chars() {
        if matches!(c, '"' | '`' | '$' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh directory under the system temp dir, used as XDG_CONFIG_HOME.
    fn temp_config_home(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "kase-autostart-test-{}-{}-{name}",
            std::process::id(),
            handle_token()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    #[test]
    fn enable_disable_with_temp_xdg_config_home() {
        let config_home = temp_config_home("roundtrip");
        let dir = autostart_dir(Some(config_home.clone().into_os_string()), None).expect("dir");
        assert_eq!(dir, config_home.join("autostart"));

        let exe = Path::new("/opt/My Apps/bin/kase");
        set_autostart_file(&dir, exe, true).expect("enable");
        let file = dir.join("io.github.dabarkle.Kase.desktop");
        let text = std::fs::read_to_string(&file).expect("read entry");
        assert!(text.starts_with("[Desktop Entry]\n"));
        assert!(text.contains("Exec=\"/opt/My Apps/bin/kase\" --start-minimized\n"));
        assert!(text.contains("X-GNOME-Autostart-enabled=true\n"));

        // Idempotent.
        set_autostart_file(&dir, exe, true).expect("enable again");
        assert_eq!(std::fs::read_to_string(&file).expect("read again"), text);

        set_autostart_file(&dir, exe, false).expect("disable");
        assert!(!file.exists());
        // Disabling twice is fine.
        set_autostart_file(&dir, exe, false).expect("disable again");

        let _ = std::fs::remove_dir_all(&config_home);
    }

    #[test]
    fn autostart_dir_falls_back_to_home() {
        assert_eq!(
            autostart_dir(None, Some("/home/u".into())).expect("dir"),
            PathBuf::from("/home/u/.config/autostart")
        );
        // Relative XDG_CONFIG_HOME is ignored.
        assert_eq!(
            autostart_dir(Some("rel".into()), Some("/home/u".into())).expect("dir"),
            PathBuf::from("/home/u/.config/autostart")
        );
        assert!(autostart_dir(None, None).is_err());
    }

    #[test]
    fn exec_quoting() {
        assert_eq!(quote_exec_arg("/usr/bin/kase"), "/usr/bin/kase");
        assert_eq!(quote_exec_arg("/a b/c"), "\"/a b/c\"");
        assert_eq!(quote_exec_arg("/a$b"), "\"/a\\$b\"");
        assert_eq!(quote_exec_arg("/100%"), "\"/100%%\"");
    }

    #[test]
    fn request_path_format() {
        assert_eq!(
            request_path(":1.42", "tok"),
            "/org/freedesktop/portal/desktop/request/1_42/tok"
        );
    }
}
